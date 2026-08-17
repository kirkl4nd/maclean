use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

use super::config::{self, AppConfig, ConfigError, ModuleSpec};
use super::issue::{Privilege, ReclaimError, Relevance, ScanIssue};
use super::item::Item;

/// Per-scan configuration shared with every module.
#[derive(Debug, Clone)]
pub struct ScanContext {
    pub home: PathBuf,
    /// Extra search folders from `--roots` this run. Added on top of each
    /// searching module's configured roots (default: the home directory).
    pub cli_roots: Vec<PathBuf>,
    pub cancel: Arc<AtomicBool>,
    /// Program names modules have declared they may run. Empty until the
    /// registry fills it in — the core has no built-in tool list.
    pub allowed_programs: Vec<&'static str>,
    /// Extra PATH entries modules have declared (e.g. an app bundle's bin).
    pub path_dirs: Vec<&'static str>,
    pub config: AppConfig,
    /// Where config was read from (or would be written by `maclean config init`).
    pub config_path: PathBuf,
}

impl ScanContext {
    /// Load and validate. A missing file is defaults. A present but invalid
    /// file is an error — nothing falls back silently.
    pub fn load(config_file: Option<&Path>, specs: &[ModuleSpec]) -> Result<Self, ConfigError> {
        let home = dirs::home_dir().ok_or(ConfigError::NoHome)?;
        let (config, config_path) = AppConfig::load(config_file)?;
        config.validate_or_err(&config_path, specs, &home)?;
        Ok(Self {
            home,
            cli_roots: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            allowed_programs: Vec::new(),
            path_dirs: Vec::new(),
            config,
            config_path,
        })
    }

    /// Isolated context for tests. No config file; search modules use `home`.
    pub fn for_home(home: PathBuf) -> Self {
        Self {
            home,
            cli_roots: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            allowed_programs: Vec::new(),
            path_dirs: Vec::new(),
            config: AppConfig::default(),
            config_path: PathBuf::from("/dev/null"),
        }
    }

    /// Named location for a module: user override, else a home-relative default.
    pub fn path(&self, module: &str, key: &str, default_rel: &str) -> PathBuf {
        match self.config.path_override(module, key) {
            Some(raw) => config::expand(raw, &self.home),
            None => config::expand(default_rel, &self.home),
        }
    }

    /// Search folders for a module that walks a tree. Configured roots, or
    /// the home directory when none are set, plus `--roots` for this run.
    pub fn roots_for(&self, module: &str) -> Vec<PathBuf> {
        let mut roots = self.config.search_roots(module, &self.home);
        for extra in &self.cli_roots {
            if !roots.contains(extra) {
                roots.push(extra.clone());
            }
        }
        roots
    }

