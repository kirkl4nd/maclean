use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::core::{
    IssueKind, Item, Module, ModuleInfo, ModuleScan, ReclaimContext, ReclaimError, ReclaimResult,
    Relevance, Safety, ScanContext, ScanIssue, allocated_bytes, delete_contents, delete_tree,
    dir_size_in, format_bytes, plural,
};

/// Finder bookkeeping that is not worth a row of its own.
const SKIP_NAMES: &[&str] = &[".DS_Store", ".localized"];

/// Long tails of tiny files make the tree noisy; emptying Trash still
/// deletes everything in the folder.
const MAX_LISTED: usize = 400;

pub struct TrashModule;

impl TrashModule {
    fn trash_dir(ctx: &ScanContext) -> PathBuf {
        ctx.path("trash", "dir", ".Trash")
    }

    fn skip_name(name: &str) -> bool {
        SKIP_NAMES.contains(&name) || name.starts_with("._")
    }

    fn access_blocked(err: &io::Error) -> bool {
        matches!(err.raw_os_error(), Some(1) | Some(13))
            || err.kind() == io::ErrorKind::PermissionDenied
            || err.to_string().contains("Operation not permitted")
    }

    fn list_issue(path: &Path, err: io::Error) -> ScanIssue {
        if Self::access_blocked(&err) {
            ScanIssue::full_disk_access(format!(
                "Could not list {}. macOS hides Trash from apps without Full Disk Access.",
                path.display()
            ))
        } else {
            ScanIssue::permission(path, err.to_string())
        }
    }
}

