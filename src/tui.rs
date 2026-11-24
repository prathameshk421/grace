//! A small loop for the full-screen views: `grace diff`, `grace tree` and the hub sidebar.
//! They draw on stdout in the alternate screen, so they work inside a tmux pane.
use anyhow::{bail, Result};
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
        Event, KeyEvent, KeyEventKind, MouseEvent,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Frame, Terminal};
use std::io::{self, IsTerminal};
use std::time::{Duration, Instant};

/// What an input event asks the loop to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

pub trait App {
    fn draw(&mut self, f: &mut Frame);
    fn key(&mut self, k: KeyEvent) -> Flow;
    fn mouse(&mut self, _m: MouseEvent) -> Flow {
        Flow::Continue
    }
    fn focus(&mut self, _focused: bool) {}
    /// Read the data again (called every `self.every()`).
    fn refresh(&mut self);
    /// Check background worker channels without blocking. Return true if state changed.
    fn poll_bg(&mut self) -> bool {
        false
    }
    /// True while a background refresh is in flight so UI polls frequently.
    fn loading(&self) -> bool {
        false
    }
    /// How often to call `refresh`.
    fn every(&self) -> Duration;
    /// A spinner is on screen: redraw often.
    fn animating(&self) -> bool {
        false
    }
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableFocusChange,
        LeaveAlternateScreen,
        Show
    );
}

/// Run `app` on the full screen until it quits.
pub fn run(app: &mut dyn App) -> Result<()> {
    if !io::stdout().is_terminal() {
        bail!("grace requires an interactive terminal for this view");
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        default_hook(info);
    }));
    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableFocusChange,
        Hide
    )?;
    let result = run_loop(app);
    restore();
    result
}

fn run_loop(app: &mut dyn App) -> Result<()> {
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut next = Instant::now() + app.every();
    loop {
        app.poll_bg();
        term.draw(|f| app.draw(f))?;
        let mut wait = next.saturating_duration_since(Instant::now());
        if app.animating() {
            wait = wait.min(Duration::from_millis(100));
        }
        if app.loading() {
            wait = wait.min(Duration::from_millis(40));
        }
        if event::poll(wait)? {
            match event::read()? {
                Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    if app.key(k) == Flow::Quit {
                        return Ok(());
                    }
                }
                Event::Mouse(m) => {
                    if app.mouse(m) == Flow::Quit {
                        return Ok(());
                    }
                }
                Event::FocusGained => app.focus(true),
                Event::FocusLost => app.focus(false),
                _ => {}
            }
        }
        if Instant::now() >= next {
            app.refresh();
            next = Instant::now() + app.every();
        }
    }
}

/// Render `draw` into a test buffer and return the text, one line per row.
#[cfg(test)]
pub fn render_text(w: u16, h: u16, draw: impl FnOnce(&mut Frame)) -> String {
    let mut term = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(draw).unwrap();
    let buf = term.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}
