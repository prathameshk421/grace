//! A small loop for the full-screen views: `grace diff`, `grace tree` and the hub sidebar.
//! They draw on stdout in the alternate screen, so they work inside a tmux pane.
use anyhow::Result;
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyEvent, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Frame, Terminal};
use std::io;
use std::time::{Duration, Instant};

/// What a key press asks the loop to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

pub trait App {
    fn draw(&mut self, f: &mut Frame);
    fn key(&mut self, k: KeyEvent) -> Flow;
    /// Read the data again.
    fn refresh(&mut self);
    /// How often to call `refresh`.
    fn every(&self) -> Duration;
    /// A spinner is on screen: redraw often.
    fn animating(&self) -> bool {
        false
    }
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

/// Run `app` on the full screen until it quits.
pub fn run(app: &mut dyn App) -> Result<()> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        default_hook(info);
    }));
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, Hide)?;
    let result = run_loop(app);
    restore();
    result
}

fn run_loop(app: &mut dyn App) -> Result<()> {
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut next = Instant::now() + app.every();
    loop {
        term.draw(|f| app.draw(f))?;
        let mut wait = next.saturating_duration_since(Instant::now());
        if app.animating() {
            wait = wait.min(Duration::from_millis(100));
        }
        if event::poll(wait)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press && app.key(k) == Flow::Quit => return Ok(()),
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

// ---------------------------------------------------------------------------
// Shared drawing helpers

use crate::picker::{dim, fit_end, fit_middle, sw};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

/// Cut `s` to `w` columns with "…" at the end.
pub fn cut(s: &str, w: usize) -> String {
    fit_end(s, w).into_iter().map(|(c, _)| c).collect()
}

/// Cut `s` to `w` columns with "…" in the middle.
pub fn cut_mid(s: &str, w: usize) -> String {
    fit_middle(s, w).into_iter().map(|(c, _)| c).collect()
}

/// Key hints for a bottom border, like " j/k move · q quit ". Hints that do not fit are left out.
pub fn hint_line(pairs: &[(&str, &str)], w: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    let mut used = 2;
    let mut first = true;
    for (k, l) in pairs {
        let add = sw(k) + 1 + sw(l) + if first { 0 } else { 3 };
        if used + add > w {
            continue;
        }
        if !first {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(Span::styled(k.to_string(), Style::default().add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(" {l}"), dim()));
        used += add;
        first = false;
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}
