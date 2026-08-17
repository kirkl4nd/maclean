use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap,
};

use crate::core::{Privilege, Safety, format_bytes, plural};

use super::app::{App, Check, ConfigRowKind, ModuleState, ModuleStatus, Screen};
use super::theme::{Theme, glyph};

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
/// Page margin. Space is the cheapest way to stop a terminal UI looking cramped.
const GUTTER: u16 = 3;
/// Right-aligned size column so titles don't jump as numbers change.
const SIZE_COL: usize = 9;
/// Spaces per tree depth — a bit more than a typical indent so nested rows read.
const TREE_INDENT: usize = 3;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Theme::base()), area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // top margin
            Constraint::Length(1), // title
            Constraint::Length(1), // rule
            Constraint::Length(1), // air
            Constraint::Min(5),    // body
            Constraint::Length(1), // air
            Constraint::Length(1), // rule
            Constraint::Length(1), // status
            Constraint::Length(1), // keys
            Constraint::Length(1), // bottom margin
        ])
        .split(area);

    draw_title(frame, app, chunks[1]);
    draw_rule(frame, chunks[2], None);
    match app.screen {
        Screen::Scan => draw_scan(frame, app, chunks[4]),
        Screen::Tree => draw_tree(frame, app, chunks[4]),
        Screen::Details => draw_details(frame, app, chunks[4]),
        Screen::Modules => draw_modules(frame, app, chunks[4]),
        Screen::Review => draw_review(frame, app, chunks[4]),
        Screen::Working => draw_working(frame, app, chunks[4]),
        Screen::Results => draw_results(frame, app, chunks[4]),
        Screen::Help => draw_help(frame, chunks[4]),
        Screen::Jobs => draw_jobs(frame, app, chunks[4]),
        Screen::Schedule => {
            draw_jobs(frame, app, chunks[4]);
            draw_schedule(frame, app);
        }
    }
    draw_rule(frame, chunks[6], None);
    draw_status(frame, app, chunks[7]);
    draw_keys(frame, app, chunks[8]);
}

/// A hairline across the screen. `filled` colours the left part of it, which is
/// all the progress indication this program needs.
fn draw_rule(frame: &mut Frame, area: Rect, filled: Option<f64>) {
    let width = area.width.saturating_sub(GUTTER * 2) as usize;
    let rule = glyph::RULE.repeat(width);
    let spans = match filled {
        Some(ratio) => {
            let cut = ((width as f64) * ratio.clamp(0.0, 1.0)).round() as usize;
            vec![
                Span::styled(glyph::RULE.repeat(cut), Theme::accent()),
                Span::styled(
                    glyph::RULE.repeat(width.saturating_sub(cut)),
                    Theme::muted(),
                ),
            ]
        }
        None => vec![Span::styled(rule, Theme::muted())],
    };
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(gutter_block()),
        area,
    );
}

fn gutter_block() -> Block<'static> {
    Block::default()
        .borders(Borders::NONE)
        .padding(Padding::horizontal(GUTTER))
}

fn draw_title(frame: &mut Frame, app: &App, area: Rect) {
    let section = match app.screen {
        Screen::Scan => "scanning",
        Screen::Tree => "disk",
        Screen::Details => "details",
        Screen::Modules => "modules",
        Screen::Review => "review",
        Screen::Working => "cleaning",
        Screen::Results => "results",
        Screen::Jobs | Screen::Schedule => "schedule",
        Screen::Help => "keys",
    };

    let mut right = String::new();
    if app.selected_count() > 0 {
        right.push_str(&format!(
            "{} selected  {}  {}   ",
            app.selected_count(),
            glyph::DOT,
            format_bytes(app.selected_bytes())
        ));
    } else if app.total_bytes() > 0 {
        right.push_str(&format!("{} found   ", format_bytes(app.total_bytes())));
    }
    if let Some(disk) = &app.disk {
        right.push_str(&format!(
            "{} free of {}",
            format_bytes(disk.available_bytes),
            format_bytes(disk.total_bytes)
        ));
    }

    let left = format!("maclean   {section}");
    let pad = (area.width as usize)
        .saturating_sub(left.chars().count() + right.chars().count() + (GUTTER as usize * 2))
        .max(2);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("maclean", Theme::strong()),
            Span::styled(format!("   {section}"), Theme::muted()),
            Span::raw(" ".repeat(pad)),
            Span::styled(right, Theme::muted()),
        ]))
        .block(gutter_block()),
        area,
    );
}

