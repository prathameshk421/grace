//! Interactive TUI picker.
//!
//! The TUI draws on stderr, because the shell function captures stdout.
use crate::model::Entry;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame, Terminal,
};

fn haystack(e: &Entry) -> String {
    let mut s = e.path.clone();
    for t in &e.tags {
        s.push(' ');
        s.push_str(&t.tool);
    }
    s
}

/// Return indices into `entries` that match `query`, best first.
fn filter(entries: &[Entry], query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..entries.len()).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let h = haystack(e);
            pattern
                .score(Utf32Str::new(&h, &mut buf), &mut matcher)
                .map(|s| (s, i))
        })
        .collect();
    // Stable sort keeps the original order for equal scores.
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Char positions in `path` that the query matched.
fn match_positions(path: &str, query: &str, matcher: &mut Matcher) -> Vec<u32> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut idx = Vec::new();
    pattern.indices(Utf32Str::new(path, &mut buf), matcher, &mut idx);
    idx
}

fn time_ago(now: i64, then: i64) -> String {
    let d = (now - then).max(0);
    match d {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", d / 60),
        3600..=86399 => format!("{}h", d / 3600),
        86400..=604799 => format!("{}d", d / 86400),
        604800..=2591999 => format!("{}w", d / 604800),
        2592000..=31535999 => format!("{}mo", d / 2592000),
        _ => format!("{}y", d / 31536000),
    }
}

fn tag_color(tool: &str) -> Color {
    match tool {
        "claude" => Color::LightRed,
        "antigravity" => Color::Blue,
        "codex" => Color::Green,
        "gemini" => Color::Magenta,
        "cursor" => Color::Cyan,
        "vscode" => Color::Rgb(0, 122, 204),
        "vscodium" => Color::Rgb(70, 190, 230),
        "windsurf" => Color::Rgb(0, 200, 170),
        "zed" => Color::Rgb(255, 140, 40),
        "neovim" => Color::LightGreen,
        "vim" => Color::Rgb(1, 152, 51),
        "sublime" => Color::LightYellow,
        "jetbrains" => Color::Rgb(255, 80, 140),
        _ => Color::Gray,
    }
}

fn display_path(path: &str, home: Option<&str>) -> String {
    if let Some(h) = home.filter(|h| !h.is_empty()) {
        let h = crate::platform::trim_separators(h);
        let h = h.as_str();
        if crate::platform::same_path(path, h) {
            return "~".into();
        }
        if let (Some(pre), Some(rest)) = (path.get(..h.len()), path.get(h.len()..)) {
            let sep = rest.starts_with('/') || (cfg!(windows) && rest.starts_with('\\'));
            if sep && crate::platform::same_path(pre, h) {
                return format!("~{rest}");
            }
        }
    }
    path.to_string()
}

struct State {
    entries: Vec<Entry>,
    query: String,
    matches: Vec<usize>,
    selected: usize,
    offset: usize,
    now: i64,
    home: Option<String>,
    matcher: Matcher,
}

impl State {
    fn refilter(&mut self) {
        self.matches = filter(&self.entries, &self.query);
        self.selected = 0;
        self.offset = 0;
    }

    fn mv(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let max = self.matches.len() - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max as isize) as usize;
    }
}

