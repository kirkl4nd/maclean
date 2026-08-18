use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use cargo_toml::Manifest;
use walkdir::WalkDir;

use crate::core::{
    Item, Module, ModuleInfo, ModuleScan, ReclaimContext, ReclaimError, ReclaimResult, Relevance,
    Safety, ScanContext, ScheduleTarget, delete_contents, delete_tree, dir_size_in,
    exists_named_within, format_bytes, run_command, skip_walk_dir,
};

/// Minimum `target/` size to report. Tiny incremental leftovers aren't worth listing.
const MIN_TARGET_BYTES: u64 = 10 * 1000 * 1000;

/// Shared caches under ~/.cargo. All of them are re-fetched on demand.
const REGISTRY_DIRS: &[(&str, &str, &str)] = &[
    ("crates", "registry/cache", "Downloaded .crate archives"),
    (
        "sources",
        "registry/src",
        "Unpacked crate sources used while building",
    ),
    ("git-db", "git/db", "Bare clones of git dependencies"),
    (
        "git-checkouts",
        "git/checkouts",
        "Working copies of git dependencies",
    ),
];

pub struct CargoModule;

impl CargoModule {
    fn roots(ctx: &ScanContext) -> Vec<PathBuf> {
        ctx.roots_for("cargo")
    }

    fn cargo_home(ctx: &ScanContext) -> PathBuf {
        std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| ctx.path("cargo", "home", ".cargo"))
    }
}

