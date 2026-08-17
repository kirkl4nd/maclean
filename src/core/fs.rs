use std::path::Path;

use walkdir::WalkDir;

/// Directory names we never descend into while hunting for project files.
/// This is a walk-cost list (huge or cyclic trees), not a catalogue of modules.
pub const SKIP_WALK_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "Library",
    ".npm",
    ".cache",
    ".local",
    ".rustup",
    ".cargo",
];

pub fn skip_walk_dir(name: &str) -> bool {
    SKIP_WALK_DIRS.iter().any(|s| *s == name)
}

/// True if `file_name` exists under any root, without measuring sizes.
/// Stops at the first hit. `max_depth` is WalkDir depth (root = 0).
pub fn exists_named_within(roots: &[impl AsRef<Path>], file_name: &str, max_depth: usize) -> bool {
    for root in roots {
        let root = root.as_ref();
        if !root.is_dir() {
            continue;
        }
        if root.join(file_name).is_file() || root.join(file_name).is_dir() {
            return true;
        }
        for entry in WalkDir::new(root)
            .follow_links(false)
            .max_depth(max_depth)
            .into_iter()
            .filter_entry(|e| {
                if !e.file_type().is_dir() {
                    return true;
                }
                let name = e.file_name().to_string_lossy();
                if name == file_name {
                    return true;
                }
                !skip_walk_dir(&name)
            })
            .filter_map(|e| e.ok())
        {
            if entry.file_name() == file_name {
                return true;
            }
        }
    }
    false
}

/// Sum apparent file sizes under `path`. Permission errors are recorded, not printed.
pub fn dir_size(path: &Path) -> u64 {
    walk_size(path, None).bytes
}

pub struct SizeReport {
    pub bytes: u64,
    pub issues: Vec<crate::core::ScanIssue>,
}

/// Same as [`dir_size`], but reports what it could not read and abandons the
/// walk when the scan is cancelled. Walks under ~/Library can take minutes;
/// this keeps quit and rescan instant.
pub fn dir_size_in(path: &Path, ctx: &crate::core::ScanContext) -> SizeReport {
    walk_size(path, Some(ctx))
}

fn walk_size(path: &Path, ctx: Option<&crate::core::ScanContext>) -> SizeReport {
    let mut bytes = 0u64;
    let mut issues = Vec::new();
    if !path.exists() {
        return SizeReport { bytes: 0, issues };
    }
    let mut counter: u32 = 0;
    for entry in WalkDir::new(path).follow_links(false).into_iter() {
        counter = counter.wrapping_add(1);
        if counter % 2048 == 0 {
            if let Some(ctx) = ctx {
                if ctx.cancelled() {
                    return SizeReport { bytes, issues };
                }
            }
        }
        match entry {
            Ok(e) if e.file_type().is_file() => {
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
            Ok(_) => {}
            Err(err) => {
                if issues.len() >= 8 {
                    continue;
                }
                let p = err
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| path.to_path_buf());
                issues.push(crate::core::ScanIssue::permission(p, err.to_string()));
            }
        }
    }
    SizeReport { bytes, issues }
}
