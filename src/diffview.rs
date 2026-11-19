//! `grace diff`: changed files and a colored diff of the selected file.
use crate::git::{self, Change, LineKind, Mark, Repo};
use crate::picker::{dim, sw, Theme};
use crate::tui::{self, cut, cut_mid, hint_line, Flow};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
    Frame,
};
use std::path::PathBuf;
use std::time::Duration;

const HINTS: [(&str, &str); 4] = [("j/k", "move"), ("tab", "focus"), ("r", "refresh"), ("q", "quit")];
/// Below this width the list sits above the diff.
const SIDE_BY_SIDE: u16 = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Diff,
}

pub struct DiffView {
    dir: PathBuf,
    repo: Option<Repo>,
    selected: usize,
    list_off: usize,
    diff: Vec<(LineKind, String)>,
    diff_path: Option<String>,
    scroll: usize,
    /// Diff rows on screen at the last draw, for PgUp and PgDn.
    page: usize,
    focus: Focus,
    theme: Theme,
    hints: [(&'static str, &'static str); 4],
}

/// Color of a change mark.
pub fn mark_color(m: Mark) -> Color {
    match m {
        Mark::Modified => Color::Yellow,
        Mark::Added => Color::Green,
        Mark::Deleted => Color::Red,
        Mark::Renamed => Color::Cyan,
        Mark::Conflict => Color::Magenta,
    }
}

fn line_style(t: &Theme, k: LineKind) -> Style {
    let s = Style::default();
    match k {
        LineKind::Add => t.fg(s, Color::Green),
        LineKind::Del => t.fg(s, Color::Red),
        LineKind::Hunk => t.fg(s, Color::Cyan),
        LineKind::Header => s.add_modifier(Modifier::BOLD | Modifier::DIM),
        LineKind::Meta => dim().add_modifier(Modifier::ITALIC),
        LineKind::Context => s,
    }
}

impl DiffView {
    /// In the hub, q hides the pane instead of quitting.
    pub fn in_hub(mut self, yes: bool) -> DiffView {
        if yes {
            self.hints[3] = ("q", "hide");
        }
        self
    }

    pub fn new(dir: PathBuf, theme: Theme) -> DiffView {
        let mut v = DiffView {
            dir,
            repo: None,
            selected: 0,
            list_off: 0,
            diff: Vec::new(),
            diff_path: None,
            scroll: 0,
            page: 10,
            focus: Focus::List,
            theme,
            hints: HINTS,
        };
        v.reload();
        v
    }

    /// For tests: a view with fixed data and no git.
    #[cfg(test)]
    fn with_repo(repo: Option<Repo>, diff: Vec<(LineKind, String)>) -> DiffView {
        let path = repo.as_ref().and_then(|r| r.changes.first()).map(|c| c.path.clone());
        DiffView {
            dir: PathBuf::from("."),
            repo,
            selected: 0,
            list_off: 0,
            diff,
            diff_path: path,
            scroll: 0,
            page: 10,
            focus: Focus::List,
            theme: Theme { color: true, truecolor: true },
            hints: HINTS,
        }
    }

    fn changes(&self) -> &[Change] {
        self.repo.as_ref().map_or(&[], |r| &r.changes)
    }

    fn reload(&mut self) {
        let keep = self.changes().get(self.selected).map(|c| c.path.clone());
        self.repo = git::status(&self.dir);
        let n = self.changes().len();
        self.selected = keep
            .and_then(|p| self.changes().iter().position(|c| c.path == p))
            .unwrap_or(self.selected.min(n.saturating_sub(1)));
        self.load_diff();
    }

    fn load_diff(&mut self) {
        let Some(repo) = self.repo.as_ref() else {
            self.diff.clear();
            self.diff_path = None;
            return;
        };
        let Some(c) = repo.changes.get(self.selected) else {
            self.diff.clear();
            self.diff_path = None;
            return;
        };
        let same = self.diff_path.as_deref() == Some(c.path.as_str());
        let path = c.path.clone();
        self.diff = git::file_diff(repo, c);
        if !same {
            self.scroll = 0;
        }
        self.diff_path = Some(path);
        self.scroll = self.scroll.min(self.diff.len().saturating_sub(1));
    }

    fn select(&mut self, delta: isize) {
        let n = self.changes().len();
        if n == 0 {
            return;
        }
        let s = (self.selected as isize + delta).clamp(0, n as isize - 1) as usize;
        if s != self.selected {
            self.selected = s;
            self.load_diff();
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let max = self.diff.len().saturating_sub(self.page.max(1)) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max.max(0)) as usize;
    }
}

