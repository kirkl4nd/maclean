use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::{
    IssueKind, Item, Module, ModuleInfo, ModuleScan, ReclaimContext, ReclaimError, ReclaimResult,
    Relevance, Safety, ScanContext, ScanIssue, ScheduleTarget, allocated_bytes, delete_tree,
    format_bytes, run_command, run_scan, run_scan_with,
};

pub struct DockerModule;

impl DockerModule {
    fn data_dir(ctx: &ScanContext) -> PathBuf {
        ctx.path("docker", "data", "Library/Containers/com.docker.docker")
    }
    fn group_dir(ctx: &ScanContext) -> PathBuf {
        ctx.path(
            "docker",
            "group",
            "Library/Group Containers/group.com.docker",
        )
    }
    fn dot_docker(ctx: &ScanContext) -> PathBuf {
        ctx.path("docker", "cli", ".docker")
    }

    fn disk_image(ctx: &ScanContext) -> Option<PathBuf> {
        let data = Self::data_dir(ctx).join("Data/vms/0/data");
        for name in ["Docker.raw", "Docker.qcow2"] {
            let path = data.join(name);
            if path.is_file() {
                return Some(path);
            }
        }
        None
    }

    fn docker_running(allowed: &[&str], path_dirs: &[&str]) -> bool {
        Self::daemon_error(allowed, path_dirs).is_none()
    }

    /// `None` if the daemon answered. Otherwise the exact CLI error, so a
    /// missing socket or a permission failure is visible instead of a blank tree.
    fn daemon_error(allowed: &[&str], path_dirs: &[&str]) -> Option<String> {
        match run_scan_with(
            "docker",
            &["version", "--format", "{{.Server.Version}}"],
            Duration::from_secs(8),
            allowed,
            path_dirs,
        ) {
            Ok(output) if output.status.success() && !output.stdout.is_empty() => None,
            Ok(output) => {
                let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
                Some(if !err.is_empty() {
                    err
                } else if !out.is_empty() {
                    out
                } else {
                    "docker daemon did not respond".into()
                })
            }
            Err(err) => Some(err),
        }
    }

    fn lines(args: &[&str], ctx: &ScanContext) -> Result<Vec<String>, String> {
        let output = run_scan("docker", args, Duration::from_secs(30), ctx)?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(if err.is_empty() {
                format!("docker {} failed", args.join(" "))
            } else {
                err
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| l.to_string())
            .filter(|l| !l.trim().is_empty())
            .collect())
    }
}