impl Module for CargoModule {
    fn id(&self) -> &'static str {
        "cargo"
    }

    fn name(&self) -> &'static str {
        "Cargo"
    }

    fn description(&self) -> &'static str {
        "Cargo registry cache and project build output (`target/`)"
    }

    fn programs(&self) -> &'static [&'static str] {
        &["cargo"]
    }

    fn paths(&self) -> Vec<(&'static str, &'static str)> {
        vec![("home", ".cargo")]
    }

    fn searches(&self) -> bool {
        true
    }

    fn schedule_targets(&self) -> Vec<ScheduleTarget> {
        vec![
            ScheduleTarget::new(
                "cargo:projects",
                "Project build output",
                "cargo clean in every project found when the job runs",
            ),
            ScheduleTarget::new(
                "cargo:registry",
                "Registry and git cache",
                "Shared crates and git deps under ~/.cargo",
            ),
        ]
    }

    fn info(&self, ctx: &ScanContext) -> ModuleInfo {
        let mut info = ModuleInfo::new(self.id(), self.name(), self.description())
            .finds("The shared registry and git caches under ~/.cargo")
            .finds("`target/` directories next to every Cargo.toml under your home directory (and any extra search folders)")
            .finds("Workspaces (one shared target) and leftover targets inside members")
            .finds("Custom target directories set by .cargo/config.toml")
            .effect("Cleaning a project runs `cargo clean`, which empties target/ only")
            .effect("Your source, Cargo.toml and Cargo.lock are never touched")
            .effect("Cargo re-downloads registry files and rebuilds target/ on the next build")
            .location(Self::cargo_home(ctx));
        for root in Self::roots(ctx) {
            info = info.location(root);
        }
        info
    }

    fn relevance(&self, ctx: &ScanContext) -> Relevance {
        let cargo_home = Self::cargo_home(ctx);
        if cargo_home.join("registry").is_dir() {
            return Relevance::yes(format!("registry cache exists at {}", cargo_home.display()));
        }
        let roots = Self::roots(ctx);
        if exists_named_within(&roots, "Cargo.toml", 8) {
            Relevance::yes(
                "found at least one Cargo.toml under the search folders (toolchain not required)",
            )
        } else {
            Relevance::no("no Cargo.toml under the search folders (default is your home directory)")
        }
    }

    fn scan(&self, ctx: &ScanContext, relevance: Relevance) -> ModuleScan {
        let mut scan = ModuleScan::new(self.id(), self.name(), relevance);
        if let Some(registry) = registry_item(self.id(), &Self::cargo_home(ctx), ctx) {
            scan.items.push(registry);
        }
        let manifests = find_manifests(&Self::roots(ctx));
        if manifests.is_empty() {
            return scan;
        }

        let parsed: Vec<ParsedManifest> = manifests
            .iter()
            .filter_map(|path| parse_manifest(path).ok())
            .collect();

        let member_dirs = workspace_member_dirs(&parsed);
        let mut seen_targets = HashSet::new();
        let mut items = Vec::new();

        for manifest in &parsed {
            if ctx.cancelled() {
                break;
            }
            let is_member = member_dirs.contains(&manifest.dir);
            let owns_workspace_target = manifest.is_workspace || !is_member;
            let target = resolve_target_dir(&manifest.dir);

            if owns_workspace_target {
                push_target_item(
                    &mut items,
                    &mut seen_targets,
                    self.id(),
                    &manifest.dir,
                    &target,
                    manifest.is_workspace,
                    false,
                    ctx,
                );
            } else if target.is_dir() {
                push_target_item(
                    &mut items,
                    &mut seen_targets,
                    self.id(),
                    &manifest.dir,
                    &target,
                    false,
                    true,
                    ctx,
                );
            }
        }

        if !items.is_empty() {
            items.sort_by(|a, b| b.bytes.cmp(&a.bytes));
            let bytes: u64 = items.iter().map(|i| i.bytes).sum();
            let count = items.len();
            scan.items.push(
                Item::new(self.id(), "cargo:projects", "Project build output")
                    .with_summary(format!(
                        "cargo clean in {} — source, Cargo.toml and Cargo.lock stay",
                        crate::core::plural(count, "project")
                    ))
                    .with_bytes(bytes)
                    .with_safety(Safety::Safe)
                    .with_note("Each row runs `cargo clean` in that project. The project directory is not removed.")
                    .with_children(items),
            );
        }
        scan.items.sort_by(|a, b| b.bytes.cmp(&a.bytes));
        scan
    }

    fn reclaim(&self, item: &Item, ctx: &ReclaimContext) -> Result<ReclaimResult, ReclaimError> {
        let Some(target) = item.paths.first() else {
            return Err(ReclaimError::new(
                &item.id,
                crate::core::IssueKind::Warning,
                "cargo item has no path",
            ));
        };
        if !target.is_dir() {
            return Err(ReclaimError::new(
                &item.id,
                crate::core::IssueKind::Warning,
                format!("refusing to delete {} (not a directory)", target.display()),
            ));
        }

        if item.id.starts_with("cargo:registry:") {
            let freed = delete_contents(item, target, ctx)?;
            return Ok(ReclaimResult::ok(
                &item.id,
                freed,
                format!("emptied {} ({})", target.display(), format_bytes(freed)),
                ctx.dry_run,
            ));
        }

        if ctx.dry_run {
            return Ok(ReclaimResult::ok(
                &item.id,
                item.bytes,
                format!(
                    "would run cargo clean in {} ({})",
                    target.parent().unwrap_or(target).display(),
                    format_bytes(item.bytes)
                ),
                true,
            ));
        }

        let project = item
            .id
            .strip_prefix("cargo:")
            .map(PathBuf::from)
            .unwrap_or_else(|| target.parent().unwrap_or(target).to_path_buf());
        let manifest = project.join("Cargo.toml");
        let manifest_s = manifest.to_string_lossy();
        if run_command(
            item,
            "cargo",
            &["clean", "--manifest-path", manifest_s.as_ref()],
            ctx,
        )
        .is_ok()
            && !target.exists()
        {
            return Ok(ReclaimResult::ok(
                &item.id,
                item.bytes,
                format!(
                    "cargo clean in {} ({})",
                    project.display(),
                    format_bytes(item.bytes)
                ),
                false,
            ));
        }

        let freed = delete_tree(item, target, ctx)?;
        Ok(ReclaimResult::ok(
            &item.id,
            freed,
            format!("emptied {} ({})", target.display(), format_bytes(freed)),
            false,
        ))
    }
}