fn draw_scan(frame: &mut Frame, app: &App, area: Rect) {
    let done = app
        .modules
        .iter()
        .filter(|m| !matches!(m.state, ModuleState::Pending | ModuleState::Running))
        .count();
    let total = app.modules.len().max(1);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::styled(
            "Looking for space you can get back",
            Theme::strong(),
        ))
        .block(gutter_block()),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "{done} of {total} modules finished    {}    {:.1}s",
                glyph::DOT,
                app.scan_started.elapsed().as_secs_f32()
            ),
            Theme::muted(),
        ))
        .block(gutter_block()),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new(Line::from("")).block(gutter_block()),
        chunks[2],
    );
    draw_rule(frame, chunks[3], Some(done as f64 / total as f64));
    frame.render_widget(
        Paragraph::new(Line::from("")).block(gutter_block()),
        chunks[4],
    );

    let spin = SPINNER[app.tick % SPINNER.len()];
    let items: Vec<ListItem> = app
        .modules
        .iter()
        .map(|m| module_card(m, false, spin))
        .collect();
    frame.render_widget(List::new(items).block(gutter_block()), chunks[5]);
}

fn draw_tree(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.rows().is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Nothing to select.", Theme::strong()),
                Line::from(""),
                Line::styled(
                    "Press m to see every module — including things that only take up space — or r to scan again.",
                    Theme::muted(),
                ),
            ])
            .wrap(Wrap { trim: true })
            .block(gutter_block()),
            area,
        );
        return;
    }

    let width = area.width.saturating_sub(GUTTER * 2) as usize;
    let cursor = app.tree_state.selected();
    let checks: Vec<Check> = app.rows().iter().map(|r| app.check(r)).collect();
    let rows = app.rows();

    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let here = cursor == Some(i);
            let indent = " ".repeat(TREE_INDENT * row.depth);
            let twist = if !row.has_children {
                " "
            } else if row.expanded {
                glyph::OPEN
            } else {
                glyph::CLOSED
            };
            let (box_glyph, box_style) = match checks[i] {
                Check::None => (" ", Theme::muted()),
                Check::Empty => (glyph::BOX_EMPTY, Theme::muted()),
                Check::Partial => (glyph::BOX_PARTIAL, Theme::accent()),
                Check::Full => (glyph::BOX_FULL, Theme::accent()),
            };
            let size = if row.bytes > 0 {
                format!("{:>SIZE_COL$}", format_bytes(row.bytes))
            } else {
                " ".repeat(SIZE_COL)
            };
            let admin = if row.privilege == Privilege::Admin {
                "  !"
            } else {
                ""
            };

            // After the cursor column: indent + twist + two spaces + box + two spaces.
            let body_prefix = indent.chars().count() + 1 + 2 + 1 + 2;
            let prefix = 2 + body_prefix + SIZE_COL + admin.chars().count();
            let title = truncate(&row.title, width.saturating_sub(prefix + 1));
            let pad = width.saturating_sub(prefix + title.chars().count()).max(1);

            let title_style = if here {
                Theme::selected()
            } else if row.is_root {
                Theme::strong()
            } else {
                Theme::base()
            };
            let size_style = if row.safety == Safety::Destructive {
                Theme::danger()
            } else if here {
                Theme::selected_muted()
            } else {
                Theme::muted()
            };

            let mut lines = vec![Line::from(vec![
                cursor_span(here),
                Span::styled(format!("{indent}{twist}  "), Theme::muted()),
                Span::styled(format!("{box_glyph}  "), box_style),
                Span::styled(title, title_style),
                Span::raw(" ".repeat(pad)),
                Span::styled(size, size_style),
                Span::styled(admin, Theme::warn()),
            ])];

            if row.is_root && !row.summary.is_empty() {
                let sub = truncate(&row.summary, width.saturating_sub(2 + body_prefix));
                lines.push(Line::from(vec![
                    cursor_span(here),
                    Span::raw(" ".repeat(body_prefix)),
                    Span::styled(
                        sub,
                        if here {
                            Theme::selected_muted()
                        } else {
                            Theme::muted()
                        },
                    ),
                ]));
            }

            // One blank after a module header (before its children, or before the
            // next module when collapsed) and after the last child of a group.
            let next = rows.get(i + 1);
            let gap = row.is_root || next.is_some_and(|n| n.depth < row.depth);
            if gap {
                lines.push(Line::from(""));
            }
            ListItem::new(lines)
        })
        .collect();

    let list = List::new(items).block(gutter_block());
    frame.render_stateful_widget(list, area, &mut app.tree_state);
}

