use std::path::PathBuf;

use crate::core::{
    IssueKind, Item, Module, ModuleInfo, ModuleScan, ReclaimContext, ReclaimError, ReclaimResult,
    Relevance, Safety, ScanContext, delete_contents, dir_size_in, format_bytes,
};

pub struct SpotifyModule;

impl SpotifyModule {
    fn cache_dir(ctx: &ScanContext) -> PathBuf {
        ctx.path("spotify", "cache", "Library/Caches/com.spotify.client")
    }

    fn app_dir(ctx: &ScanContext) -> PathBuf {
        ctx.path("spotify", "app", "Library/Application Support/Spotify")
    }
}

impl Module for SpotifyModule {
    fn id(&self) -> &'static str {
        "spotify"
    }
    fn name(&self) -> &'static str {
        "Spotify"
    }
    fn description(&self) -> &'static str {
        "Offline/stream cache used by the Spotify desktop app"
    }

    fn paths(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("cache", "Library/Caches/com.spotify.client"),
            ("app", "Library/Application Support/Spotify"),
        ]
    }

    fn info(&self, ctx: &ScanContext) -> ModuleInfo {
        ModuleInfo::new(self.id(), self.name(), self.description())
            .finds("Cached audio Spotify downloaded while streaming")
            .finds("Browser/renderer caches from Spotify's embedded Chromium")
            .effect("Playlists, logins, and downloads marked for offline are not stored here")
            .effect("Spotify re-downloads audio as you play it, so the cache grows back")
            .location(Self::cache_dir(ctx))
            .location(Self::app_dir(ctx))
    }

    fn relevance(&self, ctx: &ScanContext) -> Relevance {
        let path = Self::cache_dir(ctx);
        if path.is_dir() {
            Relevance::yes(format!("cache directory exists at {}", path.display()))
        } else {
            Relevance::no("no Spotify cache directory (app may never have run here)")
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
                    Item::new(self.id(), format!("spotify:cache:{name}"), name)
                        .with_summary(child.display().to_string())
                        .with_bytes(report.bytes)
                        .with_path(child)
                        .with_safety(Safety::Safe)
                        .with_reclaimable(true)
                        .with_detail("Kind", "Spotify cache subfolder"),
                );
            }
        }

        let total: u64 = children.iter().map(|c| c.bytes).sum();
        if total == 0 {
            return scan;
        }

        scan.items.push(
            Item::new(self.id(), "spotify:cache", "Spotify cache")
                .with_summary(format!(
                    "Streamed audio and app cache in {}",
                    path.display()
                ))
                .with_bytes(total)
                .with_path(path.clone())
                .with_safety(Safety::Safe)
                .with_reclaimable(true)
                .clean_whole()
                .with_detail("Path", path.display().to_string())
                .with_detail("Rebuilds", "Yes, as you keep listening")
                .with_note("Deletes the contents of the cache folder, not the folder itself.")
                .with_note(
                    "Your account, playlists, and settings live elsewhere and are untouched.",
                )
                .with_children(children)
                .prune_children(10 * 1000 * 1000, 12),
        );
        scan
    }

    fn reclaim(&self, item: &Item, ctx: &ReclaimContext) -> Result<ReclaimResult, ReclaimError> {
        let Some(path) = item.paths.first() else {
            return Err(ReclaimError::new(&item.id, IssueKind::Warning, "no path"));
        };
        let bytes = delete_contents(item, path, ctx)?;
        Ok(ReclaimResult::ok(
            &item.id,
            bytes,
            format!("cleared {} ({})", item.title, format_bytes(bytes)),
            ctx.dry_run,
        ))
    }
}
