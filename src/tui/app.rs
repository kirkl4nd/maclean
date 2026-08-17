use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::core::{
    DiskUsage, Item, ModuleInfo, ModuleScan, Privilege, ReclaimContext, Registry, Safety,
    ScanContext, ScanEvent, format_bytes, plural,
};
use crate::schedule::{self, ScheduledJob};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Scan,
    Tree,
    Details,
    Modules,
    Review,
    Working,
    Results,
    Jobs,
    Schedule,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ModuleState {
    Pending,
    Running,
    Skipped,
    Done,
}

pub struct ModuleStatus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub state: ModuleState,
    pub reason: String,
    pub bytes: u64,
    pub findings: usize,
    pub issues: usize,
    pub elapsed: Option<Duration>,
    started: Option<Instant>,
}

/// One line of the flattened tree. Built once per change, not once per frame:
/// re-walking and cloning the whole forest on every draw is what made the old
/// UI stutter.
pub struct Row {
    pub depth: usize,
    pub id: String,
    pub title: String,
    pub summary: String,
    pub bytes: u64,
    pub safety: Safety,
    pub privilege: Privilege,
    pub has_children: bool,
    pub expanded: bool,
    pub is_root: bool,
    /// Ids of the cleanable units under this row (itself, if it is one).
    /// Selection happens per unit; a row's checkbox is derived from these.
    units: Vec<String>,
}

/// Checkbox state of a row, derived from the units underneath it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Nothing here can be cleaned: no box at all.
    None,
    Empty,
    Partial,
    Full,
}

#[derive(Clone)]
pub enum ConfigRowKind {
    Enabled,
    Root { index: usize, virtual_default: bool },
    AddRoot,
    Path { key: String },
}

#[derive(Clone)]
pub struct ConfigRow {
    pub kind: ConfigRowKind,
    pub label: String,
    pub value: String,
}

pub struct Outcome {
    pub title: String,
    pub ok: bool,
    pub message: String,
    pub bytes: u64,
    pub hint: Option<String>,
}

enum CleanEvent {
    Result(Outcome),
    Done,
}

pub struct App {
    registry: Arc<Registry>,
    ctx: ScanContext,
    pub disk: Option<DiskUsage>,
    pub forest: Vec<Item>,
    pub modules: Vec<ModuleStatus>,
    pub scan_started: Instant,
    pub scan_elapsed: Option<Duration>,
    pub tick: usize,

    rows: Vec<Row>,
    pub expanded: HashSet<String>,
    selected: HashSet<String>,
    unit_bytes: HashMap<String, u64>,

    pub tree_state: ListState,
    pub module_state: ListState,
    pub review_state: ListState,
    pub results_state: ListState,
    pub jobs_state: ListState,
    pub schedule_state: ListState,

    pub screen: Screen,
    prev_screen: Screen,
    pub should_quit: bool,
    pub status: String,

    pub detail_item: Option<Item>,
    pub detail_info: Option<ModuleInfo>,
    pub config_module: Option<String>,
    pub config_rows: Vec<ConfigRow>,
    pub config_state: ListState,
    pub config_edit: Option<String>,
    pub review: Vec<Item>,
    pub outcomes: Vec<Outcome>,
    pub working_total: usize,
    pub working_started: Instant,
    pub needs_admin: bool,
    admin_go: bool,

    pub jobs: Vec<ScheduledJob>,
    pub schedule_choices: Vec<(&'static str, &'static str)>,
    pub schedule_item: Option<String>,

    scan_rx: Option<Receiver<ScanEvent>>,
    clean_rx: Option<Receiver<CleanEvent>>,
}

impl App {
    pub fn new(registry: Arc<Registry>, ctx: ScanContext, disk: Option<DiskUsage>) -> Self {
        let modules = registry
            .iter()
            .map(|m| ModuleStatus {
                id: m.id().into(),
                name: m.name().into(),
                description: m.description().into(),
                state: ModuleState::Pending,
                reason: String::new(),
                bytes: 0,
                findings: 0,
                issues: 0,
                elapsed: None,
                started: None,
            })
            .collect();

        let mut app = Self {
            registry,
            ctx,
            disk,
            forest: Vec::new(),
            modules,
            scan_started: Instant::now(),
            scan_elapsed: None,
            tick: 0,
            rows: Vec::new(),
            expanded: HashSet::new(),
            selected: HashSet::new(),
            unit_bytes: HashMap::new(),
            tree_state: ListState::default(),
            module_state: ListState::default(),
            review_state: ListState::default(),
            results_state: ListState::default(),
            jobs_state: ListState::default(),
            schedule_state: ListState::default(),
            screen: Screen::Scan,
            prev_screen: Screen::Tree,
            should_quit: false,
            status: String::new(),
            detail_item: None,
            detail_info: None,
            config_module: None,
            config_rows: Vec::new(),
            config_state: ListState::default(),
            config_edit: None,
            review: Vec::new(),
            outcomes: Vec::new(),
            working_total: 0,
            working_started: Instant::now(),
            needs_admin: false,
            admin_go: false,
            jobs: Vec::new(),
            schedule_choices: vec![
                ("1d", "Every day"),
                ("1w", "Every week"),
                ("2w", "Every 2 weeks"),
                ("4w", "Every 4 weeks"),
            ],
            schedule_item: None,
            scan_rx: None,
            clean_rx: None,
        };
        app.start_scan();
        app
    }