impl tui::App for DiffView {
    fn draw(&mut self, f: &mut Frame) {
        draw(f, self);
    }

    fn key(&mut self, k: KeyEvent) -> Flow {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.page.max(1) as isize;
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return Flow::Quit,
            KeyCode::Char('c') if ctrl => return Flow::Quit,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::List { Focus::Diff } else { Focus::List };
            }
            KeyCode::Char('j') | KeyCode::Down => match self.focus {
                Focus::List => self.select(1),
                Focus::Diff => self.scroll_by(1),
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                Focus::List => self.select(-1),
                Focus::Diff => self.scroll_by(-1),
            },
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::Char('d') if ctrl => self.scroll_by(page / 2),
            KeyCode::Char('u') if ctrl => self.scroll_by(-page / 2),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll_by(isize::MAX / 2),
            KeyCode::Char('r') => self.reload(),
            _ => {}
        }
        Flow::Continue
    }

    fn refresh(&mut self) {
        self.reload();
    }

    fn every(&self) -> Duration {
        Duration::from_secs(2)
    }
}

/// " main ↑1 ↓2 " for the top border.
fn branch_text(r: &Repo) -> String {
    let mut s = r.branch.name.clone();
    if r.branch.ahead > 0 {
        s += &format!(" ↑{}", r.branch.ahead);
    }
    if r.branch.behind > 0 {
        s += &format!(" ↓{}", r.branch.behind);
    }
    s
}

fn center(f: &mut Frame, area: Rect, msg: &str) {
    if area.height == 0 {
        return;
    }
    let y = area.y + area.height.saturating_sub(1) / 2;
    let p = Paragraph::new(Span::styled(cut(msg, area.width as usize), dim())).alignment(Alignment::Center);
    f.render_widget(p, Rect { y, height: 1, ..area });
}