impl Module for DockerModule {
    fn id(&self) -> &'static str {
        "docker"
    }
    fn name(&self) -> &'static str {
        "Docker"
    }
    fn description(&self) -> &'static str {
        "Docker Desktop VM disk, images, volumes, containers, and build cache"
    }

    fn programs(&self) -> &'static [&'static str] {
        &["docker"]
    }

    fn path_dirs(&self) -> &'static [&'static str] {
        &["/Applications/Docker.app/Contents/Resources/bin"]
    }

    fn schedule_targets(&self) -> Vec<ScheduleTarget> {
        vec![
            ScheduleTarget::new(
                "docker:build-cache",
                "Build cache",
                "docker builder prune when the job runs",
            ),
            ScheduleTarget::new(
                "docker:images",
                "Unused images",
                "docker image prune --all when the job runs",
            ),
            ScheduleTarget::new(
                "docker:volumes",
                "Unused volumes",
                "docker volume prune when the job runs",
            ),
            ScheduleTarget::new(
                "docker:containers",
                "Stopped containers",
                "docker container prune when the job runs",
            ),
        ]
    }

    fn paths(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("data", "Library/Containers/com.docker.docker"),
            ("group", "Library/Group Containers/group.com.docker"),
            ("cli", ".docker"),
        ]
    }

    fn info(&self, ctx: &ScanContext) -> ModuleInfo {
        ModuleInfo::new(self.id(), self.name(), self.description())
            .finds("The Docker Desktop VM disk image (Docker.raw), which Finder often shows at its 60+ GB ceiling")
            .finds("Images, named volumes, stopped containers, and the builder cache inside that VM")
            .finds("Leftover Docker data when Docker Desktop has been uninstalled")
            .effect("Cleaning images/volumes runs docker prune — it does not delete the VM file")
            .effect("Compacting the VM runs fstrim inside Docker's Linux VM (nsenter); it does not delete images")
            .effect("The VM file is never deleted while Docker Desktop is installed")
            .location(Self::data_dir(ctx))
            .location(Self::group_dir(ctx))
            .location(Self::dot_docker(ctx))
    }

    fn relevance(&self, ctx: &ScanContext) -> Relevance {
        if Self::disk_image(ctx).is_some() || Self::data_dir(ctx).is_dir() {
            return Relevance::yes("Docker Desktop data exists on this Mac");
        }
        if Self::group_dir(ctx).is_dir() {
            return Relevance::yes("Docker group container exists (leftover Desktop data)");
        }
        if Self::dot_docker(ctx).is_dir() {
            return Relevance::yes("~/.docker exists (CLI config / leftover data)");
        }
        Relevance::no("no Docker Desktop data, group container, or ~/.docker directory")
    }

    fn scan(&self, ctx: &ScanContext, relevance: Relevance) -> ModuleScan {
        let mut scan = ModuleScan::new(self.id(), self.name(), relevance);
        let daemon = Self::daemon_error(&ctx.allowed_programs, &ctx.path_dirs);

        // Overview is just files on disk. Images live inside the VM, so this
        // number is right whether or not the daemon is up.
        if let Some(image) = Self::disk_image(ctx) {
            scan.items.push(vm_item(&image, daemon.is_none()));
        }
        for (id, title, path) in [
            ("docker:group", "Group container", Self::group_dir(ctx)),
            ("docker:cli", "~/.docker", Self::dot_docker(ctx)),
        ] {
            if !path.is_dir() {
                continue;
            }
            let bytes = crate::core::dir_size_in(&path, ctx).bytes;
            if bytes == 0 {
                continue;
            }
            scan.items.push(
                Item::new(self.id(), id, title)
                    .with_summary(path.display().to_string())
                    .with_bytes(bytes)
                    .with_path(path)
                    .with_safety(Safety::Info)
                    .with_reclaimable(false)
                    .with_note(
                        "Shown for size only. It is not inside the VM, and it is not deleted.",
                    ),
            );
        }

        if let Some(err) = daemon {
            scan.issues.push(
                ScanIssue::unavailable(format!("Image and volume list unavailable. {err}"))
                    .with_hint(
                        "The on-disk figure above is still accurate. Start Docker Desktop to see what is inside the VM.",
                    ),
            );
            return scan;
        }

        match docker_inventory(ctx) {
            Ok(inv) => {
                push_group(
                    &mut scan,
                    "docker:images",
                    "Images",
                    "docker image prune --all",
                    Safety::Caution,
                    "Removes images not used by a container. They are pulled or rebuilt on next use.",
                    inv.images,
                );
                push_group(
                    &mut scan,
                    "docker:volumes",
                    "Volumes",
                    "docker volume prune",
                    Safety::Destructive,
                    "Volumes hold databases and uploaded files. Deleting one cannot be undone.",
                    inv.volumes,
                );
                push_group(
                    &mut scan,
                    "docker:containers",
                    "Containers",
                    "docker container prune",
                    Safety::Caution,
                    "Only stopped containers are removed.",
                    inv.containers,
                );
                if inv.build_cache > 0 {
                    scan.items.push(
                        Item::new(self.id(), "docker:build-cache", "Build cache")
                            .with_summary("Reclaimable layer cache from docker build")
                            .with_bytes(inv.build_cache)
                            .with_safety(Safety::Safe)
                            .with_reclaimable(true)
                            .clean_whole()
                            .with_detail("Cleanup", "docker builder prune --all")
                            .with_note(
                                "The next build of each image will be slower, then cached again.",
                            ),
                    );
                }
            }
            Err(err) => scan.issues.push(ScanIssue::warning(err)),
        }

        scan
    }

    fn reclaim(&self, item: &Item, ctx: &ReclaimContext) -> Result<ReclaimResult, ReclaimError> {
        if item.id == "docker:vm" {
            return compact_vm(item, ctx);
        }
        if item.id == "docker:data" {
            let path = item.paths.first().cloned().unwrap_or_default();
            if Self::docker_running(&ctx.allowed_programs, &ctx.path_dirs) {
                return Err(ReclaimError::new(
                    &item.id,
                    IssueKind::Warning,
                    "Docker Desktop is running; the VM disk will not be deleted",
                )
                .with_hint(
                    "Quit Docker Desktop first, or compact the VM instead of deleting it.",
                ));
            }
            let bytes = delete_tree(item, &path, ctx)?;
            return Ok(ReclaimResult::ok(
                &item.id,
                bytes,
                format!("removed leftover Docker data ({})", format_bytes(bytes)),
                ctx.dry_run,
            ));
        }

        if ctx.dry_run {
            return Ok(ReclaimResult::ok(
                &item.id,
                item.bytes,
                format!("would run docker {}", docker_args(item)?.join(" ")),
                true,
            ));
        }

        let args = docker_args(item)?;
        let output = run_command(
            item,
            "docker",
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
            ctx,
        )?;
        let msg = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(ReclaimResult::ok(
            &item.id,
            item.bytes,
            if msg.is_empty() {
                format!("docker {}", args.join(" "))
            } else {
                msg
            },
            false,
        ))
    }
}

