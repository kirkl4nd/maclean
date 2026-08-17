use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{Result, bail};

use super::config::ModuleSpec;
use super::module::{ModuleInfo, ModuleScan, ScanEvent, reclaim_node};
use super::{Item, Module, ReclaimContext, ReclaimError, ReclaimResult, Relevance, ScanContext};

/// Relevance is computed once and handed to `scan`, so a module never walks
/// the same directories twice.
fn scan_one(module: &dyn Module, ctx: &ScanContext) -> ModuleScan {
    if !ctx.module_enabled(module.id()) {
        return ModuleScan::new(
            module.id(),
            module.name(),
            Relevance::no("disabled in config"),
        );
    }
    let relevance = module.relevance(ctx);
    if !relevance.relevant || ctx.cancelled() {
        return ModuleScan::new(module.id(), module.name(), relevance);
    }
    module.scan(ctx, relevance)
}

pub struct Registry {
    modules: Vec<Box<dyn Module>>,
}

impl Registry {
    pub fn new(modules: Vec<Box<dyn Module>>) -> Self {
        Self { modules }
    }

    /// Copy declared programs and PATH dirs onto a scan context. The core
    /// never hard-codes which tools exist.
    pub fn bind(&self, ctx: &mut ScanContext) {
        ctx.allowed_programs = self.programs();
        ctx.path_dirs = self.path_dirs();
    }

    pub fn programs(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for module in self.iter() {
            for program in module.programs() {
                if !out.contains(program) {
                    out.push(program);
                }
            }
        }
        out
    }

    pub fn path_dirs(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for module in self.iter() {
            for dir in module.path_dirs() {
                if !out.contains(dir) {
                    out.push(dir);
                }
            }
        }
        out
    }

    pub fn get(&self, id: &str) -> Option<&dyn Module> {
        self.modules
            .iter()
            .find(|m| m.id() == id)
            .map(|m| m.as_ref())
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn Module> {
        self.modules.iter().map(|m| m.as_ref())
    }

    pub fn specs(&self) -> Vec<ModuleSpec> {
        self.iter().map(ModuleSpec::from_module).collect()
    }

    /// Module-authored documentation, for UIs that want to explain a module.
    pub fn info(&self, id: &str, ctx: &ScanContext) -> Option<ModuleInfo> {
        self.get(id).map(|m| m.info(ctx))
    }

    /// Blocking scan used by the CLI. TUI should use [`Self::scan_parallel`].
    pub fn scan_all(&self, ctx: &ScanContext) -> Vec<ModuleScan> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.scan_parallel(ctx, tx);
        let mut scans = Vec::new();
        while let Ok(ev) = rx.recv() {
            match ev {
                ScanEvent::Finished(scan) => scans.push(scan),
                ScanEvent::Done => break,
                ScanEvent::Started { .. } => {}
            }
        }
        scans
    }

    pub fn scan_module(&self, id: &str, ctx: &ScanContext) -> Result<ModuleScan> {
        let Some(module) = self.get(id) else {
            bail!("unknown module '{id}'");
        };
        Ok(scan_one(module, ctx))
    }

    pub fn tree(&self, ctx: &ScanContext) -> Vec<Item> {
        let mut roots: Vec<Item> = self
            .scan_all(ctx)
            .iter()
            .filter_map(ModuleScan::tree_root)
            .collect();
        roots.sort_by(|a, b| b.bytes.cmp(&a.bytes));
        roots
    }

    /// Run each module on its own thread. The caller (TUI) stays responsive.
    pub fn scan_parallel(&self, ctx: &ScanContext, tx: Sender<ScanEvent>) {
        thread::scope(|s| {
            for module in self.iter() {
                let tx = tx.clone();
                let ctx = ctx.clone();
                s.spawn(move || {
                    if ctx.cancelled() {
                        return;
                    }
                    let _ = tx.send(ScanEvent::Started {
                        module_id: module.id().into(),
                        module_name: module.name().into(),
                    });
                    let scan = scan_one(module, &ctx);
                    let _ = tx.send(ScanEvent::Finished(scan));
                });
            }
        });
        let _ = tx.send(ScanEvent::Done);
    }

    pub fn reclaim(
        &self,
        item: &Item,
        ctx: &ReclaimContext,
    ) -> Result<Vec<ReclaimResult>, ReclaimError> {
        let Some(module) = self.get(&item.module) else {
            return Err(ReclaimError::new(
                &item.id,
                super::IssueKind::Unavailable,
                format!("unknown module '{}'", item.module),
            ));
        };
        reclaim_node(module, item, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_knows_no_modules_or_tools() {
        let registry = Registry::new(Vec::new());
        assert!(registry.get("trash").is_none());
        assert!(registry.programs().is_empty());
        assert!(registry.path_dirs().is_empty());
    }
}
