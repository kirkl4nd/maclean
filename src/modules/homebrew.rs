use std::path::PathBuf;

use crate::core::{
    Item, Module, ModuleInfo, ModuleScan, ReclaimContext, ReclaimError, ReclaimResult, Relevance,
    Safety, ScanContext, ScheduleTarget, delete_contents, dir_size, dir_size_in, format_bytes,
    run_command,
};

pub struct HomebrewModule;

impl HomebrewModule {
    fn cache_dir(ctx: &ScanContext) -> PathBuf {
        ctx.path("homebrew", "cache", "Library/Caches/Homebrew")
    }
}

impl Module for HomebrewModule {
    fn id(&self) -> &'static str {
        "homebrew"
    }
    fn name(&self) -> &'static str {
        "Homebrew"
    }
    fn description(&self) -> &'static str {
        "Downloaded bottles and cask artifacts kept in the Homebrew cache"
    }

    fn programs(&self) -> &'static [&'static str] {
        &["brew"]
    }

    fn paths(&self) -> Vec<(&'static str, &'static str)> {
        vec![("cache", "Library/Caches/Homebrew")]
    }

    fn schedule_targets(&self) -> Vec<ScheduleTarget> {
        vec![ScheduleTarget::new(
            "homebrew:cache",
            "Homebrew cache",
            "Downloaded bottles and cask artifacts",
        )]
    }

    fn info(&self, ctx: &ScanContext) -> ModuleInfo {
        ModuleInfo::new(self.id(), self.name(), self.description())
            .finds("Downloaded bottle archives kept after installing or upgrading")
            .finds("Cask installers (.dmg/.pkg) Homebrew already unpacked")
            .effect("Installed packages are not touched — only the download cache")
            .effect("Runs `brew cleanup --prune=all` when brew is available")
            .location(Self::cache_dir(ctx))
    }

    fn relevance(&self, ctx: &ScanContext) -> Relevance {
        let path = Self::cache_dir(ctx);
        if path.is_dir() {
            Relevance::yes(format!("Homebrew cache exists at {}", path.display()))
        } else {
            Relevance::no("no Homebrew cache directory")
        }
    }

    fn scan(&self, ctx: &ScanContext, relevance: Relevance) -> ModuleScan {
        let mut scan = ModuleScan::new(self.id(), self.name(), relevance);
        let path = Self::cache_dir(ctx);
        let mut children = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&path) {
            for entry in entries.flatten() {
                if ctx.cancelled() {
                    return scan;
                }
                let child = entry.path();
                let report = dir_size_in(&child, ctx);
                scan.issues.extend(report.issues);
                if report.bytes == 0 {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                children.push(
                    Item::new(self.id(), format!("homebrew:cache:{name}"), name)
                        .with_summary(child.display().to_string())
                        .with_bytes(report.bytes)
                        .with_path(child)
                        .with_safety(Safety::Safe)
                        .with_reclaimable(true),
                );
            }
        }

        let total: u64 = children.iter().map(|c| c.bytes).sum();
        if total == 0 {
            return scan;
        }

        scan.items.push(
            Item::new(self.id(), "homebrew:cache", "Homebrew cache")
                .with_summary(format!("Bottles and cask files in {}", path.display()))
                .with_bytes(total)
                .with_path(path.clone())
                .with_safety(Safety::Safe)
                .with_reclaimable(true)
                .clean_whole()
                .with_detail("Path", path.display().to_string())
                .with_detail("Method", "brew cleanup --prune=all, else delete contents")
                .with_note("Installed formulae and casks keep working; only downloads are removed.")
                .with_note("Reinstalling a package will download it again.")
                .with_children(children)
                .prune_children(10 * 1000 * 1000, 12),
        );
        scan
    }

    fn reclaim(&self, item: &Item, ctx: &ReclaimContext) -> Result<ReclaimResult, ReclaimError> {
        let path = item.paths.first().cloned().unwrap_or_default();
        if ctx.dry_run {
            return Ok(ReclaimResult::ok(
                &item.id,
                item.bytes,
                format!(
                    "would run brew cleanup --prune=all ({})",
                    format_bytes(item.bytes)
                ),
                true,
            ));
        }
        let before = dir_size(&path);
        let brew = run_command(item, "brew", &["cleanup", "--prune=all"], ctx);
        let bytes = match brew {
            Ok(_) => {
                let freed = before.saturating_sub(dir_size(&path));
                if freed > 0 {
                    freed
                } else {
                    delete_contents(item, &path, ctx).unwrap_or(0)
                }
            }
            Err(_) => delete_contents(item, &path, ctx)?,
        };
        Ok(ReclaimResult::ok(
            &item.id,
            bytes,
            format!("cleared Homebrew cache ({})", format_bytes(bytes)),
            false,
        ))
    }
}
