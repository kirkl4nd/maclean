use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::core::{
    IssueKind, Item, Module, ModuleInfo, ModuleScan, ReclaimContext, ReclaimError, ReclaimResult,
    Relevance, Safety, ScanContext, ScheduleTarget, delete_contents, delete_tree, dir_size,
    dir_size_in, exists_named_within, format_bytes, run_command, skip_walk_dir,
};

/// A `node_modules` smaller than this is not worth a row in the tree.
const MIN_MODULES_BYTES: u64 = 100 * 1000 * 1000;

/// One cache directory belonging to one package manager. Managers keep more
/// than one (npm's own cache and npx's are separate, and `npm cache clean`
/// does not touch npx), so every directory gets its own row.
struct Cache {
    key: &'static str,
    name: &'static str,
    path: &'static str,
    /// The manager's own cleanup command, preferred over deleting files.
    command: Option<&'static [&'static str]>,
    safety: Safety,
    note: &'static str,
}

const CACHES: &[Cache] = &[
    Cache {
        key: "npm",
        name: "npm cache",
        path: ".npm/_cacache",
        command: Some(&["npm", "cache", "clean", "--force"]),
        safety: Safety::Safe,
        note: "Downloaded tarballs only. The next install re-downloads what it needs.",
    },
    Cache {
        key: "npx",
        name: "npx cache",
        path: ".npm/_npx",
        command: None,
        safety: Safety::Safe,
        note: "One-off packages run through npx. They are fetched again if you use them again.",
    },
    Cache {
        key: "yarn",
        name: "Yarn cache (classic)",
        path: "Library/Caches/Yarn",
        command: Some(&["yarn", "cache", "clean"]),
        safety: Safety::Safe,
        note: "Yarn refills this on the next install.",
    },
    Cache {
        key: "yarn-berry",
        name: "Yarn cache (berry)",
        path: ".yarn/berry/cache",
        command: None,
        safety: Safety::Safe,
        note: "Yarn 2+ global cache. Projects using their own .yarn/cache are unaffected.",
    },
    Cache {
        key: "pnpm-lib",
        name: "pnpm store",
        path: "Library/pnpm/store",
        command: Some(&["pnpm", "store", "prune"]),
        safety: Safety::Caution,
        note: "pnpm hard-links this store into projects, so only unreferenced packages are pruned.",
    },
    Cache {
        key: "pnpm-home",
        name: "pnpm store",
        path: ".pnpm-store",
        command: Some(&["pnpm", "store", "prune"]),
        safety: Safety::Caution,
        note: "pnpm hard-links this store into projects, so only unreferenced packages are pruned.",
    },
    Cache {
        key: "pnpm-share",
        name: "pnpm store",
        path: ".local/share/pnpm/store",
        command: Some(&["pnpm", "store", "prune"]),
        safety: Safety::Caution,
        note: "pnpm hard-links this store into projects, so only unreferenced packages are pruned.",
    },
    Cache {
        key: "bun",
        name: "Bun cache",
        path: ".bun/install/cache",
        command: Some(&["bun", "pm", "cache", "rm"]),
        safety: Safety::Safe,
        note: "Bun refills this on the next install.",
    },
];

pub struct NodeModule;

impl NodeModule {
    fn roots(ctx: &ScanContext) -> Vec<PathBuf> {
        ctx.roots_for("node")
    }

    fn cache_path(ctx: &ScanContext, cache: &Cache) -> Option<PathBuf> {
        let path = ctx.path("node", cache.key, cache.path);
        path.is_dir().then_some(path)
    }
}