fn vm_item(path: &Path, daemon_up: bool) -> Item {
    let logical = path.metadata().map(|m| m.len()).unwrap_or(0);
    let on_disk = allocated_bytes(path);
    Item::new("docker", "docker:vm", "VM disk image")
        .with_summary(format!(
            "{} on disk, {} apparent (the ceiling Finder shows)",
            format_bytes(on_disk),
            format_bytes(logical)
        ))
        .with_bytes(on_disk)
        .with_path(path.to_path_buf())
        .with_safety(Safety::Caution)
        .with_reclaimable(daemon_up && on_disk > 500 * 1_000_000)
        .clean_whole()
        .with_detail("On disk", format_bytes(on_disk))
        .with_detail("Apparent size", format_bytes(logical))
        .with_detail("File", path.display().to_string())
        .with_detail("Cleanup", compact_args().join(" "))
        .with_note(
            "This is a sparse file. macOS Storage often lists the apparent size (commonly 60 GB); only the on-disk figure is actually occupied.",
        )
        .with_note(
            "Images, volumes, and containers live inside this file. Compacting runs fstrim in the Linux VM so unused blocks go back to macOS. Never delete Docker.raw in Finder.",
        )
}

/// `docker/desktop-reclaim-space` is linux/amd64 only. On Apple Silicon it
/// prints a platform warning and dies with `setns:mnt: Invalid argument`.
/// Alpine is multi-arch and ships nsenter + fstrim; this is the same TRIM
/// the official image was meant to run, inside the Linux VM.
fn compact_args() -> Vec<&'static str> {
    vec![
        "run",
        "--rm",
        "--privileged",
        "--pid=host",
        "alpine:3.21",
        "nsenter",
        "-t",
        "1",
        "-m",
        "-u",
        "-n",
        "-i",
        "fstrim",
        "-v",
        "/var/lib/docker",
    ]
}

fn compact_vm(item: &Item, ctx: &ReclaimContext) -> Result<ReclaimResult, ReclaimError> {
    let before = item
        .paths
        .first()
        .map(|p| allocated_bytes(p))
        .unwrap_or(item.bytes);
    let args = compact_args();
    if ctx.dry_run {
        return Ok(ReclaimResult::ok(
            &item.id,
            before,
            format!("would run docker {}", args.join(" ")),
            true,
        ));
    }
    let output = run_command(item, "docker", &args, ctx)?;
    let after = item
        .paths
        .first()
        .map(|p| allocated_bytes(p))
        .unwrap_or(before);
    let freed = before.saturating_sub(after);
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let mut message = if freed > 0 {
        format!("compacted VM disk, freed {}", format_bytes(freed))
    } else {
        "trimmed unused blocks in the Docker VM".into()
    };
    if !stdout.is_empty() {
        message.push_str(": ");
        message.push_str(&stdout);
    }
    Ok(ReclaimResult::ok(&item.id, freed, message, false))
}

fn docker_args(item: &Item) -> Result<Vec<String>, ReclaimError> {
    let args: Vec<String> = match item.id.as_str() {
        "docker:build-cache" => vec!["builder", "prune", "--all", "--force"],
        "docker:images" => vec!["image", "prune", "--all", "--force"],
        "docker:volumes" => vec!["volume", "prune", "--force"],
        "docker:containers" => vec!["container", "prune", "--force"],
        other => {
            if let Some(id) = other.strip_prefix("docker:image:") {
                vec!["rmi", "--force", id]
            } else if let Some(name) = other.strip_prefix("docker:volume:") {
                vec!["volume", "rm", name]
            } else if let Some(id) = other.strip_prefix("docker:container:") {
                vec!["rm", "--force", id]
            } else {
                return Err(ReclaimError::new(
                    &item.id,
                    IssueKind::Warning,
                    format!("docker module does not reclaim '{}'", item.id),
                ));
            }
        }
    }
    .into_iter()
    .map(|s| s.to_string())
    .collect();
    Ok(args)
}

struct Inventory {
    images: Vec<Item>,
    volumes: Vec<Item>,
    containers: Vec<Item>,
    build_cache: u64,
}