struct ParsedManifest {
    dir: PathBuf,
    is_workspace: bool,
    member_patterns: Vec<String>,
    exclude_patterns: Vec<String>,
    workspace_ptr: Option<PathBuf>,
}

fn find_manifests(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in WalkDir::new(root)
            .follow_links(false)
            .max_depth(8)
            .into_iter()
            .filter_entry(|e| {
                if !e.file_type().is_dir() {
                    return true;
                }
                !skip_walk_dir(&e.file_name().to_string_lossy())
            })
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() && entry.file_name() == "Cargo.toml" {
                found.push(entry.path().to_path_buf());
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

fn parse_manifest(cargo_toml: &Path) -> Result<ParsedManifest> {
    let bytes = fs::read(cargo_toml)?;
    let manifest = Manifest::from_slice(&bytes)?;
    let dir = cargo_toml
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let (is_workspace, member_patterns, exclude_patterns) = match &manifest.workspace {
        Some(ws) => (true, ws.members.clone(), ws.exclude.clone()),
        None => (false, Vec::new(), Vec::new()),
    };
    let workspace_ptr = manifest
        .package
        .as_ref()
        .and_then(|p| p.workspace.as_ref())
        .map(|rel| dir.join(rel));
    Ok(ParsedManifest {
        dir,
        is_workspace,
        member_patterns,
        exclude_patterns,
        workspace_ptr,
    })
}

fn workspace_member_dirs(parsed: &[ParsedManifest]) -> HashSet<PathBuf> {
    let mut members = HashSet::new();
    for manifest in parsed {
        if let Some(ptr) = &manifest.workspace_ptr {
            members.insert(normalize(ptr));
        }
        if !manifest.is_workspace {
            continue;
        }
        let expanded = expand_globs(&manifest.dir, &manifest.member_patterns);
        let excluded = expand_globs(&manifest.dir, &manifest.exclude_patterns);
        for path in expanded {
            if excluded.contains(&path) {
                continue;
            }
            members.insert(path);
        }
    }
    members
}

fn expand_globs(root: &Path, patterns: &[String]) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    for pattern in patterns {
        let joined = root.join(pattern);
        let glob_pat = joined.to_string_lossy();
        if let Ok(entries) = glob::glob(&glob_pat) {
            for entry in entries.flatten() {
                let dir = if entry.is_dir() {
                    entry
                } else {
                    continue;
                };
                if dir.join("Cargo.toml").is_file() {
                    out.insert(normalize(&dir));
                }
            }
        }
        // Non-glob exact member path.
        let exact = root.join(pattern);
        if exact.join("Cargo.toml").is_file() {
            out.insert(normalize(&exact));
        }
    }
    out
}

fn normalize(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(serde::Deserialize)]
struct CargoConfig {
    build: Option<CargoBuild>,
}

#[derive(serde::Deserialize)]
struct CargoBuild {
    #[serde(rename = "target-dir")]
    target_dir: Option<String>,
}

fn resolve_target_dir(project: &Path) -> PathBuf {
    for name in [".cargo/config.toml", ".cargo/config"] {
        let cfg = project.join(name);
        let Ok(text) = fs::read_to_string(&cfg) else {
            continue;
        };
        let Ok(parsed) = toml::from_str::<CargoConfig>(&text) else {
            continue;
        };
        if let Some(dir) = parsed.build.and_then(|b| b.target_dir) {
            let path = PathBuf::from(dir);
            return if path.is_absolute() {
                path
            } else {
                project.join(path)
            };
        }
    }
    project.join("target")
}

fn push_target_item(
    items: &mut Vec<Item>,
    seen: &mut HashSet<PathBuf>,
    module: &str,
    project: &Path,
    target: &Path,
    workspace: bool,
    leftover_member: bool,
    ctx: &ScanContext,
) {
    let key = normalize(target);
    if !seen.insert(key) {
        return;
    }
    if !target.is_dir() {
        return;
    }
    let bytes = dir_size_in(target, ctx).bytes;
    if bytes < MIN_TARGET_BYTES {
        return;
    }
    let name = project
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    let title = if workspace {
        format!("{name} — build output (workspace)")
    } else {
        format!("{name} — build output")
    };
    let summary = if leftover_member {
        format!(
            "Stale target/ inside a workspace member — the shared one lives at the workspace root ({})",
            target.display()
        )
    } else {
        format!("cargo clean in {}", project.display())
    };
    let mut profiles = Vec::new();
    if let Ok(entries) = fs::read_dir(target) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() || ctx.cancelled() {
                continue;
            }
            let sub = entry.path();
            let sub_bytes = dir_size_in(&sub, ctx).bytes;
            if sub_bytes == 0 {
                continue;
            }
            let profile = entry.file_name().to_string_lossy().into_owned();
            profiles.push(
                Item::new(
                    module,
                    format!("cargo:{}:{profile}", project.display()),
                    profile,
                )
                .with_summary(sub.display().to_string())
                .with_bytes(sub_bytes)
                .with_safety(Safety::Safe),
            );
        }
    }

    items.push(
        Item::new(module, format!("cargo:{}", project.display()), title)
            .with_summary(summary)
            .with_bytes(bytes)
            .with_path(target.to_path_buf())
            .with_safety(Safety::Safe)
            .with_reclaimable(true)
            .clean_whole()
            .with_detail("Project", project.display().to_string())
            .with_detail("Runs", "cargo clean")
            .with_detail("Empties", target.display().to_string())
            .with_detail(
                "Kind",
                if workspace {
                    "workspace root"
                } else if leftover_member {
                    "workspace member (stale target)"
                } else {
                    "standalone crate"
                },
            )
            .with_note(
                "The project is not removed. Only target/ is emptied — source, Cargo.toml and Cargo.lock stay.",
            )
            .with_note("The next `cargo build` compiles from scratch.")
            .with_children(profiles)
            .prune_children(1_000_000, 8),
    );
}