fn draw_details(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.config_rows.is_empty() {
        frame.render_widget(
            Paragraph::new(detail_lines(app, true))
                .wrap(Wrap { trim: false })
                .block(gutter_block()),
            area,
        );
        return;
    }

    let config_h = (app.config_rows.len() as u16 + 4).min(area.height.saturating_sub(6).max(6));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(config_h),
            Constraint::Min(3),
        ])
        .split(area);

    let mut head = Vec::new();
    if let Some(info) = &app.detail_info {
        head.push(Line::styled(info.name.clone(), Theme::strong()));
        head.push(Line::from(""));
        head.push(Line::styled(info.description.clone(), Theme::muted()));
    }
    frame.render_widget(Paragraph::new(head).block(gutter_block()), chunks[0]);

    let mut cfg_lines = vec![Line::styled("Config", Theme::heading()), Line::from("")];
    let cursor = app.config_state.selected();
    for (i, row) in app.config_rows.iter().enumerate() {
        let here = cursor == Some(i);
        let editing = here && app.config_edit.is_some();
        let value = if editing {
            format!("{}▌", app.config_edit.as_deref().unwrap_or(""))
        } else {
            row.value.clone()
        };
        let label_style = if here {
            Theme::selected()
        } else {
            Theme::muted()
        };
        let value_style = if here {
            Theme::selected()
        } else if matches!(row.kind, ConfigRowKind::AddRoot) {
            Theme::muted()
        } else {
            Theme::base()
        };
        cfg_lines.push(Line::from(vec![
            cursor_span(here),
            Span::styled(format!("{:<12}", row.label), label_style),
            Span::styled(value, value_style),
        ]));
    }
    cfg_lines.push(Line::from(""));
    cfg_lines.push(Line::styled(
        format!("    {}", app.config_file_path().display()),
        Theme::muted(),
    ));
    frame.render_widget(
        Paragraph::new(cfg_lines)
            .wrap(Wrap { trim: false })
            .block(gutter_block()),
        chunks[1],
    );

    frame.render_widget(
        Paragraph::new(detail_lines(app, false))
            .wrap(Wrap { trim: false })
            .block(gutter_block()),
        chunks[2],
    );
}