fn draw(f: &mut Frame, st: &mut State) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);

    // Prompt line.
    let prompt = Line::from(vec![
        Span::styled("grace ❯ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(st.query.clone()),
        Span::styled("█", dim),
    ]);
    f.render_widget(Paragraph::new(prompt), chunks[0]);

    // List, with scrolling that keeps the selection visible.
    let height = chunks[1].height as usize;
    if height > 0 {
        if st.selected < st.offset {
            st.offset = st.selected;
        } else if st.selected >= st.offset + height {
            st.offset = st.selected + 1 - height;
        }
    }
    let width = chunks[1].width as usize;
    let mut lines = Vec::new();
    for (row, &ei) in st.matches.iter().enumerate().skip(st.offset).take(height) {
        let e = &st.entries[ei];
        let base = if row == st.selected {
            Style::default().bg(Color::DarkGray)
        } else {
            Style::default()
        };

        let shown = display_path(&e.path, st.home.as_deref());
        let (parent, name) = match shown.rfind(|c| c == '/' || (cfg!(windows) && c == '\\')) {
            Some(p) if p + 1 < shown.len() => (&shown[..=p], &shown[p + 1..]),
            _ => ("", shown.as_str()),
        };
        // Match positions refer to the full path. Map them onto the shown text.
        let skip = e.path.chars().count() as i64 - shown.chars().count() as i64;
        let hl: Vec<u32> = match_positions(&e.path, &st.query, &mut st.matcher)
            .into_iter()
            .filter_map(|p| {
                let q = p as i64 - skip;
                // Chars inside the home prefix have no match in the shown text.
                if q >= 0 && (skip == 0 || q >= 1) {
                    Some(q as u32)
                } else {
                    None
                }
            })
            .collect();

        let mut spans: Vec<Span> = vec![Span::styled(
            if e.pinned { "★ " } else { "  " },
            base.fg(Color::Yellow),
        )];
        let parent_len = parent.chars().count();
        let mut push_text = |text: &str, offset: usize, style: Style| {
            for (i, ch) in text.chars().enumerate() {
                let mut s = style;
                if hl.contains(&((offset + i) as u32)) {
                    s = s.fg(Color::Yellow).add_modifier(Modifier::UNDERLINED);
                }
                spans.push(Span::styled(ch.to_string(), s));
            }
        };
        push_text(parent, 0, base.add_modifier(Modifier::DIM));
        push_text(name, parent_len, base.add_modifier(Modifier::BOLD));

        let mut used = 2 + shown.chars().count();
        for t in &e.tags {
            let badge = format!(" [{}]", t.tool);
            used += badge.chars().count();
            spans.push(Span::styled(badge, base.fg(tag_color(&t.tool))));
        }
        let ago = time_ago(st.now, e.last_visit);
        let pad = width.saturating_sub(used + ago.chars().count());
        spans.push(Span::styled(" ".repeat(pad.max(1)), base));
        spans.push(Span::styled(ago, base.add_modifier(Modifier::DIM)));
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), chunks[1]);

    // Footer.
    let help = "↑↓ move · enter travel · esc cancel";
    let count = format!("{}/{}", st.matches.len(), st.entries.len());
    let pad = (chunks[2].width as usize)
        .saturating_sub(help.chars().count() + count.chars().count());
    let footer = Line::from(vec![
        Span::styled(help, dim),
        Span::raw(" ".repeat(pad)),
        Span::styled(count, dim),
    ]);
    f.render_widget(Paragraph::new(footer), chunks[2]);
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stderr(), LeaveAlternateScreen);
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stderr>>,
    st: &mut State,
) -> Result<Option<Entry>> {
    loop {
        terminal.draw(|f| draw(f, st))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Ok(None),
            KeyCode::Char('c') if ctrl => return Ok(None),
            KeyCode::Enter => {
                if let Some(&i) = st.matches.get(st.selected) {
                    return Ok(Some(st.entries[i].clone()));
                }
            }
            KeyCode::Up => st.mv(-1),
            KeyCode::Down => st.mv(1),
            KeyCode::Char('p') | KeyCode::Char('k') if ctrl => st.mv(-1),
            KeyCode::Char('n') | KeyCode::Char('j') if ctrl => st.mv(1),
            KeyCode::Char('u') if ctrl => {
                st.query.clear();
                st.refilter();
            }
            KeyCode::Backspace => {
                st.query.pop();
                st.refilter();
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                st.query.push(c);
                st.refilter();
            }
            _ => {}
        }
    }
}

pub fn pick(entries: Vec<Entry>, query: &str, now: i64) -> Result<Option<Entry>> {
    let matches = filter(&entries, query);
    // Fast travel: one match for a given query, so skip the UI.
    if !query.trim().is_empty() && matches.len() == 1 {
        return Ok(Some(entries[matches[0]].clone()));
    }

    let mut st = State {
        entries,
        query: query.to_string(),
        matches,
        selected: 0,
        offset: 0,
        now,
        home: crate::platform::home_dir().map(|h| h.to_string_lossy().into_owned()),
        matcher: Matcher::new(Config::DEFAULT.match_paths()),
    };

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        default_hook(info);
    }));

    enable_raw_mode()?;
    if let Err(e) = execute!(std::io::stderr(), EnterAlternateScreen) {
        restore();
        return Err(e.into());
    }
    let result = (|| {
        let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stderr()))?;
        run(&mut terminal, &mut st)
    })();
    restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Tag;

    fn entry(path: &str, tools: &[&str]) -> Entry {
        Entry {
            path: path.into(),
            visits: 1,
            last_visit: 0,
            pinned: false,
            tags: tools
                .iter()
                .map(|t| Tag { tool: t.to_string(), count: 1, last_used: 0 })
                .collect(),
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            entry("/home/u/alpha", &["claude"]),
            entry("/home/u/beta", &["codex"]),
            entry("/home/u/gamma", &[]),
        ]
    }

    #[test]
    fn empty_query_keeps_order() {
        assert_eq!(filter(&sample(), ""), vec![0, 1, 2]);
    }

    #[test]
    fn query_narrows() {
        assert_eq!(filter(&sample(), "beta"), vec![1]);
    }

    #[test]
    fn tag_text_matches() {
        assert_eq!(filter(&sample(), "claude"), vec![0]);
    }

    #[test]
    fn no_match_is_empty() {
        assert!(filter(&sample(), "zzzqqq").is_empty());
    }

    #[test]
    fn time_ago_units() {
        assert_eq!(time_ago(100, 90), "now");
        assert_eq!(time_ago(1000, 700), "5m");
        assert_eq!(time_ago(3 * 3600, 0), "3h");
        assert_eq!(time_ago(2 * 86400, 0), "2d");
    }
}