    pub fn start_scan(&mut self) {
        self.ctx
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.ctx = self.ctx.restarted();

        for m in &mut self.modules {
            m.state = ModuleState::Pending;
            m.reason.clear();
            m.bytes = 0;
            m.findings = 0;
            m.issues = 0;
            m.elapsed = None;
            m.started = None;
        }
        self.forest.clear();
        self.rows.clear();
        self.selected.clear();
        self.unit_bytes.clear();
        self.expanded.clear();
        self.outcomes.clear();
        self.scan_started = Instant::now();
        self.scan_elapsed = None;
        self.screen = Screen::Scan;
        self.status.clear();

        let (tx, rx) = mpsc::channel();
        self.scan_rx = Some(rx);
        let registry = self.registry.clone();
        let ctx = self.ctx.clone();
        thread::spawn(move || registry.scan_parallel(&ctx, tx));
    }

    pub fn cancel_scan(&mut self) {
        self.ctx
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Drain worker channels. Cheap and non-blocking; called once per frame.
    pub fn pump(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        self.pump_scan();
        self.pump_clean();
    }

    fn pump_scan(&mut self) {
        let Some(rx) = &self.scan_rx else { return };
        let mut finished = Vec::new();
        let mut done = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                ScanEvent::Started { module_id, .. } => {
                    if let Some(m) = self.modules.iter_mut().find(|m| m.id == module_id) {
                        m.state = ModuleState::Running;
                        m.started = Some(Instant::now());
                    }
                }
                ScanEvent::Finished(scan) => finished.push(scan),
                ScanEvent::Done => done = true,
            }
        }