/// Walk each search root only. Do not follow symlinks: a Wine `z:` drive
/// is a link to `/`, and `Path::is_dir` would happily walk `/opt` from `~`.
fn find_node_modules(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in WalkDir::new(root)
            .follow_links(false)
            .max_depth(6)
            .into_iter()
            .filter_entry(|e| {
                if e.path()
                    .parent()
                    .and_then(|p| p.file_name())
                    .is_some_and(|n| n == "node_modules")
                {
                    return false;
                }
                if !e.file_type().is_dir() {
                    return true;
                }
                let name = e.file_name().to_string_lossy();
                if name == "node_modules" {
                    return true;
                }
                !skip_walk_dir(&name)
            })
        {
            match entry {
                Ok(e) if e.file_type().is_dir() && e.file_name() == "node_modules" => {
                    found.push(e.path().to_path_buf());
                }
                _ => {}
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Which package manager a project uses, read from its lockfile.
fn manager_of(project: &Path) -> (&'static str, &'static str) {
    for (file, name, install) in [
        ("pnpm-lock.yaml", "pnpm", "pnpm install"),
        ("yarn.lock", "Yarn", "yarn install"),
        ("bun.lockb", "Bun", "bun install"),
        ("bun.lock", "Bun", "bun install"),
        ("package-lock.json", "npm", "npm install"),
    ] {
        if project.join(file).is_file() {
            return (name, install);
        }
    }
    ("npm", "npm install")
}

impl Module for NodeModule {
    fn id(&self) -> &'static str {
        "node"
    }
    fn name(&self) -> &'static str {
        "Node.js"
    }
    fn description(&self) -> &'static str {
        "Package manager caches (npm, Yarn, pnpm, Bun) and large node_modules folders"
    }

    fn programs(&self) -> &'static [&'static str] {
        &["npm", "npx", "yarn", "pnpm", "bun"]
    }

    fn paths(&self) -> Vec<(&'static str, &'static str)> {
        CACHES.iter().map(|c| (c.key, c.path)).collect()
    }

    fn searches(&self) -> bool {
        true
    }

    fn schedule_targets(&self) -> Vec<ScheduleTarget> {
        vec![
            ScheduleTarget::new(
                "node:caches",
                "Package manager caches",
                "npm, Yarn, pnpm, Bun download caches found when the job runs",
            ),
            ScheduleTarget::new(
                "node:modules",
                "Project node_modules",
                "Large node_modules folders found when the job runs",
            ),
        ]
    }

    fn info(&self, ctx: &ScanContext) -> ModuleInfo {
        let mut info = ModuleInfo::new(self.id(), self.name(), self.description())
            .finds("The download cache of every package manager you have used")
            .finds(format!(
                "node_modules folders over {} under your home directory (and any extra search folders)",
                format_bytes(MIN_MODULES_BYTES)
            ))
            .effect("Caches are refilled automatically by the next install")
            .effect(
                "Cleaning a project removes only its node_modules folder — source, package.json and lockfile stay",
            )
            .effect("Each manager's own cleanup command is used when that manager is installed");
        for cache in CACHES {
            if let Some(path) = Self::cache_path(ctx, cache) {
                info = info.location(path);
            }
        }
        for root in Self::roots(ctx) {
            info = info.location(root);
        }
        info
    }

    fn relevance(&self, ctx: &ScanContext) -> Relevance {
        let mut present: Vec<&str> = CACHES
            .iter()
            .filter(|c| Self::cache_path(ctx, c).is_some())
            .map(|c| c.name)
            .collect();
        present.dedup();
        if !present.is_empty() {
            return Relevance::yes(format!("found {}", present.join(", ")));
        }
        let roots = Self::roots(ctx);
        if exists_named_within(&roots, "package.json", 6)
            || exists_named_within(&roots, "node_modules", 6)
        {
            return Relevance::yes(
                "found package.json or node_modules under the search folders (no package manager needed)",
            );
        }
        Relevance::no(
            "no package manager cache and no package.json under the search folders (default is your home directory)",
        )
    }

    fn scan(&self, ctx: &ScanContext, relevance: Relevance) -> ModuleScan {
        let mut scan = ModuleScan::new(self.id(), self.name(), relevance);

        let mut caches = Vec::new();
        for cache in CACHES {
            let Some(path) = Self::cache_path(ctx, cache) else {
                continue;
            };
            let report = dir_size_in(&path, ctx);
            scan.issues.extend(report.issues);
            if report.bytes == 0 {
                continue;
            }
            let method = match cache.command {
                Some(argv) => argv.join(" "),
                None => "delete cached files".into(),
            };
            caches.push(
                Item::new(
                    self.id(),
                    format!("node:cache:{}", cache.key),
                    cache.name.to_string(),
                )
                .with_summary(path.display().to_string())
                .with_bytes(report.bytes)
                .with_path(path)
                .with_safety(cache.safety)
                .with_reclaimable(true)
                .clean_whole()
                .with_detail("Runs", method)
                .with_note(cache.note),
            );
        }
        if !caches.is_empty() {
            caches.sort_by(|a, b| b.bytes.cmp(&a.bytes));
            let bytes: u64 = caches.iter().map(|c| c.bytes).sum();
            scan.items.push(
                Item::new(self.id(), "node:caches", "Package manager caches")
                    .with_summary("Downloaded packages kept outside your projects")
                    .with_bytes(bytes)
                    .with_safety(Safety::Safe)
                    .with_note("No project is touched: these are shared download caches.")
                    .with_children(caches),
            );
        }

        let mut projects = Vec::new();
        for dir in find_node_modules(&Self::roots(ctx)) {
            if ctx.cancelled() {
                return scan;
            }
            let report = dir_size_in(&dir, ctx);
            scan.issues.extend(report.issues);
            if report.bytes < MIN_MODULES_BYTES {
                continue;
            }
            let project = dir.parent().unwrap_or(&dir).to_path_buf();
            let name = project
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("project")
                .to_string();
            let (manager, install) = manager_of(&project);
            projects.push(
                Item::new(
                    self.id(),
                    format!("node:modules:{}", project.display()),
                    format!("{name} — node_modules"),
                )
                .with_summary(format!(
                    "Installed dependencies of {}",
                    project.display()
                ))
                .with_bytes(report.bytes)
                .with_path(dir.clone())
                .with_safety(Safety::Caution)
                .with_reclaimable(true)
                .clean_whole()
                .with_detail("Project", project.display().to_string())
                .with_detail("Manager", manager)
                .with_detail("Restore with", install)
                .with_note(format!(
                    "Deletes the node_modules folder only. Source, package.json and the lockfile stay; run `{install}` to get it back."
                )),
            );
        }
        if !projects.is_empty() {
            projects.sort_by(|a, b| b.bytes.cmp(&a.bytes));
            let bytes: u64 = projects.iter().map(|p| p.bytes).sum();
            let count = projects.len();
            scan.items.push(
                Item::new(self.id(), "node:modules", "Project dependencies")
                    .with_summary(format!(
                        "{} with a large node_modules",
                        crate::core::plural(count, "project")
                    ))
                    .with_bytes(bytes)
                    .with_safety(Safety::Caution)
                    .with_detail("Projects", count.to_string())
                    .with_note(
                        "Only the node_modules folder is deleted — the project itself is never removed.",
                    )
                    .with_children(projects),
            );
        }

        scan
    }

    fn reclaim(&self, item: &Item, ctx: &ReclaimContext) -> Result<ReclaimResult, ReclaimError> {
        let Some(path) = item.paths.first().cloned() else {
            return Err(ReclaimError::new(
                &item.id,
                IssueKind::Warning,
                format!("nothing to clean for '{}'", item.id),
            ));
        };

        if let Some(key) = item.id.strip_prefix("node:cache:") {
            let cache = CACHES
                .iter()
                .find(|c| c.key == key)
                .ok_or_else(|| ReclaimError::new(&item.id, IssueKind::Warning, "unknown cache"))?;
            if ctx.dry_run {
                return Ok(ReclaimResult::ok(
                    &item.id,
                    item.bytes,
                    format!(
                        "would clear the {} ({})",
                        cache.name,
                        format_bytes(item.bytes)
                    ),
                    true,
                ));
            }
            return clear_cache(item, cache, &path, ctx);
        }

        if !item.id.starts_with("node:modules:") {
            return Err(ReclaimError::new(
                &item.id,
                IssueKind::Warning,
                format!("the Node.js module does not clean '{}'", item.id),
            ));
        }
        if path.file_name().and_then(|s| s.to_str()) != Some("node_modules") {
            return Err(ReclaimError::new(
                &item.id,
                IssueKind::Warning,
                format!(
                    "refusing to delete {} — it is not node_modules",
                    path.display()
                ),
            ));
        }
        let bytes = delete_tree(item, &path, ctx)?;
        Ok(ReclaimResult::ok(
            &item.id,
            bytes,
            format!("removed node_modules ({})", format_bytes(bytes)),
            ctx.dry_run,
        ))
    }
}

