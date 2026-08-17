//! The only place maclean deletes files or starts processes.
//!
//! Modules describe *what* to do. This module decides whether it is allowed,
//! and is the only code that talks to the OS. A future module cannot
//! `rm -rf /` or wrap itself in sudo: it has to come through here, with a
//! declared path or a program it declared on [`crate::core::Module`].

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::issue::{IssueKind, Privilege, ReclaimError};
use super::item::Item;
use super::module::ReclaimContext;

/// Interpreters, shells, and generic destructive tools. A module cannot
/// declare these even if it wants to: the core's job is the sandbox, not
/// the catalogue of feature tools.
const DENIED_PROGRAMS: &[&str] = &[
    "bash",
    "sh",
    "zsh",
    "fish",
    "dash",
    "csh",
    "tcsh",
    "ksh",
    "sudo",
    "su",
    "doas",
    "rm",
    "rmdir",
    "dd",
    "osascript",
    "python",
    "python3",
    "perl",
    "ruby",
    "php",
    "node",
];

const DENIED_EXACT: &[&str] = &[
    "/",
    "/System",
    "/usr",
    "/bin",
    "/sbin",
    "/etc",
    "/private",
    "/Applications",
    "/Library",
    "/Users",
    "/opt",
    "/var",
    "/tmp",
    "/Volumes",
];

/// Block size POSIX uses for `st_blocks`.
const BLOCK_BYTES: u64 = 512;

pub fn running_as_root() -> bool {
    unsafe { geteuid() == 0 }
}

unsafe extern "C" {
    fn geteuid() -> u32;
}

/// Bytes actually occupying disk. For a sparse file this is much smaller
/// than `metadata.len()` (the figure Finder often shows).
pub fn allocated_bytes(path: &Path) -> u64 {
    let Ok(meta) = fs::metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.blocks() * BLOCK_BYTES;
    }
    0
}

pub fn run(
    item: &Item,
    program: &str,
    args: &[&str],
    ctx: &ReclaimContext,
) -> Result<Output, ReclaimError> {
    deny_root_process(&item.id)?;
    if let Err(err) = program_permitted(program, &ctx.allowed_programs) {
        return Err(ReclaimError::new(&item.id, IssueKind::Warning, err));
    }

    let elevate = item.privilege == Privilege::Admin && ctx.allow_admin;
    if item.privilege == Privilege::Admin && !ctx.allow_admin && !ctx.dry_run {
        return Err(ReclaimError::from_issue(
            &item.id,
            super::issue::ScanIssue::needs_admin(format!(
                "'{}' needs a one-shot administrator password",
                item.title
            )),
        ));
    }

    let mut cmd = if elevate {
        let mut c = Command::new("sudo");
        c.arg("--").arg(program).args(args);
        c.stdin(Stdio::inherit());
        c
    } else {
        let mut c = Command::new(program);
        c.args(args);
        c.stdin(Stdio::null());
        c
    };
    apply_path(&mut cmd, &ctx.path_dirs);
    let output = timed_output(cmd, Duration::from_secs(180))
        .map_err(|err| ReclaimError::new(&item.id, IssueKind::Unavailable, err))?;

    if output.status.success() {
        return Ok(output);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("{program} exited {}", output.status)
    };

    if elevate && output.status.code() == Some(1) && detail.to_lowercase().contains("password") {
        return Err(ReclaimError::new(
            &item.id,
            IssueKind::NeedsAdmin,
            "sudo could not read a password (not a TTY, or the timestamp expired)",
        )
        .with_hint("Confirm this action in the interactive UI so maclean can ask once, for this command only."));
    }

    Err(ReclaimError::new(&item.id, IssueKind::Warning, detail))
}

/// Scan-time helper: run a module-declared program, never with sudo.
pub fn run_scan(
    program: &str,
    args: &[&str],
    timeout: Duration,
    ctx: &super::module::ScanContext,
) -> Result<Output, String> {
    run_scan_with(
        program,
        args,
        timeout,
        &ctx.allowed_programs,
        &ctx.path_dirs,
    )
}