/// `docker image ls` hides some Desktop-internal images. `system df -v` does not.
fn docker_inventory(ctx: &ScanContext) -> Result<Inventory, String> {
    let lines = DockerModule::lines(&["system", "df", "-v"], ctx)?;
    let mut section = Section::None;
    let mut images = Vec::new();
    let mut volumes = Vec::new();
    let mut containers = Vec::new();
    let mut build_cache = 0u64;
    let mut skip_header = false;

    for line in lines {
        let lower = line.to_lowercase();
        if lower.starts_with("images space usage") {
            section = Section::Images;
            skip_header = true;
            continue;
        }
        if lower.starts_with("containers space usage") {
            section = Section::Containers;
            skip_header = true;
            continue;
        }
        if lower.starts_with("local volumes space usage") {
            section = Section::Volumes;
            skip_header = true;
            continue;
        }
        if lower.starts_with("build cache") {
            section = Section::Build;
            skip_header = true;
            if let Some(bytes) = line.split_whitespace().last().map(parse_docker_size) {
                if line.contains(':') {
                    // "Build cache usage: 12.3GB"
                    build_cache = bytes;
                }
            }
            continue;
        }
        if skip_header {
            skip_header = false;
            continue;
        }
        match section {
            Section::Images => {
                let cols: Vec<&str> = line.split_whitespace().collect();
                // REPOSITORY TAG IMAGE ID CREATED SIZE SHARED UNIQUE CONTAINERS
                if cols.len() < 5 {
                    continue;
                }
                let repo = cols[0];
                let tag = cols[1];
                let id = cols[2];
                let size = parse_docker_size(cols[4]);
                let dangling = repo == "<none>";
                let title = if dangling {
                    format!("<dangling> {id}")
                } else {
                    format!("{repo}:{tag}")
                };
                images.push(
                    Item::new("docker", format!("docker:image:{id}"), title)
                        .with_summary(format!("created {}", cols.get(3).unwrap_or(&"")))
                        .with_bytes(size)
                        .with_safety(if dangling {
                            Safety::Safe
                        } else {
                            Safety::Caution
                        })
                        .with_reclaimable(true)
                        .with_detail("Image ID", id),
                );
            }
            Section::Containers => {
                let cols: Vec<&str> = line.split_whitespace().collect();
                // CONTAINER ID IMAGE COMMAND ... STATUS ... NAMES — too ragged.
                // Use the first field as id; last as name.
                if cols.len() < 7 {
                    continue;
                }
                let id = cols[0];
                let image = cols[1];
                let name = *cols.last().unwrap_or(&id);
                let running = line.to_lowercase().contains(" up ");
                let size = cols
                    .iter()
                    .find(|c| parse_docker_size(c) > 0 && (c.contains('B') || c.contains('b')))
                    .map(|c| parse_docker_size(c))
                    .unwrap_or(0);
                containers.push(
                    Item::new("docker", format!("docker:container:{id}"), name)
                        .with_summary(image)
                        .with_bytes(size)
                        .with_safety(if running {
                            Safety::Destructive
                        } else {
                            Safety::Caution
                        })
                        .with_reclaimable(!running)
                        .with_detail("Container ID", id)
                        .with_detail("Image", image)
                        .with_note(if running {
                            "This container is running. maclean will not remove it."
                        } else {
                            "Removes the container's writable layer, not its image or volumes."
                        }),
                );
            }
            Section::Volumes => {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() < 3 {
                    continue;
                }
                let name = cols[0];
                let bytes = parse_docker_size(cols[2]);
                volumes.push(
                    Item::new("docker", format!("docker:volume:{name}"), name)
                        .with_summary("named volume")
                        .with_bytes(bytes)
                        .with_safety(Safety::Destructive)
                        .with_reclaimable(true)
                        .with_note("Deleting a volume permanently deletes whatever a container stored in it."),
                );
            }
            Section::Build => {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() >= 3 {
                    build_cache = build_cache.saturating_add(parse_docker_size(cols[2]));
                }
            }
            Section::None => {}
        }
    }

    Ok(Inventory {
        images,
        volumes,
        containers,
        build_cache,
    })
}

enum Section {
    None,
    Images,
    Containers,
    Volumes,
    Build,
}

fn push_group(
    scan: &mut ModuleScan,
    id: &str,
    title: &str,
    cleanup: &str,
    safety: Safety,
    note: &str,
    children: Vec<Item>,
) {
    if children.is_empty() {
        return;
    }
    let bytes: u64 = children.iter().map(|i| i.bytes).sum();
    let count = children.len();
    scan.items.push(
        Item::new("docker", id, title)
            .with_summary(format!("{count} listed by docker system df"))
            .with_bytes(bytes)
            .with_safety(safety)
            .with_reclaimable(true)
            .clean_whole()
            .with_detail("Cleanup", cleanup)
            .with_note(note)
            .with_children(children),
    );
}

fn parse_docker_size(raw: &str) -> u64 {
    let s = raw.trim().split('(').next().unwrap_or(raw).trim();
    let digits = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect::<String>()
        .replace(',', ".");
    let n: f64 = digits.parse().unwrap_or(0.0);
    let lower = s.to_ascii_lowercase();
    let mul = if lower.contains("tb") {
        1_000_000_000_000.0
    } else if lower.contains("gb") {
        1_000_000_000.0
    } else if lower.contains("mb") {
        1_000_000.0
    } else if lower.contains("kb") {
        1_000.0
    } else {
        1.0
    };
    (n * mul) as u64
}