/// Prefer the package manager's own cleanup, fall back to deleting the files.
fn clear_cache(
    item: &Item,
    cache: &Cache,
    path: &Path,
    ctx: &ReclaimContext,
) -> Result<ReclaimResult, ReclaimError> {
    if let Some(argv) = cache.command {
        if ctx.dry_run {
            return Ok(ReclaimResult::ok(
                &item.id,
                item.bytes,
                format!(
                    "would run {} ({})",
                    argv.join(" "),
                    format_bytes(item.bytes)
                ),
                true,
            ));
        }
        if run_command(item, argv[0], &argv[1..], ctx).is_ok() {
            let freed = item.bytes.saturating_sub(dir_size(path));
            return Ok(ReclaimResult::ok(
                &item.id,
                freed,
                format!("{} ({})", argv.join(" "), format_bytes(freed)),
                false,
            ));
        }
    }
    if cache.key.starts_with("pnpm") {
        return Err(ReclaimError::new(
            &item.id,
            IssueKind::Unavailable,
            "pnpm is not installed, so the store cannot be pruned safely",
        )
        .with_hint("Install pnpm and run `pnpm store prune`, or leave this alone."));
    }
    let freed = delete_contents(item, path, ctx)?;
    Ok(ReclaimResult::ok(
        &item.id,
        freed,
        format!("cleared the {} ({})", cache.name, format_bytes(freed)),
        ctx.dry_run,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch_dir(path: &Path) {
        fs::create_dir_all(path).unwrap();
    }

    #[test]
    fn finds_node_modules_under_the_search_root() {
        let root = std::env::temp_dir().join(format!("maclean-nm-ok-{}", std::process::id()));
        let modules = root.join("proj/node_modules");
        touch_dir(&modules);
        let found = find_node_modules(&[root.clone()]);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(found, vec![modules]);
    }

    #[test]
    fn does_not_follow_a_symlink_out_of_the_search_root() {
        let tmp = std::env::temp_dir().join(format!("maclean-nm-link-{}", std::process::id()));
        let root = tmp.join("home");
        let outside = tmp.join("opt/homebrew/lib/node_modules");
        touch_dir(&outside);
        touch_dir(&root);
        std::os::unix::fs::symlink(tmp.join("opt"), root.join("opt-link")).unwrap();
        let found = find_node_modules(&[root]);
        let _ = fs::remove_dir_all(&tmp);
        assert!(
            found.is_empty(),
            "followed a symlink out of the root: {found:?}"
        );
    }

    #[test]
    fn wine_z_drive_does_not_escape_to_root() {
        let tmp = std::env::temp_dir().join(format!("maclean-nm-wine-{}", std::process::id()));
        let home = tmp.join("home");
        let outside = tmp.join("opt/homebrew/lib/node_modules");
        touch_dir(&outside);
        let dos = home.join(".wine/dosdevices");
        touch_dir(&dos);
        std::os::unix::fs::symlink("/", dos.join("z:")).unwrap();
        std::os::unix::fs::symlink(tmp.join("opt"), dos.join("opt-also")).unwrap();
        let found = find_node_modules(&[home]);
        let _ = fs::remove_dir_all(&tmp);
        assert!(
            found.iter().all(|p| !p.to_string_lossy().contains("/opt/")),
            "wine-style symlink escaped the home root: {found:?}"
        );
    }
}