pub fn run_scan_with(
    program: &str,
    args: &[&str],
    timeout: Duration,
    allowed_programs: &[&str],
    path_dirs: &[&str],
) -> Result<Output, String> {
    program_permitted(program, allowed_programs)?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    apply_path(&mut cmd, path_dirs);
    cmd.stdin(Stdio::null());
    timed_output(cmd, timeout)
}

pub fn delete_tree(item: &Item, path: &Path, ctx: &ReclaimContext) -> Result<u64, ReclaimError> {
    delete(item, path, false, ctx)
}

pub fn delete_contents(
    item: &Item,
    path: &Path,
    ctx: &ReclaimContext,
) -> Result<u64, ReclaimError> {
    delete(item, path, true, ctx)
}

fn delete(
    item: &Item,
    path: &Path,
    contents_only: bool,
    ctx: &ReclaimContext,
) -> Result<u64, ReclaimError> {
    deny_root_process(&item.id)?;
    if item.privilege == Privilege::Admin {
        return Err(ReclaimError::new(
            &item.id,
            IssueKind::Warning,
            "this action is marked as admin and cannot delete files directly",
        ));
    }
    let allowed = check_path(item, path, &ctx.allowed_roots)?;
    if ctx.dry_run {
        return Ok(dir_or_file_size(&allowed));
    }
    let size = dir_or_file_size(&allowed);
    if contents_only {
        if !allowed.is_dir() {
            return Err(ReclaimError::new(
                &item.id,
                IssueKind::Warning,
                format!("{} is not a directory", allowed.display()),
            ));
        }
        let entries = fs::read_dir(&allowed)
            .map_err(|err| ReclaimError::new(&item.id, IssueKind::Permission, err.to_string()))?;
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_dir() {
                fs::remove_dir_all(&child).map_err(|err| {
                    ReclaimError::new(&item.id, IssueKind::Permission, err.to_string())
                })?;
            } else {
                fs::remove_file(&child).map_err(|err| {
                    ReclaimError::new(&item.id, IssueKind::Permission, err.to_string())
                })?;
            }
        }
    } else if allowed.is_dir() {
        fs::remove_dir_all(&allowed)
            .map_err(|err| ReclaimError::new(&item.id, IssueKind::Permission, err.to_string()))?;
    } else {
        fs::remove_file(&allowed)
            .map_err(|err| ReclaimError::new(&item.id, IssueKind::Permission, err.to_string()))?;
    }
    Ok(size)
}

fn check_path(item: &Item, path: &Path, roots: &[PathBuf]) -> Result<PathBuf, ReclaimError> {
    let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if DENIED_EXACT.iter().any(|p| Path::new(p) == canon) {
        return Err(ReclaimError::new(
            &item.id,
            IssueKind::Warning,
            format!("refusing to touch {}", canon.display()),
        ));
    }
    let under_root = roots.iter().any(|r| {
        let root = fs::canonicalize(r).unwrap_or_else(|_| r.clone());
        canon == root || canon.starts_with(&root)
    });
    if !under_root {
        return Err(ReclaimError::new(
            &item.id,
            IssueKind::Warning,
            format!("{} is outside the allowed roots", canon.display()),
        ));
    }
    // Must be the scanned path, or inside it. A module cannot delete a sibling
    // it never reported.
    if item.paths.is_empty() {
        return Err(ReclaimError::new(
            &item.id,
            IssueKind::Warning,
            "refusing to delete; this item declared no paths at scan time",
        ));
    }
    let declared = item.paths.iter().any(|p| {
        let declared = fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        canon == declared || canon.starts_with(&declared)
    });
    if !declared {
        return Err(ReclaimError::new(
            &item.id,
            IssueKind::Warning,
            format!(
                "{} was not a path this item declared at scan time",
                canon.display()
            ),
        ));
    }
    if roots.first().is_some_and(|home| {
        fs::canonicalize(home)
            .ok()
            .is_some_and(|h| canon == h || canon == h.join("Library"))
    }) {
        return Err(ReclaimError::new(
            &item.id,
            IssueKind::Warning,
            format!("refusing to delete {}", canon.display()),
        ));
    }
    Ok(canon)
}