fn draw(f: &mut Frame, v: &mut DiffView) {
    let t = v.theme;
    let area = f.area();
    let accent = t.fg(Style::default(), t.accent()).add_modifier(Modifier::BOLD);
    let w = area.width as usize;
    let right = match &v.repo {
        Some(r) => {
            // Keep room for the " diff " title and the corners.
            let room = w.saturating_sub(10);
            let text = cut(&branch_text(r), room.saturating_sub(2));
            if room >= 4 { Line::from(Span::styled(format!(" {text} "), dim())).right_aligned() } else { Line::default() }
        }
        None => Line::default(),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(dim())
        .title_top(Line::from(Span::styled(" diff ", accent)).left_aligned())
        .title_top(right)
        .title_bottom(hint_line(&v.hints, w.saturating_sub(4)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let Some(repo) = &v.repo else {
        center(f, inner, "not a git repository");
        return;
    };
    if repo.changes.is_empty() {
        center(f, inner, "clean: nothing to commit");
        return;
    }

    // Split the space: side by side when wide, list on top when narrow.
    let n = repo.changes.len();
    let (list, sep, diff) = if inner.width >= SIDE_BY_SIDE {
        let lw = (inner.width / 3).clamp(20, 44);
        let list = Rect { width: lw, ..inner };
        let sep = Rect { x: inner.x + lw, width: 1, ..inner };
        let diff = Rect { x: inner.x + lw + 1, width: inner.width - lw - 1, ..inner };
        (list, sep, diff)
    } else {
        let lh = (n as u16).min((inner.height / 3).max(3)).min(inner.height);
        let list = Rect { height: lh, ..inner };
        let sep = Rect { y: inner.y + lh, height: 1.min(inner.height - lh), ..inner };
        let diff = Rect { y: sep.y + sep.height, height: inner.height - lh - sep.height, ..inner };
        (list, sep, diff)
    };

    // Separator.
    let sep_lines: Vec<Line> = if sep.width == 1 {
        (0..sep.height).map(|_| Line::from(Span::styled("│", dim()))).collect()
    } else {
        vec![Line::from(Span::styled("─".repeat(sep.width as usize), dim()))]
    };
    f.render_widget(Paragraph::new(sep_lines), sep);

    // File list.
    let lh = list.height as usize;
    if v.selected < v.list_off {
        v.list_off = v.selected;
    } else if lh > 0 && v.selected >= v.list_off + lh {
        v.list_off = v.selected + 1 - lh;
    }
    let lw = list.width as usize;
    let mut lines = Vec::new();
    for (i, c) in repo.changes.iter().enumerate().skip(v.list_off).take(lh) {
        let sel = i == v.selected;
        let mut base = Style::default();
        if sel {
            if let Some(bg) = t.sel_bg() {
                base = base.bg(bg);
            }
        }
        let bar = if sel && v.focus == Focus::List { "▌" } else { " " };
        let code = c.code();
        let name = cut_mid(&git::printable(&c.path), lw.saturating_sub(1 + sw(&code) + 1));
        let name_style = if sel { t.fg(base, t.accent()).add_modifier(Modifier::BOLD) } else { base };
        let used = 1 + sw(&code) + 1 + sw(&name);
        lines.push(Line::from(vec![
            Span::styled(bar, t.fg(base, t.accent())),
            Span::styled(code, t.fg(base, mark_color(c.mark())).add_modifier(Modifier::BOLD)),
            Span::styled(" ", base),
            Span::styled(name, name_style),
            Span::styled(" ".repeat(lw.saturating_sub(used)), base),
        ]));
    }
    f.render_widget(Paragraph::new(lines), list);

    // Diff of the selected file.
    v.page = diff.height as usize;
    let dw = diff.width as usize;
    let lines: Vec<Line> = v
        .diff
        .iter()
        .skip(v.scroll)
        .take(diff.height as usize)
        .map(|(k, l)| Line::from(Span::styled(cut(l, dw), line_style(&t, *k))))
        .collect();
    f.render_widget(Paragraph::new(lines), diff);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Branch;
    use crate::tui::App;

    fn repo() -> Repo {
        let ch = |x, y, p: &str| Change { x, y, path: p.into(), orig: None };
        Repo {
            root: "/r".into(),
            prefix: String::new(),
            branch: Branch { name: "main".into(), upstream: None, ahead: 2, behind: 1 },
            changes: vec![ch(' ', 'M', "src/main.rs"), ch('?', '?', "docs/a-very-long-file-name.md"), ch('D', ' ', "old.txt")],
        }
    }

    fn diff() -> Vec<(LineKind, String)> {
        git::classify_all("diff --git a/src/main.rs b/src/main.rs\n@@ -1,2 +1,2 @@\n fn main() {\n-    old();\n+    new_function_with_a_long_name();\n }\n")
    }

    #[test]
    fn render_narrow() {
        let mut v = DiffView::with_repo(Some(repo()), diff());
        let out = tui::render_text(30, 14, |f| draw(f, &mut v));
        println!("{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("╭ diff ") && lines[0].contains("main ↑2 ↓1"), "{}", lines[0]);
        assert!(lines[1].starts_with("│▌ M src/main.rs"), "{}", lines[1]);
        assert!(lines[2].contains("?? docs/") && lines[2].contains("…"), "{}", lines[2]);
        assert!(lines[3].contains("D  old.txt"), "{}", lines[3]);
        assert!(lines[4].contains("────"), "{}", lines[4]);
        assert!(out.contains("+    new_function_with_a_lo…"), "{out}");
        assert!(lines[13].starts_with("╰ j/k move"), "{}", lines[13]);
        for l in &lines {
            assert!(l.chars().count() <= 30, "{l}");
        }
    }

    #[test]
    fn render_wide_and_states() {
        let mut v = DiffView::with_repo(Some(repo()), diff());
        let out = tui::render_text(90, 8, |f| draw(f, &mut v));
        println!("{out}");
        assert!(out.lines().nth(1).unwrap().contains("│diff --git"), "{out}");

        let mut none = DiffView::with_repo(None, Vec::new());
        assert!(tui::render_text(25, 5, |f| draw(f, &mut none)).contains("not a git repository"));
        let mut clean = DiffView::with_repo(Some(Repo { changes: vec![], ..repo() }), Vec::new());
        assert!(tui::render_text(40, 5, |f| draw(f, &mut clean)).contains("clean: nothing to commit"));
    }

    #[test]
    fn keys_move_and_scroll() {
        let mut v = DiffView::with_repo(Some(repo()), diff());
        v.page = 2;
        v.key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(v.focus, Focus::Diff);
        v.key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(v.scroll, 1);
        v.key(KeyEvent::from(KeyCode::End));
        assert_eq!(v.scroll, v.diff.len() - 2);
        v.key(KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(v.scroll, 0);
        assert_eq!(v.key(KeyEvent::from(KeyCode::Char('q'))), Flow::Quit);
    }
}