fn detail_lines(app: &App, include_header: bool) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    if include_header {
        if let Some(info) = &app.detail_info {
            lines.push(Line::styled(info.name.clone(), Theme::strong()));
            lines.push(Line::from(""));
            lines.push(Line::styled(info.description.clone(), Theme::muted()));
            lines.push(Line::from(""));
        }
    }

    if let Some(info) = &app.detail_info {
        section(&mut lines, "Looks for", &info.finds);
        section(&mut lines, "Cleaning it", &info.effects);
        if !info.locations.is_empty() {
            lines.push(Line::styled("Locations", Theme::heading()));
            for path in &info.locations {
                lines.push(Line::styled(
                    format!("    {}", path.display()),
                    Theme::muted(),
                ));
            }
            lines.push(Line::from(""));
        }
    }

    if let Some(item) = &app.detail_item {
        if app.detail_info.is_some() {
            lines.push(Line::styled("Found now", Theme::heading()));
            lines.push(Line::from(""));
            lines.push(Line::from(format!(
                "    {} across {}",
                format_bytes(item.bytes),
                plural(item.children.len(), "item")
            )));
            lines.push(Line::from(""));
        } else {
            lines.push(Line::styled(item.title.clone(), Theme::strong()));
            if !item.summary.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::styled(item.summary.clone(), Theme::muted()));
            }
            lines.push(Line::from(""));
            lines.push(field("Size", format_bytes(item.bytes)));
            lines.push(Line::from(vec![
                Span::styled(format!("    {:<14}", "Safety"), Theme::muted()),
                Span::styled(item.safety.label(), Theme::safety(item.safety)),
                Span::styled(format!("    {}", item.safety.hint()), Theme::muted()),
            ]));
            if item.privilege == Privilege::Admin {
                lines.push(Line::from(vec![
                    Span::styled(format!("    {:<14}", "Privilege"), Theme::muted()),
                    Span::styled("asks for an administrator password once", Theme::warn()),
                ]));
            }
            for detail in &item.details {
                lines.push(field(&detail.label, detail.value.clone()));
            }
            for path in &item.paths {
                let shown = path.display().to_string();
                if item.details.iter().any(|d| d.value == shown) {
                    continue;
                }
                lines.push(field("Path", shown));
            }
            if !item.notes.is_empty() {
                lines.push(Line::from(""));
                for note in &item.notes {
                    lines.push(Line::styled(format!("    {note}"), Theme::muted()));
                    lines.push(Line::from(""));
                }
            }
        }

        if !item.issues.is_empty() {
            lines.push(Line::styled("Problems", Theme::heading()));
            lines.push(Line::from(""));
            for issue in &item.issues {
                lines.push(Line::styled(
                    format!("    {}", issue.message),
                    Theme::warn(),
                ));
                if let Some(hint) = &issue.hint {
                    lines.push(Line::styled(format!("    {hint}"), Theme::muted()));
                }
                lines.push(Line::from(""));
            }
        }

        if !item.children.is_empty() {
            lines.push(Line::styled(
                format!("Contains {}", plural(item.children.len(), "item")),
                Theme::heading(),
            ));
            lines.push(Line::from(""));
            for child in item.children.iter().take(12) {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("    {:>SIZE_COL$}    ", format_bytes(child.bytes)),
                        Theme::muted(),
                    ),
                    Span::raw(child.title.clone()),
                ]));
            }
        }
    }

    lines
}

fn section(lines: &mut Vec<Line>, title: &str, entries: &[String]) {
    if entries.is_empty() {
        return;
    }
    lines.push(Line::styled(title.to_string(), Theme::heading()));
    lines.push(Line::from(""));
    for entry in entries {
        lines.push(Line::from(format!("    {entry}")));
    }
    lines.push(Line::from(""));
}

fn field(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("    {label:<14}"), Theme::muted()),
        Span::raw(value),
    ])
}

fn draw_modules(frame: &mut Frame, app: &mut App, area: Rect) {
    let spin = SPINNER[app.tick % SPINNER.len()];
    let cursor = app.module_state.selected();
    let items: Vec<ListItem> = app
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| module_card(m, cursor == Some(i), spin))
        .collect();
    let list = List::new(items).block(gutter_block());
    frame.render_stateful_widget(list, area, &mut app.module_state);
}

fn module_card(m: &ModuleStatus, here: bool, spin: &str) -> ListItem<'static> {
    let (mark, style) = match m.state {
        ModuleState::Pending => (glyph::DOT, Theme::muted()),
        ModuleState::Running => (spin, Theme::accent()),
        ModuleState::Skipped => (glyph::BOX_EMPTY, Theme::muted()),
        ModuleState::Done if m.findings == 0 && m.issues == 0 => (glyph::BOX_EMPTY, Theme::muted()),
        ModuleState::Done => (glyph::BOX_FULL, Theme::accent()),
    };
    let size = if m.bytes > 0 {
        format!("{:>SIZE_COL$}", format_bytes(m.bytes))
    } else {
        String::new()
    };
    let status = if m.state == ModuleState::Running {
        "scanning".to_string()
    } else if m.reason.is_empty() {
        "not scanned yet".to_string()
    } else {
        m.reason.clone()
    };
    let name_style = if here {
        Theme::selected()
    } else {
        Theme::base()
    };
    let desc_style = if here {
        Theme::selected_muted()
    } else {
        Theme::base()
    };
    let status_style = if here {
        Theme::selected_muted()
    } else {
        Theme::muted()
    };
    ListItem::new(vec![
        Line::from(vec![
            cursor_span(here),
            Span::styled(format!("{mark}  "), style),
            Span::styled(format!("{:<16}", m.name), name_style),
            Span::styled(
                size,
                if here {
                    Theme::selected_muted()
                } else {
                    Theme::muted()
                },
            ),
        ]),
        Line::from(vec![
            cursor_span(here),
            Span::raw("   "),
            Span::styled(m.description.clone(), desc_style),
        ]),
        Line::from(vec![
            cursor_span(here),
            Span::raw("   "),
            Span::styled(status, status_style),
        ]),
        Line::from(""),
    ])
}