fn dir_or_file_size(path: &Path) -> u64 {
    if path.is_file() {
        return allocated_bytes(path).max(path.metadata().map(|m| m.len()).unwrap_or(0));
    }
    super::fs::dir_size(path)
}

fn deny_root_process(item_id: &str) -> Result<(), ReclaimError> {
    if running_as_root() {
        return Err(ReclaimError::new(
            item_id,
            IssueKind::Warning,
            "maclean is running as root; refusing to continue",
        )
        .with_hint("Quit and run maclean as yourself. Individual actions ask for a password if they need one."));
    }
    Ok(())
}

fn apply_path(cmd: &mut Command, extras: &[&str]) {
    let mut path = std::env::var("PATH").unwrap_or_default();
    // Standard macOS locations, then whatever a module declared.
    let generic = ["/usr/local/bin", "/opt/homebrew/bin"];
    for extra in generic.iter().copied().chain(extras.iter().copied()) {
        if !path.split(':').any(|p| p == extra) {
            path = format!("{extra}:{path}");
        }
    }
    cmd.env("PATH", path);
}

fn program_permitted(program: &str, allowed: &[&str]) -> Result<(), String> {
    if program.contains('/') || program.contains('\\') {
        return Err("refusing to run a path; pass a program name".into());
    }
    if DENIED_PROGRAMS.contains(&program) {
        return Err(format!("refusing to run '{program}'"));
    }
    if !allowed.contains(&program) {
        return Err(format!(
            "refusing to run '{program}' — not declared by any module"
        ));
    }
    Ok(())
}

fn timed_output(mut cmd: Command, timeout: Duration) -> Result<Output, String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(cmd.output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err(format!("timed out after {}s", timeout.as_secs())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shells_are_denied_even_if_a_module_declares_them() {
        assert!(program_permitted("bash", &["bash"]).is_err());
        assert!(program_permitted("sh", &["sh"]).is_err());
        assert!(program_permitted("osascript", &["osascript"]).is_err());
        assert!(program_permitted("/bin/rm", &["/bin/rm"]).is_err());
    }

    #[test]
    fn undeclared_programs_are_rejected() {
        assert!(program_permitted("tool", &[]).is_err());
        assert!(program_permitted("tool", &["tool"]).is_ok());
        assert!(program_permitted("other", &["tool"]).is_err());
    }

    fn dry_ctx(root: &Path) -> ReclaimContext {
        ReclaimContext {
            dry_run: true,
            allow_admin: false,
            allowed_roots: vec![root.to_path_buf()],
            allowed_programs: Vec::new(),
            path_dirs: Vec::new(),
        }
    }

    #[test]
    fn delete_refuses_a_parent_of_the_declared_path() {
        let root = std::env::temp_dir().join(format!("maclean-del-{}", std::process::id()));
        let child = root.join("proj/node_modules");
        fs::create_dir_all(&child).unwrap();
        let item = Item::new("t", "node:modules:proj", "proj")
            .with_reclaimable(true)
            .with_path(child);
        let ctx = dry_ctx(&root);
        assert!(delete_tree(&item, &root, &ctx).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_refuses_an_item_with_no_declared_paths() {
        let root = std::env::temp_dir().join(format!("maclean-nopath-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let item = Item::new("t", "x", "x").with_reclaimable(true);
        let ctx = dry_ctx(&root);
        assert!(delete_tree(&item, &root, &ctx).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