/// The shared caches under ~/.cargo, one row per kind so you can pick.
fn registry_item(module: &str, cargo_home: &Path, ctx: &ScanContext) -> Option<Item> {
    let mut children = Vec::new();
    for (key, rel, what) in REGISTRY_DIRS {
        let path = cargo_home.join(rel);
        if !path.is_dir() {
            continue;
        }
        let bytes = dir_size_in(&path, ctx).bytes;
        if bytes == 0 {
            continue;
        }
        children.push(
            Item::new(module, format!("cargo:registry:{key}"), rel.to_string())
                .with_summary(what.to_string())
                .with_bytes(bytes)
                .with_path(path.clone())
                .with_safety(Safety::Safe)
                .with_reclaimable(true)
                .clean_whole()
                .with_detail("Path", path.display().to_string())
                .with_note("Cargo fetches this again the next time a build needs it."),
        );
    }
    if children.is_empty() {
        return None;
    }
    let bytes = children.iter().map(|c| c.bytes).sum();
    Some(
        Item::new(module, "cargo:registry", "Registry and git cache")
            .with_summary(format!("Shared download cache in {}", cargo_home.display()))
            .with_bytes(bytes)
            .with_safety(Safety::Safe)
            .with_note("Shared by every Rust project on this Mac. No project is modified.")
            .with_children(children),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_member_paths_expand() {
        let tmp = std::env::temp_dir().join(format!("maclean-cargo-test-{}", std::process::id()));
        let member = tmp.join("crates/foo");
        fs::create_dir_all(&member).unwrap();
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname=\"foo\"\nversion=\"0.1.0\"\n",
        )
        .unwrap();
        let expanded = expand_globs(&tmp, &["crates/foo".into()]);
        assert!(expanded.contains(&normalize(&member)));
        let _ = fs::remove_dir_all(&tmp);
    }
}