fn draw_review(frame: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(3)])
        .split(area);

    let mut head = vec![
        Line::from(vec![
            Span::styled("Ready to clean  ", Theme::base()),
            Span::styled(plural(app.review.len(), "item"), Theme::strong()),
            Span::styled("    freeing about  ", Theme::base()),
            Span::styled(format_bytes(app.review_bytes()), Theme::strong()),
        ]),
        Line::from(""),
    ];
    if app.needs_admin {
        head.push(Line::styled(
            "One of these needs an administrator password. maclean will step aside so sudo can ask.",
            Theme::warn(),
        ));
    } else {
        head.push(Line::styled(
            "Enter to go ahead.  Esc to change the selection.",
            Theme::muted(),
        ));
    }
    head.push(Line::from(""));
    frame.render_widget(Paragraph::new(head).block(gutter_block()), chunks[0]);

    let cursor = app.review_state.selected();
    let items: Vec<ListItem> = app
        .review
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let here = cursor == Some(i);
            let mut lines = vec![Line::from(vec![
                cursor_span(here),
                Span::styled(
                    format!("{:>SIZE_COL$}    ", format_bytes(item.bytes)),
                    if here {
                        Theme::selected_muted()
                    } else {
                        Theme::muted()
                    },
                ),
                Span::styled(
                    item.title.clone(),
                    if here {
                        Theme::selected()
                    } else {
                        Theme::base()
                    },
                ),
                Span::styled(
                    format!("    {}", item.safety.label().to_lowercase()),
                    Theme::safety(item.safety),
                ),
            ])];
            for note in item.notes.iter().take(2) {
                lines.push(Line::from(vec![
                    cursor_span(here),
                    Span::raw("     "),
                    Span::styled(
                        note.clone(),
                        if here {
                            Theme::selected_muted()
                        } else {
                            Theme::muted()
                        },
                    ),
                ]));
            }
            lines.push(Line::from(""));
            ListItem::new(lines)
        })
        .collect();

    let list = List::new(items).block(gutter_block());
    frame.render_stateful_widget(list, chunks[1], &mut app.review_state);
}

fn draw_working(frame: &mut Frame, app: &App, area: Rect) {
    let done = app.outcomes.len();
    let total = app.working_total.max(1);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(area);

    let spin = SPINNER[app.tick % SPINNER.len()];
    frame.render_widget(
        Paragraph::new(Line::styled("Cleaning", Theme::strong())).block(gutter_block()),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "{spin}    {done} of {total} done    {}    {:.0}s",
                glyph::DOT,
                app.working_elapsed().as_secs_f32()
            ),
            Theme::muted(),
        ))
        .block(gutter_block()),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new(Line::from("")).block(gutter_block()),
        chunks[2],
    );
    draw_rule(frame, chunks[3], Some(done as f64 / total as f64));
    frame.render_widget(
        Paragraph::new(Line::from("")).block(gutter_block()),
        chunks[4],
    );

    let lines: Vec<Line> = app
        .outcomes
        .iter()
        .flat_map(|o| {
            vec![
                Line::styled(o.message.clone(), Theme::muted()),
                Line::from(""),
            ]
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(gutter_block()), chunks[5]);
}