impl Module for TrashModule {
    fn id(&self) -> &'static str {
        "trash"
    }
    fn name(&self) -> &'static str {
        "Trash"
    }
    fn description(&self) -> &'static str {
        "Files and folders in the user Trash"
    }

    fn paths(&self) -> Vec<(&'static str, &'static str)> {
        vec![("dir", ".Trash")]
    }

    fn info(&self, ctx: &ScanContext) -> ModuleInfo {
        ModuleInfo::new(self.id(), self.name(), self.description())
            .finds("Everything sitting in ~/.Trash")
            .effect("Emptying Trash deletes the contents of that folder in one go")
            .effect("Selecting individual rows deletes only those files or folders")
            .effect("There is no Put Back — this cannot be undone")
            .effect("Trash on external disks is not listed; empty those from Finder")
            .effect(
                "Listing ~/.Trash needs Full Disk Access for this terminal (sudo will not help)",
            )
            .location(Self::trash_dir(ctx))
    }

    fn relevance(&self, ctx: &ScanContext) -> Relevance {
        let path = Self::trash_dir(ctx);
        match fs::read_dir(&path) {
            Ok(entries) => {
                let any = entries.flatten().any(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|name| !Self::skip_name(name))
                });
                if any {
                    Relevance::yes(format!("Trash exists at {}", path.display()))
                } else {
                    Relevance::no("Trash is empty")
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                Relevance::no("no ~/.Trash folder")
            }
            Err(_) => Relevance::yes(
                "Trash exists but could not be listed — Full Disk Access may be required",
            ),
        }
    }

    fn scan(&self, ctx: &ScanContext, relevance: Relevance) -> ModuleScan {
        let mut scan = ModuleScan::new(self.id(), self.name(), relevance);
        let path = Self::trash_dir(ctx);
        let mut children = Vec::new();

        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(err) => {
                scan.issues.push(Self::list_issue(&path, err));
                return scan;
            }
        };

        for entry in entries.flatten() {
            if ctx.cancelled() {
                return scan;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if Self::skip_name(name) {
                continue;
            }
            let child = entry.path();
            let meta = match fs::symlink_metadata(&child) {
                Ok(m) => m,
                Err(err) => {
                    scan.issues
                        .push(crate::core::ScanIssue::permission(&child, err.to_string()));
                    continue;
                }
            };
            let (bytes, kind) = if meta.is_dir() {
                let report = dir_size_in(&child, ctx);
                scan.issues.extend(report.issues);
                (report.bytes, "Folder")
            } else if meta.file_type().is_symlink() {
                (meta.len(), "Alias")
            } else {
                (allocated_bytes(&child).max(meta.len()), "File")
            };
            children.push(
                Item::new(self.id(), format!("trash:item:{name}"), name)
                    .with_summary(child.display().to_string())
                    .with_bytes(bytes)
                    .with_path(child)
                    .with_safety(Safety::Destructive)
                    .with_reclaimable(true)
                    .with_detail("Kind", kind)
                    .with_note("Deleted permanently. Finder Put Back will not bring this back."),
            );
        }

        if children.is_empty() {
            return scan;
        }

        children.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.title.cmp(&b.title)));
        let total_count = children.len();
        let total_bytes: u64 = children.iter().map(|c| c.bytes).sum();
        let truncated = total_count > MAX_LISTED;

        let mut item = Item::new(self.id(), "trash:contents", "Trash contents")
            .with_summary(format!(
                "{} in {}",
                plural(total_count, "item"),
                path.display()
            ))
            .with_bytes(total_bytes)
            .with_path(path.clone())
            .with_safety(Safety::Destructive)
            .with_reclaimable(true)
            .clean_whole()
            .with_detail("Path", path.display().to_string())
            .with_detail("Items", total_count.to_string())
            .with_note(
                "Selecting this row empties ~/.Trash. Selecting a child deletes only that item.",
            )
            .with_note("This cannot be undone.")
            .with_children(children)
            .prune_children(0, MAX_LISTED);

        if truncated {
            item = item.with_note(format!(
                "Showing the largest {MAX_LISTED} of {total_count}. Emptying Trash still removes everything."
            ));
        }

        scan.items.push(item);
        scan
    }

    fn reclaim(&self, item: &Item, ctx: &ReclaimContext) -> Result<ReclaimResult, ReclaimError> {
        let Some(path) = item.paths.first() else {
            return Err(ReclaimError::new(&item.id, IssueKind::Warning, "no path"));
        };

        if item.id == "trash:contents" {
            let bytes = delete_contents(item, path, ctx)?;
            return Ok(ReclaimResult::ok(
                &item.id,
                bytes,
                format!("emptied Trash ({})", format_bytes(bytes)),
                ctx.dry_run,
            ));
        }

        let Some(name) = item.id.strip_prefix("trash:item:") else {
            return Err(ReclaimError::new(
                &item.id,
                IssueKind::Warning,
                format!("cannot reclaim '{}'", item.id),
            ));
        };
        if path.file_name().and_then(|s| s.to_str()) != Some(name) {
            return Err(ReclaimError::new(
                &item.id,
                IssueKind::Warning,
                format!(
                    "refusing to delete {} — name does not match",
                    path.display()
                ),
            ));
        }
        let bytes = delete_tree(item, path, ctx)?;
        Ok(ReclaimResult::ok(
            &item.id,
            bytes,
            format!(
                "deleted {} from Trash ({})",
                item.title,
                format_bytes(bytes)
            ),
            ctx.dry_run,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_at(home: PathBuf) -> ScanContext {
        ScanContext::for_home(home)
    }

    #[test]
    fn lists_files_and_skips_finder_bookkeeping() {
        let tmp = std::env::temp_dir().join(format!("maclean-trash-test-{}", std::process::id()));
        let trash = tmp.join(".Trash");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&trash).unwrap();
        fs::write(trash.join("notes.txt"), "hello").unwrap();
        fs::write(trash.join(".DS_Store"), "nope").unwrap();
        fs::write(trash.join(".localized"), "").unwrap();
        fs::create_dir(trash.join("old-folder")).unwrap();
        fs::write(trash.join("old-folder").join("a"), "x").unwrap();

        let module = TrashModule;
        let ctx = ctx_at(tmp.clone());
        let rel = module.relevance(&ctx);
        assert!(rel.relevant);
        let scan = module.scan(&ctx, rel);
        assert_eq!(scan.items.len(), 1);
        let names: Vec<&str> = scan.items[0]
            .children
            .iter()
            .map(|c| c.title.as_str())
            .collect();
        assert!(names.contains(&"notes.txt"));
        assert!(names.contains(&"old-folder"));
        assert!(
            !names
                .iter()
                .any(|n| *n == ".DS_Store" || *n == ".localized")
        );
        assert!(scan.items[0].clean_whole);
        assert_eq!(scan.items[0].id, "trash:contents");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_trash_is_not_relevant() {
        let tmp = std::env::temp_dir().join(format!("maclean-trash-empty-{}", std::process::id()));
        let trash = tmp.join(".Trash");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&trash).unwrap();
        fs::write(trash.join(".DS_Store"), "x").unwrap();
        let module = TrashModule;
        let rel = module.relevance(&ctx_at(tmp.clone()));
        assert!(!rel.relevant);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dry_run_empties_or_deletes_without_touching_files() {
        let tmp = std::env::temp_dir().join(format!("maclean-trash-dry-{}", std::process::id()));
        let trash = tmp.join(".Trash");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&trash).unwrap();
        fs::write(trash.join("keep-me.txt"), "data").unwrap();

        let module = TrashModule;
        let scan_ctx = ctx_at(tmp.clone());
        let scan = module.scan(&scan_ctx, module.relevance(&scan_ctx));
        let parent = &scan.items[0];
        let reclaim = ReclaimContext::dry(&scan_ctx);

        let emptied = module.reclaim(parent, &reclaim).unwrap();
        assert!(emptied.dry_run);
        assert!(emptied.message.contains("Trash"));
        assert!(trash.join("keep-me.txt").exists());

        let file = parent
            .children
            .iter()
            .find(|c| c.title == "keep-me.txt")
            .unwrap();
        let deleted = module.reclaim(file, &reclaim).unwrap();
        assert!(deleted.dry_run);
        assert!(deleted.message.contains("keep-me.txt"));
        assert!(trash.join("keep-me.txt").exists());

        let bogus = Item::new("trash", "trash:nope", "nope").with_path(trash.clone());
        assert!(module.reclaim(&bogus, &reclaim).is_err());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unlistable_trash_is_relevant_and_reports_access() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = std::env::temp_dir().join(format!("maclean-trash-denied-{}", std::process::id()));
        let trash = tmp.join(".Trash");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&trash).unwrap();
        fs::write(trash.join("secret.bin"), vec![0u8; 64]).unwrap();
        let deny = fs::Permissions::from_mode(0o000);
        fs::set_permissions(&trash, deny).unwrap();

        let module = TrashModule;
        let ctx = ctx_at(tmp.clone());
        let rel = module.relevance(&ctx);
        assert!(rel.relevant, "{}", rel.reason);
        let scan = module.scan(&ctx, rel);
        assert!(scan.items.is_empty());
        assert!(!scan.issues.is_empty());
        assert!(scan.issues.iter().any(|i| {
            matches!(
                i.kind,
                crate::core::IssueKind::NeedsFullDiskAccess | crate::core::IssueKind::Permission
            )
        }));
        assert!(scan.tree_root().is_some());

        let _ = fs::set_permissions(&trash, fs::Permissions::from_mode(0o755));
        let _ = fs::remove_dir_all(&tmp);
    }
}