        for scan in finished {
            self.absorb(scan);
        }
        if done {
            self.finish_scan();
        }
    }

    fn absorb(&mut self, scan: ModuleScan) {
        if let Some(m) = self.modules.iter_mut().find(|m| m.id == scan.module_id) {
            m.state = if scan.relevance.relevant {
                ModuleState::Done
            } else {
                ModuleState::Skipped
            };
            m.reason = scan.relevance.reason.clone();
            m.bytes = scan.bytes();
            m.findings = scan.items.len();
            m.issues = scan.issues.len();
            m.elapsed = m.started.map(|t| t.elapsed());
        }
        if let Some(root) = scan.tree_root() {
            self.forest.push(root);
        }
    }

    fn finish_scan(&mut self) {
        self.scan_rx = None;
        self.scan_elapsed = Some(self.scan_started.elapsed());
        self.forest.sort_by(|a, b| b.bytes.cmp(&a.bytes));
        for root in &self.forest {
            for unit in root.selection_leaves() {
                self.unit_bytes.insert(unit.id.clone(), unit.bytes);
            }
        }
        self.rebuild_rows();
        self.tree_state
            .select(if self.rows.is_empty() { None } else { Some(0) });
        let total: u64 = self.unit_bytes.values().copied().sum();
        let modules = self.rows.iter().filter(|r| r.is_root).count();
        self.status = if self.rows.is_empty() {
            if self.forest.is_empty() {
                "Nothing worth cleaning was found.".into()
            } else {
                "Nothing here can be selected. Press m to see what was found.".into()
            }
        } else {
            format!(
                "Found {} across {}",
                format_bytes(total),
                plural(modules, "module")
            )
        };
        self.screen = Screen::Tree;
    }

    fn pump_clean(&mut self) {
        let Some(rx) = &self.clean_rx else { return };
        let mut done = false;
        let mut new = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            match ev {
                CleanEvent::Result(outcome) => new.push(outcome),
                CleanEvent::Done => done = true,
            }
        }
        self.outcomes.extend(new);
        if done {
            self.clean_rx = None;
            self.finish_clean();
        }
    }

    fn finish_clean(&mut self) {
        let freed: u64 = self.outcomes.iter().filter(|o| o.ok).map(|o| o.bytes).sum();
        let failed = self.outcomes.iter().filter(|o| !o.ok).count();
        self.status = if failed == 0 {
            format!("Freed {}", format_bytes(freed))
        } else {
            format!(
                "Freed {} · {} could not be completed",
                format_bytes(freed),
                plural(failed, "action")
            )
        };
        self.selected.clear();
        self.review.clear();
        self.results_state.select(Some(0));
        self.screen = Screen::Results;
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn total_bytes(&self) -> u64 {
        self.unit_bytes.values().copied().sum()
    }

    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    pub fn selected_bytes(&self) -> u64 {
        self.selected
            .iter()
            .filter_map(|id| self.unit_bytes.get(id))
            .sum()
    }

    /// A row is checked when every unit under it is selected, partially
    /// checked when only some are.
    pub fn check(&self, row: &Row) -> Check {
        if row.units.is_empty() {
            return Check::None;
        }
        let selected = row
            .units
            .iter()
            .filter(|id| self.selected.contains(*id))
            .count();
        if selected == 0 {
            Check::Empty
        } else if selected == row.units.len() {
            Check::Full
        } else {
            Check::Partial
        }
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        for root in &self.forest {
            flatten(root, 0, true, &self.expanded, &mut rows);
        }
        self.rows = rows;
        if let Some(sel) = self.tree_state.selected() {
            if sel >= self.rows.len() {
                self.tree_state.select(self.rows.len().checked_sub(1));
            }
        }
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.tree_state.selected().and_then(|i| self.rows.get(i))
    }

    pub fn config_file_path(&self) -> &std::path::Path {
        &self.ctx.config_path
    }

    fn find(&self, id: &str) -> Option<&Item> {
        self.forest.iter().find_map(|r| r.find(id))
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            self.quit();
            return;
        }
        if self.config_edit.is_some() && self.screen == Screen::Details {
            self.keys_config_edit(key);
            return;
        }
        // Q quits from anywhere except screens where work is in flight.
        if matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q'))
            && self.screen != Screen::Working
        {
            self.quit();
            return;
        }
        match self.screen {
            Screen::Scan => self.keys_scan(key.code),
            Screen::Tree => self.keys_tree(key.code),
            Screen::Details => self.keys_details(key.code),
            Screen::Modules => self.keys_modules(key.code),
            Screen::Review => self.keys_review(key.code),
            Screen::Working => {}
            Screen::Results => self.keys_results(key.code),
            Screen::Jobs => self.keys_jobs(key.code),
            Screen::Schedule => self.keys_schedule(key.code),
            Screen::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter) {
                    self.screen = self.prev_screen;
                }
            }
        }
    }

    fn quit(&mut self) {
        self.cancel_scan();
        self.should_quit = true;
    }

    fn keys_scan(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.quit(),
            KeyCode::Char('?') => self.open_help(),
            _ => {}
        }
    }

    fn keys_tree(&mut self, code: KeyCode) {
        let n = self.rows.len();
        match code {
            KeyCode::Esc => self.quit(),
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Down | KeyCode::Char('j') => {
                self.status.clear();
                move_sel(&mut self.tree_state, n, 1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.status.clear();
                move_sel(&mut self.tree_state, n, -1);
            }
            KeyCode::Right | KeyCode::Char('l') => self.set_expanded(true),
            KeyCode::Left | KeyCode::Char('h') => self.set_expanded(false),
            KeyCode::Char(' ') => self.toggle_selection(),
            KeyCode::Char('*') => self.select_all(),
            KeyCode::Char('-') => self.deselect_all(),
            KeyCode::Enter => self.open_details(),
            KeyCode::Char('m') | KeyCode::Char('M') => self.open_modules(),
            KeyCode::Char('r') | KeyCode::Char('R') => self.start_scan(),
            KeyCode::Char('a') | KeyCode::Char('A') => self.open_review(),
            KeyCode::Char('s') | KeyCode::Char('S') => self.open_jobs(),
            _ => {}
        }
    }

    fn keys_details(&mut self, code: KeyCode) {
        if !self.config_rows.is_empty() {
            match code {
                KeyCode::Esc | KeyCode::Left => {
                    self.close_details();
                    return;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    move_sel(&mut self.config_state, self.config_rows.len(), 1);
                    return;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    move_sel(&mut self.config_state, self.config_rows.len(), -1);
                    return;
                }
                KeyCode::Char(' ') => {
                    self.toggle_config_enabled();
                    return;
                }
                KeyCode::Enter | KeyCode::Right => {
                    self.begin_config_edit();
                    return;
                }
                KeyCode::Char('+') => {
                    self.add_config_root();
                    return;
                }
                KeyCode::Char('-') => {
                    self.remove_config_root();
                    return;
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.start_scan();
                    return;
                }
                KeyCode::Char('?') => {
                    self.open_help();
                    return;
                }
                _ => return,
            }
        }
        match code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Left => self.close_details(),
            KeyCode::Char('?') => self.open_help(),
            _ => {}
        }
    }

    fn close_details(&mut self) {
        self.detail_item = None;
        self.detail_info = None;
        self.clear_config_panel();
        self.screen = self.prev_screen;
    }

    fn keys_modules(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('m') | KeyCode::Char('M') | KeyCode::Left => {
                self.screen = Screen::Tree
            }
            KeyCode::Down | KeyCode::Char('j') => {
                move_sel(&mut self.module_state, self.modules.len(), 1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                move_sel(&mut self.module_state, self.modules.len(), -1)
            }
            KeyCode::Enter | KeyCode::Right => self.open_module_details(),
            KeyCode::Char('r') | KeyCode::Char('R') => self.start_scan(),
            KeyCode::Char('?') => self.open_help(),
            _ => {}
        }
    }

    fn keys_review(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Left => {
                self.review.clear();
                self.screen = Screen::Tree;
                self.status = "Nothing was changed.".into();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                move_sel(&mut self.review_state, self.review.len(), 1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                move_sel(&mut self.review_state, self.review.len(), -1)
            }
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => self.start_clean(),
            _ => {}
        }
    }

    fn keys_results(&mut self, code: KeyCode) {
        match code {
            // Sizes on the old tree are stale the moment something is deleted,
            // so going back means scanning again.
            KeyCode::Esc | KeyCode::Enter => self.start_scan(),
            KeyCode::Down | KeyCode::Char('j') => {
                move_sel(&mut self.results_state, self.outcomes.len(), 1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                move_sel(&mut self.results_state, self.outcomes.len(), -1)
            }
            KeyCode::Char('r') | KeyCode::Char('R') => self.start_scan(),
            _ => {}
        }
    }

    fn keys_jobs(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Left => {
                self.screen = Screen::Tree;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                move_sel(&mut self.jobs_state, self.jobs.len(), 1)
            }
            KeyCode::Up | KeyCode::Char('k') => move_sel(&mut self.jobs_state, self.jobs.len(), -1),
            KeyCode::Enter => {
                if self.jobs.is_empty() {
                    self.begin_schedule_from_tree();
                } else {
                    self.edit_selected_job();
                }
            }
            KeyCode::Char('+') => self.begin_schedule_from_tree(),
            KeyCode::Char('-') | KeyCode::Delete | KeyCode::Backspace => self.remove_selected_job(),
            KeyCode::Char('?') => self.open_help(),
            _ => {}
        }
    }

    fn keys_schedule(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('n') => {
                self.schedule_item = None;
                self.screen = Screen::Jobs;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                move_sel(&mut self.schedule_state, self.schedule_choices.len(), 1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                move_sel(&mut self.schedule_state, self.schedule_choices.len(), -1)
            }
            KeyCode::Enter | KeyCode::Char('y') => self.apply_schedule(),
            _ => {}
        }
    }

    fn open_help(&mut self) {
        if self.screen != Screen::Help {
            self.prev_screen = self.screen;
            self.screen = Screen::Help;
        }
    }

    fn set_expanded(&mut self, open: bool) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if !row.has_children {
            return;
        }
        let id = row.id.clone();
        let keep = id.clone();
        let changed = if open {
            self.expanded.insert(id)
        } else {
            self.expanded.remove(&id)
        };
        if changed {
            self.rebuild_rows();
            if let Some(idx) = self.rows.iter().position(|r| r.id == keep) {
                self.tree_state.select(Some(idx));
            }
        }
    }

    /// Toggling a parent toggles everything under it; unticking one child
    /// leaves the parent partially ticked.
    fn toggle_selection(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.units.is_empty() {
            self.status = format!("'{}' is shown for information only.", row.title);
            return;
        }
        let units = row.units.clone();
        let all_on = units.iter().all(|id| self.selected.contains(id));
        for id in units {
            if all_on {
                self.selected.remove(&id);
            } else {
                self.selected.insert(id);
            }
        }
        self.status = format!(
            "{} selected · {}",
            self.selected.len(),
            format_bytes(self.selected_bytes())
        );
    }

    fn select_all(&mut self) {
        let mut n = 0;
        for root in &self.forest {
            for unit in root.selection_leaves() {
                self.selected.insert(unit.id.clone());
                n += 1;
            }
        }
        if n == 0 {
            self.status = "Nothing here can be selected.".into();
            return;
        }
        self.status = format!(
            "{} selected · {}",
            self.selected.len(),
            format_bytes(self.selected_bytes())
        );
    }

    fn deselect_all(&mut self) {
        if self.selected.is_empty() {
            self.status = "Nothing is selected.".into();
            return;
        }
        self.selected.clear();
        self.status = "Nothing selected.".into();
    }

    fn open_details(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let (id, is_root) = (row.id.clone(), row.is_root);
        self.detail_item = self.find(&id).cloned();
        self.detail_info = if is_root {
            self.registry.info(&id, &self.ctx)
        } else {
            None
        };
        if is_root {
            self.load_config_panel(&id);
        } else {
            self.clear_config_panel();
        }
        if self.detail_item.is_some() || self.detail_info.is_some() {
            self.prev_screen = Screen::Tree;
            self.screen = Screen::Details;
        }
    }

    fn open_modules(&mut self) {
        if self.module_state.selected().is_none() {
            self.module_state.select(Some(0));
        }
        self.screen = Screen::Modules;
    }

    fn open_module_details(&mut self) {
        let Some(idx) = self.module_state.selected() else {
            return;
        };
        let Some(status) = self.modules.get(idx) else {
            return;
        };
        let id = status.id.clone();
        self.detail_info = self.registry.info(&id, &self.ctx);
        self.detail_item = self.find(&id).cloned();
        self.load_config_panel(&id);
        self.prev_screen = Screen::Modules;
        self.screen = Screen::Details;
    }

    fn load_config_panel(&mut self, module_id: &str) {
        self.config_module = Some(module_id.to_string());
        self.config_edit = None;
        self.rebuild_config_rows();
        if self.config_state.selected().is_none() {
            self.config_state.select(Some(0));
        } else if let Some(sel) = self.config_state.selected() {
            if sel >= self.config_rows.len() {
                self.config_state
                    .select(self.config_rows.len().checked_sub(1));
            }
        }
    }

    fn clear_config_panel(&mut self) {
        self.config_module = None;
        self.config_rows.clear();
        self.config_edit = None;
        self.config_state.select(None);
    }

    fn rebuild_config_rows(&mut self) {
        let Some(id) = self.config_module.clone() else {
            self.config_rows.clear();
            return;
        };
        let Some(module) = self.registry.get(&id) else {
            self.config_rows.clear();
            return;
        };
        let mut rows = vec![ConfigRow {
            kind: ConfigRowKind::Enabled,
            label: "enabled".into(),
            value: if self.ctx.module_enabled(&id) {
                "yes".into()
            } else {
                "no".into()
            },
        }];
        if module.searches() {
            let listed = self
                .ctx
                .config
                .modules
                .get(&id)
                .map(|m| m.roots.clone())
                .unwrap_or_default();
            if listed.is_empty() {
                rows.push(ConfigRow {
                    kind: ConfigRowKind::Root {
                        index: 0,
                        virtual_default: true,
                    },
                    label: "search".into(),
                    value: "~".into(),
                });
            } else {
                for (i, root) in listed.iter().enumerate() {
                    rows.push(ConfigRow {
                        kind: ConfigRowKind::Root {
                            index: i,
                            virtual_default: false,
                        },
                        label: if i == 0 {
                            "search".into()
                        } else {
                            String::new()
                        },
                        value: root.clone(),
                    });
                }
            }
            rows.push(ConfigRow {
                kind: ConfigRowKind::AddRoot,
                label: String::new(),
                value: "add a search folder".into(),
            });
        }
        for (key, rel) in module.paths() {
            let value = self
                .ctx
                .config
                .path_override(&id, key)
                .map(str::to_string)
                .unwrap_or_else(|| format!("~/{rel}"));
            rows.push(ConfigRow {
                kind: ConfigRowKind::Path {
                    key: key.to_string(),
                },
                label: key.to_string(),
                value,
            });
        }
        self.config_rows = rows;
    }

    fn selected_config_row(&self) -> Option<&ConfigRow> {
        self.config_state
            .selected()
            .and_then(|i| self.config_rows.get(i))
    }

    fn persist_config(&mut self) -> bool {
        let specs = self.registry.specs();
        if let Err(err) =
            self.ctx
                .config
                .validate_or_err(&self.ctx.config_path, &specs, &self.ctx.home)
        {
            self.status = err.to_string().replace('\n', " ");
            return false;
        }
        if let Err(err) = self.ctx.config.save(&self.ctx.config_path) {
            self.status = err.to_string();
            return false;
        }
        if let Some(id) = self.config_module.clone() {
            self.detail_info = self.registry.info(&id, &self.ctx);
        }
        self.rebuild_config_rows();
        self.status = format!(
            "saved {} — press r to scan again",
            self.ctx.config_path.display()
        );
        true
    }

    fn apply_config(&mut self, edit: impl FnOnce(&mut crate::core::AppConfig)) -> bool {
        let snapshot = self.ctx.config.clone();
        edit(&mut self.ctx.config);
        if self.persist_config() {
            true
        } else {
            self.ctx.config = snapshot;
            self.rebuild_config_rows();
            false
        }
    }

    fn toggle_config_enabled(&mut self) {
        let Some(id) = self.config_module.clone() else {
            return;
        };
        if !matches!(
            self.selected_config_row().map(|r| &r.kind),
            Some(ConfigRowKind::Enabled)
        ) {
            return;
        }
        let next = !self.ctx.module_enabled(&id);
        self.apply_config(|cfg| cfg.set_enabled(&id, next));
    }

    fn begin_config_edit(&mut self) {
        let Some(row) = self.selected_config_row() else {
            return;
        };
        match &row.kind {
            ConfigRowKind::Enabled => self.toggle_config_enabled(),
            ConfigRowKind::AddRoot => self.add_config_root(),
            ConfigRowKind::Root { .. } | ConfigRowKind::Path { .. } => {
                self.config_edit = Some(row.value.clone());
            }
        }
    }

    fn keys_config_edit(&mut self, key: KeyEvent) {
        let Some(buf) = self.config_edit.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.config_edit = None;
                self.status.clear();
            }
            KeyCode::Enter => self.commit_config_edit(),
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                buf.push(c);
            }
            _ => {}
        }
    }

    fn commit_config_edit(&mut self) {
        let Some(value) = self.config_edit.take() else {
            return;
        };
        let Some(id) = self.config_module.clone() else {
            return;
        };
        let Some(idx) = self.config_state.selected() else {
            return;
        };
        let Some(kind) = self.config_rows.get(idx).map(|r| r.kind.clone()) else {
            return;
        };
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() {
            self.status = "value cannot be empty".into();
            self.config_edit = Some(value);
            return;
        }
        let ok = match kind {
            ConfigRowKind::Root {
                index,
                virtual_default,
            } => {
                let mut roots = self
                    .ctx
                    .config
                    .modules
                    .get(&id)
                    .map(|m| m.roots.clone())
                    .unwrap_or_default();
                if virtual_default {
                    roots = vec![trimmed.clone()];
                } else if let Some(slot) = roots.get_mut(index) {
                    *slot = trimmed.clone();
                } else {
                    roots.push(trimmed.clone());
                }
                self.apply_config(|cfg| cfg.set_roots(&id, roots))
            }
            ConfigRowKind::Path { key } => {
                self.apply_config(|cfg| cfg.set_path(&id, &key, trimmed.clone()))
            }
            ConfigRowKind::Enabled | ConfigRowKind::AddRoot => true,
        };
        if !ok {
            self.config_edit = Some(value);
        }
    }

    fn add_config_root(&mut self) {
        let Some(id) = self.config_module.clone() else {
            return;
        };
        let Some(module) = self.registry.get(&id) else {
            return;
        };
        if !module.searches() {
            return;
        }
        let mut roots = self
            .ctx
            .config
            .modules
            .get(&id)
            .map(|m| m.roots.clone())
            .unwrap_or_default();
        if roots.is_empty() {
            roots.push("~".into());
        }
        roots.push("~".into());
        if self.apply_config(|cfg| cfg.set_roots(&id, roots)) {
            let last = self
                .config_rows
                .iter()
                .rposition(|r| matches!(r.kind, ConfigRowKind::Root { .. }));
            self.config_state.select(last);
            self.begin_config_edit();
        }
    }

    fn remove_config_root(&mut self) {
        let Some(id) = self.config_module.clone() else {
            return;
        };
        let Some(idx) = self.config_state.selected() else {
            return;
        };
        let Some(ConfigRowKind::Root {
            index,
            virtual_default,
        }) = self.config_rows.get(idx).map(|r| r.kind.clone())
        else {
            return;
        };
        if virtual_default {
            self.status = "search already defaults to your home directory".into();
            return;
        }
        let mut roots = self
            .ctx
            .config
            .modules
            .get(&id)
            .map(|m| m.roots.clone())
            .unwrap_or_default();
        if index < roots.len() {
            roots.remove(index);
        }
        self.apply_config(|cfg| cfg.set_roots(&id, roots));
    }

    fn open_review(&mut self) {
        let items = self.chosen();
        if items.is_empty() {
            self.status = "Nothing selected yet — press Space on a row first.".into();
            return;
        }
        self.needs_admin = items
            .iter()
            .flat_map(|i| i.walk())
            .any(|i| i.privilege == Privilege::Admin);
        self.review = items;
        self.review_state.select(Some(0));
        self.screen = Screen::Review;
    }

    /// Collapse the selection to the highest fully-selected nodes, so a module
    /// can clean a whole group in one go instead of one call per child. Module
    /// roots are never collapsed: the review list would stop saying anything
    /// useful.
    fn chosen(&self) -> Vec<Item> {
        let mut out = Vec::new();
        for root in &self.forest {
            for child in &root.children {
                collect_chosen(child, &self.selected, &mut out);
            }
        }
        out
    }

    pub fn review_bytes(&self) -> u64 {
        self.review.iter().map(|i| i.bytes).sum()
    }

    pub fn working_elapsed(&self) -> Duration {
        self.working_started.elapsed()
    }

    fn start_clean(&mut self) {
        self.outcomes.clear();
        self.working_total = self.review.len();
        if self.needs_admin {
            // Handled by the caller: the alternate screen has to be released
            // so sudo can read the password from the real terminal.
            self.admin_go = true;
            return;
        }
        let items = std::mem::take(&mut self.review);
        let registry = self.registry.clone();
        let scan_ctx = self.ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.clean_rx = Some(rx);
        self.screen = Screen::Working;
        self.working_started = Instant::now();
        thread::spawn(move || {
            let ctx = ReclaimContext::apply(&scan_ctx, false);
            for item in &items {
                let _ = tx.send(CleanEvent::Result(run_one(&registry, item, &ctx)));
            }
            let _ = tx.send(CleanEvent::Done);
        });
    }

    /// The one place that hands control back to the plain terminal, so a
    /// one-shot `sudo` can prompt for a password.
    pub fn take_admin_work(&mut self) -> Option<Vec<Item>> {
        if !self.admin_go {
            return None;
        }
        self.admin_go = false;
        self.needs_admin = false;
        Some(std::mem::take(&mut self.review))
    }

    pub fn apply_admin_outcomes(&mut self, outcomes: Vec<Outcome>) {
        self.outcomes = outcomes;
        self.finish_clean();
    }

    pub fn run_admin(&self, items: &[Item]) -> Vec<Outcome> {
        items
            .iter()
            .map(|item| {
                let ctx = ReclaimContext::apply(&self.ctx, item.privilege == Privilege::Admin);
                run_one(&self.registry, item, &ctx)
            })
            .collect()
    }

    fn open_jobs(&mut self) {
        self.status.clear();
        self.reload_jobs();

        match self.current_schedule_target() {
            Ok(id) => {
                if let Some(idx) = self.jobs.iter().position(|j| j.item_id == id) {
                    self.jobs_state.select(Some(idx));
                    self.screen = Screen::Jobs;
                    self.status = format!(
                        "{} is already scheduled {}",
                        id,
                        self.jobs[idx].every.display()
                    );
                } else {
                    self.begin_schedule(id);
                }
            }
            Err(_) => {
                self.screen = Screen::Jobs;
            }
        }
    }

    fn reload_jobs(&mut self) {
        match schedule::current().list() {
            Ok(jobs) => {
                self.jobs = jobs;
                let n = self.jobs.len();
                match self.jobs_state.selected() {
                    Some(_) if n == 0 => self.jobs_state.select(None),
                    Some(sel) if sel >= n => self.jobs_state.select(Some(n - 1)),
                    None if n > 0 => self.jobs_state.select(Some(0)),
                    _ => {}
                }
            }
            Err(err) => {
                self.jobs.clear();
                self.jobs_state.select(None);
                self.status = err.to_string();
            }
        }
    }

    /// Title shown in the jobs list: the scan's name for this item, if we
    /// still have it, otherwise the id the job was created with.
    fn job_title(&self, job: &ScheduledJob) -> String {
        self.find(&job.item_id)
            .map(|i| i.title.clone())
            .unwrap_or_else(|| job.item_id.clone())
    }

    /// Owned rows for the jobs list, so the UI can draw without borrowing
    /// `jobs` and `forest` at the same time.
    pub fn job_list_rows(&self) -> Vec<(String, String, String)> {
        self.jobs
            .iter()
            .map(|job| {
                (
                    self.job_title(job),
                    job.item_id.clone(),
                    job.every.display(),
                )
            })
            .collect()
    }

    fn current_schedule_target(&self) -> Result<String, &'static str> {
        let Some(row) = self.selected_row() else {
            return Err("Pick a single thing to run on a schedule.");
        };
        let Some(item) = self.find(&row.id) else {
            return Err("Pick a single thing to run on a schedule.");
        };
        schedule_target(row.privilege, item)
    }

    fn begin_schedule_from_tree(&mut self) {
        match self.current_schedule_target() {
            Ok(id) => self.begin_schedule(id),
            Err(msg) => self.status = msg.into(),
        }
    }

    fn begin_schedule(&mut self, item_id: String) {
        let idx = self
            .jobs
            .iter()
            .find(|j| j.item_id == item_id)
            .and_then(|job| {
                self.schedule_choices.iter().position(|(every, _)| {
                    schedule::parse_every(every)
                        .map(|e| e.seconds == job.every.seconds)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(0);
        self.schedule_item = Some(item_id.clone());
        self.schedule_state.select(Some(idx));
        self.status = format!("Scheduling {item_id}");
        self.screen = Screen::Schedule;
    }

    fn edit_selected_job(&mut self) {
        let Some(idx) = self.jobs_state.selected() else {
            return;
        };
        let Some(job) = self.jobs.get(idx) else {
            return;
        };
        self.begin_schedule(job.item_id.clone());
    }

    fn remove_selected_job(&mut self) {
        let Some(idx) = self.jobs_state.selected() else {
            return;
        };
        let Some(id) = self.jobs.get(idx).map(|j| j.item_id.clone()) else {
            return;
        };
        match schedule::current().remove(&id) {
            Ok(()) => {
                self.reload_jobs();
                self.status = format!("Removed schedule for {id}");
            }
            Err(err) => self.status = err.to_string(),
        }
    }

    fn apply_schedule(&mut self) {
        let Some(item_id) = self.schedule_item.clone() else {
            return;
        };
        let Some(idx) = self.schedule_state.selected() else {
            return;
        };
        let Some((every, _)) = self.schedule_choices.get(idx) else {
            return;
        };
        match schedule::parse_every(every).and_then(|every| {
            let job = ScheduledJob {
                item_id: item_id.clone(),
                every,
                command: schedule::maclean_command(&item_id)?,
                schema: schedule::JOB_SCHEMA,
            };
            schedule::current().add(&job)?;
            Ok(job)
        }) {
            Ok(job) => {
                let id = job.item_id.clone();
                self.reload_jobs();
                if let Some(idx) = self.jobs.iter().position(|j| j.item_id == id) {
                    self.jobs_state.select(Some(idx));
                }
                self.status = format!("Scheduled {} {}", job.item_id, job.every.display());
            }
            Err(err) => self.status = format!("{err}"),
        }
        self.schedule_item = None;
        self.screen = Screen::Jobs;
    }
}

fn schedule_target(privilege: Privilege, item: &Item) -> Result<String, &'static str> {
    if privilege == Privilege::Admin {
        return Err("Admin actions cannot be scheduled — nothing can type the password for you.");
    }
    let leaves = item.reclaimable_leaves();
    if leaves.len() != 1 {
        return Err("Pick a single thing to run on a schedule.");
    }
    Ok(leaves[0].id.clone())
}

fn collect_chosen(item: &Item, selected: &HashSet<String>, out: &mut Vec<Item>) {
    let leaves = item.selection_leaves();
    if leaves.is_empty() {
        return;
    }
    if leaves.iter().all(|u| selected.contains(&u.id)) {
        out.push(item.clone());
        return;
    }
    for child in &item.children {
        collect_chosen(child, selected, out);
    }
}

fn run_one(registry: &Registry, item: &Item, ctx: &ReclaimContext) -> Outcome {
    match registry.reclaim(item, ctx) {
        Ok(results) => {
            let bytes = results.iter().map(|r| r.bytes_reclaimed).sum();
            let message = if results.is_empty() {
                "nothing to do".to_string()
            } else {
                results
                    .into_iter()
                    .map(|r| r.message)
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            Outcome {
                title: item.title.clone(),
                ok: true,
                message,
                bytes,
                hint: None,
            }
        }
        Err(err) => Outcome {
            title: item.title.clone(),
            ok: false,
            message: err.message.clone(),
            bytes: 0,
            hint: err.hint.clone(),
        },
    }
}

fn flatten(
    item: &Item,
    depth: usize,
    is_root: bool,
    expanded: &HashSet<String>,
    out: &mut Vec<Row>,
) {
    let units: Vec<String> = item
        .selection_leaves()
        .into_iter()
        .map(|u| u.id.clone())
        .collect();
    // The tree is a selection UI. Nodes with nothing to tick are omitted,
    // except a module root that still has a finding (Full Disk Access, a
    // warning). Those would otherwise exist only on the modules screen.
    if units.is_empty() && !(is_root && !item.issues.is_empty()) {
        return;
    }
    let has_children = item
        .children
        .iter()
        .any(|c| !c.selection_leaves().is_empty());
    let open = expanded.contains(&item.id);
    out.push(Row {
        depth,
        id: item.id.clone(),
        title: item.title.clone(),
        summary: item.summary.clone(),
        bytes: item.bytes,
        safety: item.safety,
        privilege: item.privilege,
        has_children,
        expanded: open,
        is_root,
        units,
    });
    if has_children && open {
        for child in &item.children {
            flatten(child, depth + 1, false, expanded, out);
        }
    }
}

fn move_sel(state: &mut ListState, len: usize, delta: i32) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or(0) as i32;
    let next = (current + delta).rem_euclid(len as i32) as usize;
    state.select(Some(next));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Privilege, Safety};

    fn leaf(id: &str) -> Item {
        Item::new("t", id, id).with_reclaimable(true).with_bytes(1)
    }

    #[test]
    fn flatten_skips_rows_you_cannot_select() {
        let root = Item::new("t", "docker", "Docker").with_children(vec![
            Item::new("t", "docker:vm", "VM disk image")
                .with_bytes(2_000_000_000)
                .with_safety(Safety::Info),
            Item::new("t", "docker:images", "Images")
                .with_reclaimable(true)
                .clean_whole()
                .with_children(vec![leaf("img-a")]),
        ]);
        let mut expanded = HashSet::new();
        expanded.insert("docker".into());
        expanded.insert("docker:images".into());
        let mut rows = Vec::new();
        flatten(&root, 0, true, &expanded, &mut rows);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["docker", "docker:images", "img-a"]);
        assert!(rows.iter().all(|r| !r.units.is_empty()));
    }

    #[test]
    fn flatten_hides_a_module_with_nothing_to_tick() {
        let root = Item::new("t", "docker", "Docker").with_children(vec![
            Item::new("t", "docker:vm", "VM disk image").with_bytes(2_000_000_000),
        ]);
        let mut rows = Vec::new();
        flatten(&root, 0, true, &HashSet::new(), &mut rows);
        assert!(rows.is_empty());
    }

    #[test]
    fn flatten_shows_a_module_that_only_has_an_issue() {
        let root = Item::new("t", "trash", "Trash")
            .with_issue(crate::core::ScanIssue::full_disk_access("could not list"));
        let mut rows = Vec::new();
        flatten(&root, 0, true, &HashSet::new(), &mut rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "trash");
        assert!(rows[0].units.is_empty());
    }

    #[test]
    fn flatten_does_not_list_informational_children() {
        let project = Item::new("t", "cargo:proj", "proj")
            .with_reclaimable(true)
            .clean_whole()
            .with_children(vec![Item::new("t", "debug", "debug").with_bytes(1)]);
        let mut expanded = HashSet::new();
        expanded.insert("cargo:proj".into());
        let mut rows = Vec::new();
        flatten(&project, 0, false, &expanded, &mut rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "cargo:proj");
        assert!(!rows[0].has_children);
    }

    #[test]
    fn schedule_target_accepts_a_single_reclaimable_leaf() {
        let leaf = Item::new("t", "spotify:cache", "Cache")
            .with_reclaimable(true)
            .with_bytes(1);
        assert_eq!(
            schedule_target(Privilege::None, &leaf).unwrap(),
            "spotify:cache"
        );
    }

    #[test]
    fn schedule_target_rejects_admin_and_groups() {
        let leaf = Item::new("t", "spotify:cache", "Cache")
            .with_reclaimable(true)
            .with_bytes(1);
        assert!(schedule_target(Privilege::Admin, &leaf).is_err());

        let group = Item::new("t", "spotify", "Spotify").with_children(vec![
            leaf.clone(),
            Item::new("t", "spotify:other", "Other")
                .with_reclaimable(true)
                .with_bytes(1),
        ]);
        assert!(schedule_target(Privilege::None, &group).is_err());
    }
}