fn draw_results(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.outcomes.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled("Nothing was changed.", Theme::muted()))
                .block(gutter_block()),
            area,
        );
        return;
    }
    let cursor = app.results_state.selected();
    let items: Vec<ListItem> = app
        .outcomes
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let here = cursor == Some(i);
            let (mark, style) = if o.ok {
                (glyph::OK, Theme::ok())
            } else {
                (glyph::FAIL, Theme::danger())
            };
            let size = if o.bytes > 0 {
                format!("{:>SIZE_COL$}", format_bytes(o.bytes))
            } else {
                String::new()
            };
            let mut lines = vec![Line::from(vec![
                cursor_span(here),
                Span::styled(format!("{mark}  "), style),
                Span::styled(
                    format!("{:<24}", o.title),
                    if here {
                        Theme::selected()
                    } else {
                        Theme::base()
                    },
                ),
                Span::styled(
                    size,
                    if here {
                        Theme::selected_muted()
                    } else {
                        Theme::muted()
                    },
                ),
            ])];
            lines.push(Line::from(vec![
                cursor_span(here),
                Span::raw("   "),
                Span::styled(
                    o.message.clone(),
                    if here {
                        Theme::selected_muted()
                    } else {
                        Theme::muted()
                    },
                ),
            ]));
            if let Some(hint) = &o.hint {
                lines.push(Line::from(vec![
                    cursor_span(here),
                    Span::raw("   "),
                    Span::styled(hint.clone(), Theme::warn()),
                ]));
            }
            lines.push(Line::from(""));
            ListItem::new(lines)
        })
        .collect();

    let list = List::new(items).block(gutter_block());
    frame.render_stateful_widget(list, area, &mut app.results_state);
}

fn draw_jobs(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.jobs.is_empty() {
        if app.screen == Screen::Schedule {
            return;
        }
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Nothing is scheduled yet.", Theme::strong()),
                Line::from(""),
                Line::styled(
                    "Pick a single reclaimable row in the disk view and press s. maclean writes the job; don't edit LaunchAgents by hand.",
                    Theme::muted(),
                ),
                Line::from(""),
                Line::styled(
                    "From this screen, + adds the row you were on, - removes a job, enter changes how often it runs.",
                    Theme::muted(),
                ),
            ])
            .wrap(Wrap { trim: true })
            .block(gutter_block()),
            area,
        );
        return;
    }

    let width = area.width.saturating_sub(GUTTER * 2) as usize;
    let cursor = app.jobs_state.selected();
    let rows = app.job_list_rows();
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, (title, item_id, interval))| {
            let here = cursor == Some(i);
            let shown = truncate(
                title,
                width.saturating_sub(2 + interval.chars().count() + 2),
            );
            let pad = width
                .saturating_sub(2 + shown.chars().count() + interval.chars().count())
                .max(1);
            let mut lines = vec![Line::from(vec![
                cursor_span(here),
                Span::styled(
                    shown,
                    if here {
                        Theme::selected()
                    } else {
                        Theme::strong()
                    },
                ),
                Span::raw(" ".repeat(pad)),
                Span::styled(
                    interval.clone(),
                    if here {
                        Theme::selected_muted()
                    } else {
                        Theme::muted()
                    },
                ),
            ])];
            if title != item_id {
                lines.push(Line::from(vec![
                    cursor_span(here),
                    Span::styled(
                        item_id.clone(),
                        if here {
                            Theme::selected_muted()
                        } else {
                            Theme::muted()
                        },
                    ),
                ]));
            }
            lines.push(Line::from(""));
            ListItem::new(lines)
        })
        .collect();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(2)])
        .split(area);
    let list = List::new(items).block(gutter_block());
    frame.render_stateful_widget(list, chunks[0], &mut app.jobs_state);
    frame.render_widget(
        Paragraph::new(Line::styled(
            "maclean owns these jobs — change them here, not as files.",
            Theme::muted(),
        ))
        .block(gutter_block()),
        chunks[1],
    );
}

