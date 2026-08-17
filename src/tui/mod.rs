use std::io::{Write, stdin, stdout};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;

use crate::core::{Registry, ScanContext, disk_usage};

use app::App;

mod app;
mod theme;
mod ui;

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut out = stdout();
    let _ = execute!(out, LeaveAlternateScreen, Show);
    let _ = out.flush();
}

fn enter_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, Hide)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    terminal.clear()?;
    Ok(terminal)
}

pub fn run(registry: Arc<Registry>, ctx: ScanContext) -> Result<()> {
    let disk = disk_usage(std::path::Path::new("/")).ok();
    let mut terminal = enter_terminal()?;
    let _guard = TerminalGuard;

    // Scanning starts here, on its own threads: the first frame is drawn
    // immediately and every frame after it is cheap.
    let mut app = App::new(registry, ctx, disk);

    while !app.should_quit {
        app.pump();
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        if let Some(items) = app.take_admin_work() {
            drop(terminal);
            restore_terminal();
            println!("\nOne of the selected actions needs administrator rights.");
            println!(
                "maclean is not running as root: sudo will ask you once, for this action only.\n"
            );
            let outcomes = app.run_admin(&items);
            for outcome in &outcomes {
                println!("  {}", outcome.message);
            }
            print!("\nPress Enter to go back to maclean. ");
            let _ = stdout().flush();
            let mut buf = String::new();
            let _ = stdin().read_line(&mut buf);
            terminal = enter_terminal()?;
            app.apply_admin_outcomes(outcomes);
            continue;
        }

        // Short poll so the spinner keeps moving while scans run.
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.handle_key(key);
                }
            }
        }
    }

    app.cancel_scan();
    Ok(())
}
