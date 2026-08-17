use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::core::{
    Item, ModuleScan, ReclaimContext, Registry, ScanContext, disk_usage, format_bytes,
};
use crate::schedule::{self, ScheduledJob};
use crate::tui;

#[derive(Parser)]
#[command(
    name = "maclean",
    version,
    about = "Find and reclaim disk space. Interactive TUI, or flags for scripts and agents.",
    after_help = "\
EXAMPLES:
  maclean                          Launch the interactive UI
  maclean scan                     List reclaimable items
  maclean scan --json              Same, machine-readable
  maclean scan -m node             Package manager caches and node_modules
  maclean modules docker           What the Docker module does, in its words
  maclean reclaim spotify:cache    Dry-run a specific item
  maclean reclaim spotify:cache --yes
  maclean reclaim --module docker --all --yes
  maclean schedule add spotify:cache --every 2w
  maclean schedule list
  maclean uninstall
  maclean config
  maclean config init
  maclean config enable cargo
  maclean config path cargo home ~/.cargo
  maclean config roots cargo ~
"
)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    yes: bool,
    #[arg(long, global = true, value_name = "DIR")]
    roots: Vec<PathBuf>,
    /// Config file. Default: ~/Library/Application Support/maclean/config.toml
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Scan {
        #[arg(short, long)]
        module: Option<String>,
    },
    Reclaim {
        ids: Vec<String>,
        #[arg(short, long)]
        module: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// List modules, or explain one in the module's own words.
    Modules { id: Option<String> },
    /// Recurring reclaim jobs. Same thing as pressing s in the interactive UI.
    Schedule {
        #[command(subcommand)]
        cmd: ScheduleCmd,
    },
    /// Remove launchd jobs maclean created, then tell you how to drop the binary.
    Uninstall {
        /// Also delete config and logs. Does not touch anyone else's files.
        #[arg(long)]
        purge_data: bool,
    },
    /// Show, write, or change the config file.
    Config {
        #[command(subcommand)]
        cmd: Option<ConfigCmd>,
        /// Write a starter file (same as `config init`).
        #[arg(long)]
        init: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Write a complete file with every module's current defaults.
    Init,
    /// Check the file and report every problem.
    Validate,
    Enable {
        module: String,
    },
    Disable {
        module: String,
    },
    Path {
        module: String,
        key: String,
        value: String,
    },
    /// Search folders for a module that walks a tree. Omit dirs to reset to ~.
    Roots {
        module: String,
        dirs: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ScheduleCmd {
    /// Show jobs maclean has installed. Do not edit the plist files yourself.
    List,
    Add {
        item_id: String,
        #[arg(long)]
        every: String,
    },
    Remove {
        item_id: String,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    // Uninstall must work even if config is broken — otherwise you cannot
    // take the jobs down.
    if let Some(Command::Uninstall { purge_data }) = cli.command {
        return cmd_uninstall(purge_data);
    }
    let registry = Arc::new(crate::modules::registry());
    let mut ctx = ScanContext::load(cli.config.as_deref(), &registry.specs())?;
    ctx.add_roots(&cli.roots)?;
    registry.bind(&mut ctx);

    match cli.command {
        None => {
            if cli.json || !std::io::stdout().is_terminal() {
                cmd_scan(&registry, &ctx, None, true)?;
            } else {
                tui::run(registry, ctx)?;
            }
        }
        Some(Command::Scan { module }) => {
            cmd_scan(&registry, &ctx, module.as_deref(), cli.json)?;
        }
        Some(Command::Reclaim { ids, module, all }) => {
            cmd_reclaim(
                &registry,
                &ctx,
                ids,
                module.as_deref(),
                all,
                cli.yes,
                cli.json,
            )?;
        }
        Some(Command::Modules { id }) => match id {
            Some(id) => cmd_module_info(&registry, &ctx, &id, cli.json)?,
            None => cmd_modules(&registry, &ctx, cli.json)?,
        },
        Some(Command::Schedule { cmd }) => cmd_schedule(cmd, cli.json)?,
        Some(Command::Uninstall { .. }) => unreachable!("handled before config load"),
        Some(Command::Config { cmd, init }) => {
            cmd_config(&registry, &mut ctx, cmd, init, cli.json)?
        }
    }
    Ok(())
}

fn cmd_scan(
    registry: &Registry,
    ctx: &ScanContext,
    module: Option<&str>,
    json: bool,
) -> Result<()> {
    let disk = disk_usage(std::path::Path::new("/")).ok();
    if let Some(id) = module {
        let scan = registry.scan_module(id, ctx)?;
        if json {
            print_json(&scan)?;
            return Ok(());
        }
        print_scan(&scan);
        return Ok(());
    }
    let scans = registry.scan_all(ctx);
    let tree: Vec<Item> = scans.iter().filter_map(ModuleScan::tree_root).collect();
    if json {
        print_json(&ScanReport {
            disk,
            tree: &tree,
            modules: &scans,
        })?;
        return Ok(());
    }
    if let Some(disk) = &disk {
        println!(
            "Disk: {} used, {} available of {}",
            format_bytes(disk.used_bytes),
            format_bytes(disk.available_bytes),
            format_bytes(disk.total_bytes)
        );
        println!();
    }
    if tree.is_empty() {
        println!("Nothing to clean. See `maclean modules` for what was checked.");
        return Ok(());
    }
    for root in &tree {
        print_item(root, 0);
    }
    Ok(())
}

fn print_scan(scan: &ModuleScan) {
    println!(
        "{}  relevant={}  {}",
        scan.module_id, scan.relevance.relevant, scan.relevance.reason
    );
    for issue in &scan.issues {
        println!("  ! {} — {}", issue.title(), issue.message);
        if let Some(hint) = &issue.hint {
            println!("      {hint}");
        }
    }
    if let Some(root) = scan.tree_root() {
        print_item(&root, 0);
    }
}

fn print_item(item: &Item, depth: usize) {
    let indent = "  ".repeat(depth);
    println!(
        "{indent}{:<40} {:>10}  {}",
        truncate_middle(&item.id, 40),
        format_bytes(item.bytes),
        item.title
    );
    for child in &item.children {
        print_item(child, depth + 1);
    }
}

fn cmd_reclaim(
    registry: &Registry,
    ctx: &ScanContext,
    ids: Vec<String>,
    module: Option<&str>,
    all: bool,
    yes: bool,
    json: bool,
) -> Result<()> {
    let tree = if let Some(id) = module {
        registry
            .scan_module(id, ctx)?
            .tree_root()
            .into_iter()
            .collect()
    } else {
        registry.tree(ctx)
    };

    let selected: Vec<Item> = if all {
        tree.iter()
            .flat_map(|r| r.reclaimable_leaves())
            .cloned()
            .collect()
    } else {
        if ids.is_empty() {
            bail!("pass item ids, or use --all");
        }
        let mut found = Vec::new();
        for id in &ids {
            match crate::core::find_in_forest(&tree, id) {
                Some(item) => found.push(item.clone()),
                None => bail!("item '{id}' not found (run `maclean scan`)"),
            }
        }
        found
    };
    if selected.is_empty() {
        bail!("nothing to reclaim");
    }

    let reclaim_ctx = ReclaimContext::from_scan(ctx, !yes, yes && std::io::stdin().is_terminal());

    let mut results = Vec::new();
    for item in &selected {
        match registry.reclaim(item, &reclaim_ctx) {
            Ok(rs) => results.extend(rs),
            Err(err) => {
                if json {
                    bail!("{err}");
                }
                eprintln!("error: {err}");
            }
        }
    }
    if json {
        print_json(&results)?;
        return Ok(());
    }
    for result in &results {
        let prefix = if result.dry_run { "dry-run" } else { "done" };
        println!(
            "[{prefix}] {}  {}  {}",
            result.item_id,
            format_bytes(result.bytes_reclaimed),
            result.message
        );
    }
    if !yes {
        eprintln!("Re-run with --yes to apply.");
    }
    Ok(())
}

fn cmd_modules(registry: &Registry, ctx: &ScanContext, json: bool) -> Result<()> {
    #[derive(Serialize)]
    struct Info {
        id: &'static str,
        name: &'static str,
        description: &'static str,
        relevant: bool,
        reason: String,
    }
    let infos: Vec<Info> = registry
        .iter()
        .map(|m| {
            let r = m.relevance(ctx);
            Info {
                id: m.id(),
                name: m.name(),
                description: m.description(),
                relevant: r.relevant,
                reason: r.reason,
            }
        })
        .collect();
    if json {
        print_json(&infos)?;
        return Ok(());
    }
    for m in &infos {
        println!(
            "{:<14} {:<5} {}\n               {}",
            m.id,
            if m.relevant { "yes" } else { "no" },
            m.name,
            m.reason
        );
    }
    Ok(())
}

fn cmd_module_info(registry: &Registry, ctx: &ScanContext, id: &str, json: bool) -> Result<()> {
    let Some(info) = registry.info(id, ctx) else {
        bail!("unknown module '{id}' (see `maclean modules`)");
    };
    if json {
        print_json(&info)?;
        return Ok(());
    }
    let relevance = registry
        .get(id)
        .map(|m| m.relevance(ctx))
        .expect("module exists");
    println!("{} ({})\n{}\n", info.name, info.id, info.description);
    println!(
        "Relevant here: {}\n  {}\n",
        if relevance.relevant { "yes" } else { "no" },
        relevance.reason
    );
    if !info.finds.is_empty() {
        println!("What it looks for:");
        for line in &info.finds {
            println!("  - {line}");
        }
        println!();
    }
    if !info.effects.is_empty() {
        println!("What cleaning does:");
        for line in &info.effects {
            println!("  - {line}");
        }
        println!();
    }
    if !info.locations.is_empty() {
        println!("Where it looks:");
        for path in &info.locations {
            println!("  {}", path.display());
        }
    }
    Ok(())
}

fn cmd_schedule(cmd: ScheduleCmd, json: bool) -> Result<()> {
    let scheduler = schedule::current();
    match cmd {
        ScheduleCmd::List => {
            let jobs = scheduler.list()?;
            if json {
                print_json(&jobs)?;
                return Ok(());
            }
            if jobs.is_empty() {
                println!("No scheduled maclean jobs.");
                return Ok(());
            }
            for job in jobs {
                println!(
                    "{:<32}  {:<16}  {}",
                    job.item_id,
                    job.every.display(),
                    job.command.join(" ")
                );
            }
        }
        ScheduleCmd::Add { item_id, every } => {
            let every = schedule::parse_every(&every)?;
            let job = ScheduledJob {
                item_id: item_id.clone(),
                every,
                command: schedule::maclean_command(&item_id)?,
                schema: schedule::JOB_SCHEMA,
            };
            scheduler.add(&job)?;
            if json {
                print_json(&job)?;
            } else {
                println!(
                    "scheduled {} ({})\n  {}",
                    job.item_id,
                    job.every.display(),
                    job.command.join(" ")
                );
            }
        }
        ScheduleCmd::Remove { item_id } => {
            scheduler.remove(&item_id)?;
            if json {
                print_json(&serde_json::json!({ "removed": item_id }))?;
            } else {
                println!("removed schedule for {item_id}");
            }
        }
    }
    Ok(())
}

fn cmd_uninstall(purge_data: bool) -> Result<()> {
    let removed = schedule::current().purge()?;
    if removed.is_empty() {
        println!("No maclean launchd jobs were installed.");
    } else {
        println!(
            "Removed {}.",
            crate::core::plural(removed.len(), "scheduled job")
        );
        for job in &removed {
            println!("  {}", job.item_id);
        }
    }

    if purge_data {
        let config = crate::core::config_path();
        if let Some(dir) = config.parent() {
            if dir.file_name().and_then(|s| s.to_str()) == Some("maclean") && dir.is_dir() {
                std::fs::remove_dir_all(dir)?;
                println!("Removed {}", dir.display());
            }
        }
        if let Some(home) = dirs::home_dir() {
            let logs = home.join("Library/Logs/maclean");
            if logs.is_dir() {
                std::fs::remove_dir_all(&logs)?;
                println!("Removed {}", logs.display());
            }
        }
    } else {
        println!("Left in place: {}", crate::core::config_path().display());
        if let Some(home) = dirs::home_dir() {
            println!(
                "             {}",
                home.join("Library/Logs/maclean").display()
            );
        }
        println!("Pass --purge-data if you want those gone too.");
    }

    println!();
    println!("The binary is still there. Finish with however you installed it:");
    println!("  cargo uninstall maclean");
    Ok(())
}

fn cmd_config(
    registry: &Registry,
    ctx: &mut ScanContext,
    cmd: Option<ConfigCmd>,
    init_flag: bool,
    json: bool,
) -> Result<()> {
    let cmd = match (cmd, init_flag) {
        (Some(ConfigCmd::Init), _) | (None, true) => Some(ConfigCmd::Init),
        (Some(_), true) => bail!("pass either a config subcommand or --init, not both"),
        (cmd, false) => cmd,
    };
    match cmd {
        None => cmd_config_show(registry, ctx, json),
        Some(ConfigCmd::Init) => cmd_config_init(registry, ctx, json),
        Some(ConfigCmd::Validate) => cmd_config_validate(registry, ctx, json),
        Some(ConfigCmd::Enable { module }) => {
            cmd_config_set(registry, ctx, json, |cfg| cfg.set_enabled(&module, true))
        }
        Some(ConfigCmd::Disable { module }) => {
            cmd_config_set(registry, ctx, json, |cfg| cfg.set_enabled(&module, false))
        }
        Some(ConfigCmd::Path { module, key, value }) => {
            cmd_config_set(registry, ctx, json, |cfg| {
                cfg.set_path(&module, &key, value)
            })
        }
        Some(ConfigCmd::Roots { module, dirs }) => {
            cmd_config_set(registry, ctx, json, |cfg| cfg.set_roots(&module, dirs))
        }
    }
}

fn cmd_config_show(registry: &Registry, ctx: &ScanContext, json: bool) -> Result<()> {
    if json {
        print_json(&serde_json::json!({
            "path": ctx.config_path,
            "exists": ctx.config_path.is_file(),
            "config": ctx.config,
        }))?;
        return Ok(());
    }
    println!("config  {}", ctx.config_path.display());
    if ctx.config_path.is_file() {
        println!("status  present");
    } else {
        println!("status  missing (defaults) — `maclean config init` writes a starter file");
    }
    println!();
    for module in registry.iter() {
        let enabled = if ctx.module_enabled(module.id()) {
            "yes"
        } else {
            "no"
        };
        println!("{:<14} enabled {enabled}", module.id());
        if module.searches() {
            for root in ctx.roots_for(module.id()) {
                println!("               search  {}", root.display());
            }
        }
        for (key, rel) in module.paths() {
            let path = ctx.path(module.id(), key, rel);
            println!("               {key:<7} {}", path.display());
        }
        println!();
    }
    if !ctx.cli_roots.is_empty() {
        println!("this run (--roots)");
        for root in &ctx.cli_roots {
            println!("               {}", root.display());
        }
    }
    Ok(())
}

fn cmd_config_init(registry: &Registry, ctx: &ScanContext, json: bool) -> Result<()> {
    let path = &ctx.config_path;
    if path.is_file() {
        bail!(
            "{} already exists — edit it, or pass --config",
            path.display()
        );
    }
    let modules: Vec<&dyn crate::core::Module> = registry.iter().collect();
    let cfg = crate::core::AppConfig::populated(&modules);
    cfg.validate_or_err(path, &registry.specs(), &ctx.home)?;
    let body = toml::to_string_pretty(&cfg)?;
    let text = format!(
        "# maclean — per-user config\n\
         # Paths are relative to your home directory unless they start with / or ~/.\n\
         # Search modules (cargo, node) use ~ when roots is omitted or empty.\n\
         # A present but invalid file is rejected on startup.\n\
         # `maclean --roots DIR` is added on top of a module's search folders for that run.\n\n\
         {body}"
    );
    cfg.save(path)?;
    // save() writes TOML without the header; rewrite with comments.
    std::fs::write(path, text)?;
    if json {
        print_json(&serde_json::json!({ "wrote": path }))?;
    } else {
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn cmd_config_validate(registry: &Registry, ctx: &ScanContext, json: bool) -> Result<()> {
    ctx.config
        .validate_or_err(&ctx.config_path, &registry.specs(), &ctx.home)?;
    if json {
        print_json(&serde_json::json!({
            "path": ctx.config_path,
            "ok": true,
        }))?;
    } else if ctx.config_path.is_file() {
        println!("ok  {}", ctx.config_path.display());
    } else {
        println!(
            "ok  {} (missing — in-memory defaults)",
            ctx.config_path.display()
        );
    }
    Ok(())
}

fn cmd_config_set(
    registry: &Registry,
    ctx: &mut ScanContext,
    json: bool,
    edit: impl FnOnce(&mut crate::core::AppConfig),
) -> Result<()> {
    edit(&mut ctx.config);
    ctx.config
        .validate_or_err(&ctx.config_path, &registry.specs(), &ctx.home)?;
    ctx.config.save(&ctx.config_path)?;
    if json {
        print_json(&serde_json::json!({
            "path": ctx.config_path,
            "config": ctx.config,
        }))?;
    } else {
        println!("wrote {}", ctx.config_path.display());
    }
    Ok(())
}

#[derive(Serialize)]
struct ScanReport<'a> {
    disk: Option<crate::core::DiskUsage>,
    tree: &'a [Item],
    modules: &'a [ModuleScan],
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

fn truncate_middle(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    if max < 8 {
        return format!("{}…", &s[..max.saturating_sub(1)]);
    }
    let tail = max / 2;
    let head = max.saturating_sub(tail + 1);
    format!("{}…{}", &s[..head], &s[s.len() - tail..])
}