fn draw_schedule(frame: &mut Frame, app: &mut App) {
    let area = centered(frame.area(), 56, 13);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Theme::muted())
        .padding(Padding::new(3, 3, 1, 1))
        .title(Line::styled("  Run this automatically  ", Theme::strong()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items: Vec<ListItem> = app
        .schedule_choices
        .iter()
        .map(|(_, label)| ListItem::new(vec![Line::from(*label), Line::from("")]))
        .collect();
    let list = List::new(items)
        .highlight_style(Theme::accent().add_modifier(ratatui::style::Modifier::BOLD));
    frame.render_stateful_widget(list, inner, &mut app.schedule_state);
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let rows = [
        ("↑ ↓", "move"),
        ("→ ←", "open or close a row"),
        ("space", "select — a parent selects everything under it"),
        ("*", "select everything in the tree"),
        ("-", "select nothing"),
        ("enter", "details for the row, or the module's config"),
        ("a", "review the selection, then clean"),
        ("m", "every module — config lives on that page"),
        ("r", "scan again"),
        (
            "s",
            "scheduled jobs — add this row, or manage existing ones",
        ),
        ("q", "quit, from anywhere"),
    ];
    let mut lines = vec![Line::styled("Keys", Theme::heading()), Line::from("")];
    for (key, what) in rows {
        lines.push(Line::from(vec![
            Span::styled(format!("    {key:<10}"), Theme::accent()),
            Span::raw(what),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        "    Selecting and opening are separate, so you can look without choosing.",
        Theme::muted(),
    ));
    lines.push(Line::from(""));
    lines.push(Line::styled(
        "    maclean never runs as root. Rows marked ! ask for a password once.",
        Theme::muted(),
    ));
    frame.render_widget(Paragraph::new(lines).block(gutter_block()), area);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let text = if !app.status.is_empty() {
        app.status.clone()
    } else if app.screen == Screen::Tree {
        app.selected_row()
            .map(|r| r.summary.clone())
            .unwrap_or_default()
    } else if matches!(app.screen, Screen::Jobs | Screen::Schedule) {
        app.jobs_state
            .selected()
            .and_then(|i| app.jobs.get(i))
            .map(|j| format!("{} · {}", j.item_id, j.every.display()))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let width = area.width.saturating_sub(GUTTER * 2) as usize;
    frame.render_widget(
        Paragraph::new(Line::styled(truncate(&text, width), Theme::muted())).block(gutter_block()),
        area,
    );
}

fn draw_keys(frame: &mut Frame, app: &App, area: Rect) {
    let keys: &[(&str, &str)] = match app.screen {
        Screen::Scan => &[("q", "quit"), ("?", "keys")],
        Screen::Tree => &[
            ("space", "select"),
            ("*", "all"),
            ("-", "none"),
            ("→ ←", "open"),
            ("enter", "details"),
            ("a", "clean"),
            ("s", "schedule"),
            ("r", "rescan"),
            ("?", "keys"),
            ("q", "quit"),
        ],
        Screen::Details if app.config_edit.is_some() => &[("enter", "save"), ("esc", "cancel")],
        Screen::Details if !app.config_rows.is_empty() => &[
            ("space", "toggle"),
            ("enter", "edit"),
            ("+", "add folder"),
            ("-", "remove"),
            ("r", "rescan"),
            ("esc", "back"),
            ("q", "quit"),
        ],
        Screen::Details => &[("esc", "back"), ("q", "quit")],
        Screen::Modules => &[
            ("enter", "details"),
            ("r", "rescan"),
            ("esc", "back"),
            ("q", "quit"),
        ],
        Screen::Review => &[("enter", "clean"), ("esc", "back"), ("q", "quit")],
        Screen::Working => &[],
        Screen::Results => &[("enter", "scan again"), ("q", "quit")],
        Screen::Jobs => &[
            ("enter", "change"),
            ("+", "add"),
            ("-", "remove"),
            ("esc", "back"),
            ("q", "quit"),
        ],
        Screen::Schedule => &[("enter", "confirm"), ("esc", "cancel")],
        Screen::Help => &[("esc", "back")],
    };
    let mut spans = Vec::new();
    for (i, (key, what)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                format!("   {}   ", glyph::DOT),
                Theme::muted(),
            ));
        }
        spans.push(Span::styled(key.to_string(), Theme::accent()));
        spans.push(Span::styled(format!("  {what}"), Theme::muted()));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(gutter_block()),
        area,
    );
}

fn cursor_span(here: bool) -> Span<'static> {
    Span::styled(
        if here {
            format!("{} ", glyph::CURSOR)
        } else {
            "  ".into()
        },
        Theme::accent(),
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}