    /// Paths a delete is allowed to touch: home, every configured module
    /// search root, and `--roots` for this run.
    pub fn allowed_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.home.clone()];
        for settings in self.config.modules.values() {
            for raw in &settings.roots {
                let path = config::expand(raw, &self.home);
                if !roots.contains(&path) {
                    roots.push(path);
                }
            }
        }
        for extra in &self.cli_roots {
            if !roots.contains(extra) {
                roots.push(extra.clone());
            }
        }
        roots
    }

    pub fn module_enabled(&self, id: &str) -> bool {
        self.config.module_enabled(id)
    }

    pub fn add_roots<I, P>(&mut self, roots: I) -> Result<(), ConfigError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut issues = Vec::new();
        for root in roots {
            let path = config::expand(&root.as_ref().to_string_lossy(), &self.home);
            if config::is_forbidden_root(&path) {
                issues.push(format!("search root {} is not allowed", path.display()));
                continue;
            }
            if !self.cli_roots.contains(&path) {
                self.cli_roots.push(path);
            }
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid {
                path: self.config_path.clone(),
                issues,
            })
        }
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// A fresh context with a new cancel token, for a rescan.
    pub fn restarted(&self) -> Self {
        Self {
            home: self.home.clone(),
            cli_roots: self.cli_roots.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
            allowed_programs: self.allowed_programs.clone(),
            path_dirs: self.path_dirs.clone(),
            config: self.config.clone(),
            config_path: self.config_path.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReclaimContext {
    pub dry_run: bool,
    /// User confirmed a one-shot sudo for *admin-marked* actions this turn.
    /// Never elevates anything else.
    pub allow_admin: bool,
    /// Paths a delete is allowed to touch (home + extra scan roots).
    pub allowed_roots: Vec<PathBuf>,
    pub allowed_programs: Vec<&'static str>,
    pub path_dirs: Vec<&'static str>,
}

impl ReclaimContext {
    pub fn from_scan(ctx: &ScanContext, dry_run: bool, allow_admin: bool) -> Self {
        Self {
            dry_run,
            allow_admin,
            allowed_roots: ctx.allowed_roots(),
            allowed_programs: ctx.allowed_programs.clone(),
            path_dirs: ctx.path_dirs.clone(),
        }
    }

    pub fn dry(ctx: &ScanContext) -> Self {
        Self::from_scan(ctx, true, false)
    }

    pub fn apply(ctx: &ScanContext, allow_admin: bool) -> Self {
        Self::from_scan(ctx, false, allow_admin)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReclaimResult {
    pub item_id: String,
    pub bytes_reclaimed: u64,
    pub message: String,
    pub dry_run: bool,
}

impl ReclaimResult {
    pub fn ok(
        item_id: impl Into<String>,
        bytes: u64,
        message: impl Into<String>,
        dry_run: bool,
    ) -> Self {
        Self {
            item_id: item_id.into(),
            bytes_reclaimed: bytes,
            message: message.into(),
            dry_run,
        }
    }
}

/// Everything a UI can say about a module. Authored by the module; the core
/// only renders it.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    /// What this module looks for, in plain language.
    pub finds: Vec<String>,
    /// What happens when you clean it.
    pub effects: Vec<String>,
    /// Where it looks on disk.
    pub locations: Vec<PathBuf>,
}

impl ModuleInfo {
    pub fn new(id: &str, name: &str, description: &str) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            finds: Vec::new(),
            effects: Vec::new(),
            locations: Vec::new(),
        }
    }

    pub fn finds(mut self, line: impl Into<String>) -> Self {
        self.finds.push(line.into());
        self
    }

    pub fn effect(mut self, line: impl Into<String>) -> Self {
        self.effects.push(line.into());
        self
    }

    pub fn location(mut self, path: PathBuf) -> Self {
        self.locations.push(path);
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleScan {
    pub module_id: String,
    pub module_name: String,
    pub relevance: Relevance,
    pub items: Vec<Item>,
    pub issues: Vec<ScanIssue>,
}

impl ModuleScan {
    pub fn new(id: impl Into<String>, name: impl Into<String>, relevance: Relevance) -> Self {
        Self {
            module_id: id.into(),
            module_name: name.into(),
            relevance,
            items: Vec::new(),
            issues: Vec::new(),
        }
    }

    pub fn bytes(&self) -> u64 {
        self.items.iter().map(|i| i.bytes).sum()
    }

    /// Root node for the main tree, or None if this module should stay off it.
    /// Issues are kept on the root (details / CLI) rather than as fake children
    /// — the tree only lists things you can select.
    pub fn tree_root(&self) -> Option<Item> {
        if !self.relevance.relevant {
            return None;
        }
        if self.items.is_empty() && self.issues.is_empty() {
            return None;
        }
        let mut root = Item::new(&self.module_id, &self.module_id, &self.module_name)
            .with_summary(self.relevance.reason.clone())
            .with_safety(crate::core::Safety::Info)
            .with_children(self.items.clone());
        for issue in &self.issues {
            root = root.with_issue(issue.clone());
        }
        if root.worth_showing() {
            Some(root)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub enum ScanEvent {
    Started {
        module_id: String,
        module_name: String,
    },
    Finished(ModuleScan),
    Done,
}

/// Feature plugin: detect waste and reclaim it.
///
/// The program core never special-cases a module. Call order is
/// [`Self::relevance`] (cheap) then [`Self::scan`], which receives the
/// relevance it already computed so nothing is measured twice.
pub trait Module: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;

    fn info(&self, _ctx: &ScanContext) -> ModuleInfo {
        ModuleInfo::new(self.id(), self.name(), self.description())
    }

    /// Programs this module may ask the core to run. Empty if it only
    /// deletes files it already reported. The core has no other tool list.
    fn programs(&self) -> &'static [&'static str] {
        &[]
    }

    /// Directories to prepend to PATH when running this module's programs.
    fn path_dirs(&self) -> &'static [&'static str] {
        &[]
    }

    /// Named locations this module looks at, as paths relative to the home
    /// directory (or `~/…`). Config may override any of them. These must not
    /// contain a username or a machine-specific absolute path.
    fn paths(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }

    /// Walks a tree looking for project files (Cargo.toml, node_modules, …).
    /// Those modules search the home directory unless config sets `roots`.
    fn searches(&self) -> bool {
        false
    }

    fn relevance(&self, ctx: &ScanContext) -> Relevance;

    fn scan(&self, ctx: &ScanContext, relevance: Relevance) -> ModuleScan;

    fn reclaim(&self, item: &Item, ctx: &ReclaimContext) -> Result<ReclaimResult, ReclaimError>;
}

pub fn reclaim_node(
    module: &dyn Module,
    item: &Item,
    ctx: &ReclaimContext,
) -> Result<Vec<ReclaimResult>, ReclaimError> {
    if item.privilege == Privilege::Admin && !ctx.allow_admin && !ctx.dry_run {
        return Err(ReclaimError::from_issue(
            &item.id,
            crate::core::ScanIssue::needs_admin(format!(
                "'{}' needs a one-shot administrator password",
                item.title
            )),
        ));
    }
    if item.clean_whole || item.children.is_empty() {
        if !item.reclaimable {
            return Ok(Vec::new());
        }
        return Ok(vec![module.reclaim(item, ctx)?]);
    }
    let mut out = Vec::new();
    for child in &item.children {
        out.extend(reclaim_node(module, child, ctx)?);
    }
    Ok(out)
}

pub fn find_in_forest<'a>(roots: &'a [Item], id: &str) -> Option<&'a Item> {
    roots.iter().find_map(|r| r.find(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_for_defaults_to_home_and_appends_cli() {
        let mut ctx = ScanContext::for_home(PathBuf::from("/Users/example"));
        assert_eq!(
            ctx.roots_for("cargo"),
            vec![PathBuf::from("/Users/example")]
        );
        ctx.add_roots(["/Volumes/Work"]).unwrap();
        assert_eq!(
            ctx.roots_for("cargo"),
            vec![
                PathBuf::from("/Users/example"),
                PathBuf::from("/Volumes/Work")
            ]
        );
    }

    #[test]
    fn cli_system_roots_are_rejected() {
        let mut ctx = ScanContext::for_home(PathBuf::from("/Users/example"));
        assert!(ctx.add_roots(["/"]).is_err());
        assert!(ctx.cli_roots.is_empty());
    }
}
