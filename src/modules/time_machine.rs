use std::time::Duration;

use crate::core::{
    Item, Module, ModuleInfo, ModuleScan, Privilege, ReclaimContext, ReclaimError, ReclaimResult,
    Relevance, Safety, ScanContext, ScanIssue, run_command, run_scan,
};

pub struct TimeMachineModule;

impl TimeMachineModule {
    fn snapshots(ctx: &ScanContext) -> Result<Vec<String>, ScanIssue> {
        let output = run_scan(
            "tmutil",
            &["listlocalsnapshots", "/"],
            Duration::from_secs(15),
            ctx,
        )
        .map_err(|err| ScanIssue::unavailable(format!("tmutil failed: {err}")))?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            if err.contains("Operation not permitted") || err.contains("Permission denied") {
                return Err(ScanIssue::full_disk_access(
                    "tmutil could not list local snapshots",
                ));
            }
            return Err(ScanIssue::warning(err.trim().to_string()));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("com.apple.TimeMachine.")
                    .map(|rest| rest.trim_end_matches(".local").to_string())
            })
            .filter(|s| !s.is_empty())
            .collect())
    }

    fn destination_configured(ctx: &ScanContext) -> bool {
        let Ok(output) = run_scan("tmutil", &["destinationinfo"], Duration::from_secs(10), ctx)
        else {
            return false;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        text.contains("Name") || text.contains("Kind")
    }

    fn destination_name(ctx: &ScanContext) -> Option<String> {
        let output = run_scan("tmutil", &["destinationinfo"], Duration::from_secs(10), ctx).ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines().find_map(|l| {
            l.strip_prefix("Name")
                .map(|s| s.trim().trim_start_matches(':').trim().to_string())
        })
    }
}

impl Module for TimeMachineModule {
    fn id(&self) -> &'static str {
        "timemachine"
    }
    fn name(&self) -> &'static str {
        "Time Machine"
    }
    fn description(&self) -> &'static str {
        "Local APFS snapshots and Time Machine destination health"
    }

    fn programs(&self) -> &'static [&'static str] {
        &["tmutil"]
    }

    fn info(&self, _ctx: &ScanContext) -> ModuleInfo {
        ModuleInfo::new(self.id(), self.name(), self.description())
            .finds("Local APFS snapshots Time Machine keeps on this disk")
            .finds("A Time Machine destination pointed at this Mac's own disk")
            .effect("Snapshots count as System Data and macOS only purges them under pressure")
            .effect("Deleting one asks for a one-shot administrator password; maclean never runs as root")
            .effect("Anything only in a local snapshot is gone — keep an external or network backup")
    }

    fn relevance(&self, ctx: &ScanContext) -> Relevance {
        let snap_mount = std::path::Path::new("/Volumes/com.apple.TimeMachine.localsnapshots");
        if snap_mount.exists() {
            return Relevance::yes(
                "local snapshot mount exists at /Volumes/com.apple.TimeMachine.localsnapshots",
            );
        }
        match Self::snapshots(ctx) {
            Ok(s) if !s.is_empty() => Relevance::yes(format!(
                "tmutil lists {}",
                crate::core::plural(s.len(), "local snapshot")
            )),
            Ok(_) if Self::destination_configured(ctx) => Relevance::yes(
                "Time Machine destination is configured (snapshots may still appear)",
            ),
            Ok(_) => Relevance::no("no local snapshots and no Time Machine destination configured"),
            Err(_) => Relevance::yes(
                "tmutil is present but listing snapshots failed — may need Full Disk Access",
            ),
        }
    }

    fn scan(&self, ctx: &ScanContext, relevance: Relevance) -> ModuleScan {
        let mut scan = ModuleScan::new(self.id(), self.name(), relevance);
        match Self::snapshots(ctx) {
            Ok(snaps) if !snaps.is_empty() => {
                let count = snaps.len();
                let children: Vec<Item> = snaps
                    .into_iter()
                    .map(|date| {
                        Item::new(
                            self.id(),
                            format!("timemachine:snapshot:{date}"),
                            format!("Snapshot {date}"),
                        )
                        .with_summary("On-disk APFS snapshot — space is only returned once it is deleted")
                        .with_safety(Safety::Caution)
                        .with_reclaimable(true)
                        .with_privilege(Privilege::Admin)
                        .with_detail("Taken", date.clone())
                        .with_detail("Command", format!("tmutil deletelocalsnapshots {date}"))
                        .with_note("macOS does not report a size for a snapshot; freed space shows up in System Data afterwards.")
                    })
                    .collect();
                scan.items.push(
                    Item::new(self.id(), "timemachine:snapshots", "Local snapshots")
                        .with_summary(format!(
                            "{} stored on this disk",
                            crate::core::plural(count, "snapshot")
                        ))
                        .with_safety(Safety::Caution)
                        .with_privilege(Privilege::Admin)
                        .with_detail("Snapshots", count.to_string())
                        .with_detail("Privilege", "one-shot sudo, asked for at clean time")
                        .with_note("This is the usual cause of a huge 'System Data' figure in About This Mac.")
                        .with_children(children),
                );
            }
            Ok(_) => {}
            Err(issue) => scan.issues.push(issue),
        }

        match Self::destination_name(ctx) {
            Some(name) if name == "Macintosh HD" => {
                scan.issues.push(ScanIssue::warning(
                    "Time Machine is pointed at this Mac (Macintosh HD). That can fill System Data. Use an external disk or NAS.",
                ));
            }
            Some(_) => {}
            None if Self::destination_configured(ctx) => {
                scan.issues.push(ScanIssue::warning(
                    "Time Machine destination is configured but tmutil could not name it — backups may be failing.",
                ));
            }
            None => {}
        }
        scan
    }

    fn reclaim(&self, item: &Item, ctx: &ReclaimContext) -> Result<ReclaimResult, ReclaimError> {
        let Some(date) = item.id.strip_prefix("timemachine:snapshot:") else {
            return Err(ReclaimError::new(
                &item.id,
                crate::core::IssueKind::Warning,
                format!("cannot reclaim '{}'", item.id),
            ));
        };
        if ctx.dry_run {
            return Ok(ReclaimResult::ok(
                &item.id,
                0,
                format!("would run tmutil deletelocalsnapshots {date}"),
                true,
            ));
        }
        run_command(item, "tmutil", &["deletelocalsnapshots", date], ctx)?;
        Ok(ReclaimResult::ok(
            &item.id,
            0,
            format!("deleted local snapshot {date}"),
            false,
        ))
    }
}
