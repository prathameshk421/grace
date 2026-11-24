//! `grace diff`: changed files and a colored diff of the selected file.
use crate::git::{self, Change, DiffLine, DiffScope, LineKind, Mark, Repo};
use crate::platform;
use crate::theme::{Theme, ToastKind};
use crate::tui::{self, Flow};
use crate::widgets::{
    center_msg, cut, cut_mid, cut_tail, hint_line, render_modal, render_scrollbar, sw, toast_line,
    Toast,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const HINTS: [(&str, &str); 6] = [
    ("j/k", "move"),
    ("⇥", "focus"),
    ("[/]", "hunk"),
    ("/", "find"),
    ("s/u", "stage"),
    ("?", "keys"),
];
const SIDE_BY_SIDE: u16 = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Diff,
}

type DiffBgResult = (Option<Repo>, Vec<DiffLine>, Option<String>);

pub struct DiffView {
    dir: PathBuf,
    repo: Option<Repo>,
    selected: usize,
    list_off: usize,
    raw_diff: Vec<DiffLine>,
    diff: Vec<DiffLine>,
    diff_path: Option<String>,
    scope: DiffScope,
    show_headers: bool,
    scroll: usize,
    h_scroll: usize,
    page: usize,
    focus: Focus,
    focused: bool,
    in_hub: bool,
    meta_cache: String,
    theme: Theme,
    hints: [(&'static str, &'static str); 6],
    searching: bool,
    search: String,
    show_help: bool,
    toast: Option<Toast>,
    list_area: Rect,
    diff_area: Rect,
    bg_rx: Option<Receiver<DiffBgResult>>,
    last_bg: Instant,
}

pub fn mark_color(m: Mark, theme: &Theme) -> Color {
    match m {
        Mark::Modified => theme.accent(),
        Mark::Added => theme.diff_add_fg(),
        Mark::Deleted => theme.diff_del_fg(),
        Mark::Renamed => theme.diff_hunk_fg(),
        Mark::Conflict => theme.diff_conflict_fg(),
    }
}

fn line_style(t: &Theme, k: LineKind) -> Style {
    let s = Style::default();
    match k {
        LineKind::Add => {
            let base = t.fg(s, t.diff_add_fg());
            match t.diff_add_bg() {
                Some(bg) => base.bg(bg),
                None => base,
            }
        }
        LineKind::Del => {
            let base = t.fg(s, t.diff_del_fg());
            match t.diff_del_bg() {
                Some(bg) => base.bg(bg),
                None => base,
            }
        }
        LineKind::Hunk => {
            let base = t.fg(s, t.diff_hunk_fg());
            match t.diff_hunk_bg() {
                Some(bg) => base.bg(bg),
                None => base,
            }
        }
        LineKind::Header => t.muted().add_modifier(Modifier::BOLD),
        LineKind::Meta => t.muted().add_modifier(Modifier::ITALIC),
        LineKind::Context => s,
    }
}

impl DiffView {
    pub fn in_hub(mut self, yes: bool) -> DiffView {
        self.in_hub = yes;
        if yes {
            self.hints[5] = ("q", "hide");
        }
        self
    }

    pub fn new(dir: PathBuf, theme: Theme) -> DiffView {
        let mut v = DiffView {
            dir,
            repo: None,
            selected: 0,
            list_off: 0,
            raw_diff: Vec::new(),
            diff: Vec::new(),
            diff_path: None,
            scope: DiffScope::All,
            show_headers: false,
            scroll: 0,
            h_scroll: 0,
            page: 10,
            focus: Focus::List,
            focused: true,
            in_hub: false,
            meta_cache: String::new(),
            theme,
            hints: HINTS,
            searching: false,
            search: String::new(),
            show_help: false,
            toast: None,
            list_area: Rect::default(),
            diff_area: Rect::default(),
            bg_rx: None,
            last_bg: Instant::now(),
        };
        v.reload_sync();
        v
    }

    #[cfg(test)]
    fn with_repo(repo: Option<Repo>, diff: Vec<(LineKind, String)>) -> DiffView {
        let path = repo
            .as_ref()
            .and_then(|r| r.changes.first())
            .map(|c| c.path.clone());
        let raw_diff: Vec<DiffLine> = diff
            .into_iter()
            .map(|(kind, text)| DiffLine {
                kind,
                old_no: None,
                new_no: None,
                text,
            })
            .collect();
        let mut v = DiffView {
            dir: PathBuf::from("."),
            repo,
            selected: 0,
            list_off: 0,
            raw_diff,
            diff: Vec::new(),
            diff_path: path,
            scope: DiffScope::All,
            show_headers: false,
            scroll: 0,
            h_scroll: 0,
            page: 10,
            focus: Focus::List,
            focused: true,
            in_hub: false,
            meta_cache: String::new(),
            theme: Theme::dark(),
            hints: HINTS,
            searching: false,
            search: String::new(),
            show_help: false,
            toast: None,
            list_area: Rect::default(),
            diff_area: Rect::default(),
            bg_rx: None,
            last_bg: Instant::now(),
        };
        v.rebuild_visible_diff();
        v
    }

    fn changes(&self) -> &[Change] {
        self.repo.as_ref().map_or(&[], |r| &r.changes)
    }

    fn rebuild_visible_diff(&mut self) {
        if self.show_headers {
            self.diff = self.raw_diff.clone();
        } else {
            let filtered: Vec<DiffLine> = self
                .raw_diff
                .iter()
                .filter(|l| l.kind != LineKind::Header)
                .cloned()
                .collect();
            self.diff = if filtered.is_empty() {
                self.raw_diff.clone()
            } else {
                filtered
            };
        }
        self.scroll = self.scroll.min(self.diff.len().saturating_sub(1));
    }

    fn reload_sync(&mut self) {
        let keep = self.changes().get(self.selected).map(|c| c.path.clone());
        self.repo = git::status(&self.dir);
        let n = self.changes().len();
        self.selected = keep
            .and_then(|p| self.changes().iter().position(|c| c.path == p))
            .unwrap_or(self.selected.min(n.saturating_sub(1)));
        self.load_diff();
    }

    fn reload_async(&mut self) {
        if self.bg_rx.is_some() {
            return;
        }
        if self.in_hub {
            let (win_active, attached) = crate::hub::pane_activity();
            if (!win_active || !attached) && self.last_bg.elapsed() < Duration::from_secs(15) {
                return;
            }
        }
        self.last_bg = Instant::now();
        let dir = self.dir.clone();
        let keep = self.changes().get(self.selected).map(|c| c.path.clone());
        let selected_idx = self.selected;
        let scope = self.scope;
        let (tx, rx) = mpsc::channel();
        self.bg_rx = Some(rx);
        std::thread::spawn(move || {
            let repo = git::status(&dir);
            let (lines, path) = match &repo {
                Some(r) => {
                    let idx = keep
                        .as_ref()
                        .and_then(|p| r.changes.iter().position(|c| &c.path == p))
                        .unwrap_or(selected_idx.min(r.changes.len().saturating_sub(1)));
                    if let Some(c) = r.changes.get(idx) {
                        (git::file_diff(r, c, scope), Some(c.path.clone()))
                    } else {
                        (Vec::new(), None)
                    }
                }
                None => (Vec::new(), None),
            };
            let _ = tx.send((repo, lines, path));
        });
    }

    fn load_diff(&mut self) {
        let Some(repo) = self.repo.as_ref() else {
            self.raw_diff.clear();
            self.diff.clear();
            self.diff_path = None;
            return;
        };
        let Some(c) = repo.changes.get(self.selected) else {
            self.raw_diff.clear();
            self.diff.clear();
            self.diff_path = None;
            return;
        };
        let same = self.diff_path.as_deref() == Some(c.path.as_str());
        let path = c.path.clone();
        self.raw_diff = git::file_diff(repo, c, self.scope);
        if !same {
            self.scroll = 0;
            self.h_scroll = 0;
        }
        self.diff_path = Some(path);
        self.rebuild_visible_diff();
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

    fn jump_hunk(&mut self, forward: bool) {
        if self.diff.is_empty() {
            return;
        }
        if forward {
            if let Some(pos) = self
                .diff
                .iter()
                .enumerate()
                .skip(self.scroll + 1)
                .find(|(_, l)| l.kind == LineKind::Hunk)
                .map(|(i, _)| i)
            {
                self.scroll = pos;
            }
        } else if let Some(pos) = self.diff[..self.scroll]
            .iter()
            .rposition(|l| l.kind == LineKind::Hunk)
        {
            self.scroll = pos;
        }
    }

    fn jump_search(&mut self, forward: bool) {
        if self.search.is_empty() || self.diff.is_empty() {
            return;
        }
        let q = self.search.to_ascii_lowercase();
        let matches = |l: &DiffLine| l.text.to_ascii_lowercase().contains(&q);
        if forward {
            let pos = self
                .diff
                .iter()
                .enumerate()
                .skip(self.scroll + 1)
                .find(|(_, l)| matches(l))
                .or_else(|| self.diff.iter().enumerate().find(|(_, l)| matches(l)))
                .map(|(i, _)| i);
            if let Some(p) = pos {
                self.scroll = p;
            } else {
                self.flash("pattern not found", ToastKind::Warn);
            }
        } else {
            let pos = self.diff[..self.scroll]
                .iter()
                .rposition(matches)
                .or_else(|| self.diff.iter().rposition(matches));
            if let Some(p) = pos {
                self.scroll = p;
            } else {
                self.flash("pattern not found", ToastKind::Warn);
            }
        }
    }

    fn flash(&mut self, msg: impl Into<String>, kind: ToastKind) {
        self.toast = Some(Toast::new(msg, kind, 1800));
    }

    fn stage_selected(&mut self, stage: bool) {
        let Some(repo) = self.repo.as_ref() else {
            return;
        };
        let Some(c) = repo.changes.get(self.selected).cloned() else {
            return;
        };
        if git::stage_file(repo, &c, stage) {
            let verb = if stage { "staged" } else { "unstaged" };
            self.flash(format!("{verb} {}", c.path), ToastKind::Success);
            self.reload_sync();
        } else {
            self.flash("git stage failed", ToastKind::Error);
        }
    }

    fn open_selected(&mut self) {
        let Some(repo) = self.repo.as_ref() else {
            return;
        };
        let Some(c) = repo.changes.get(self.selected) else {
            return;
        };
        if c.mark() == Mark::Deleted {
            self.flash("file is deleted", ToastKind::Warn);
            return;
        }
        let full = PathBuf::from(&repo.root).join(&c.path);
        if platform::open_path(&full) {
            self.flash(format!("opened {}", c.path), ToastKind::Success);
        } else {
            self.flash("no opener found", ToastKind::Warn);
        }
    }
}

impl tui::App for DiffView {
    fn draw(&mut self, f: &mut Frame) {
        draw(f, self);
    }

    fn focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn loading(&self) -> bool {
        self.bg_rx.is_some()
    }

    fn poll_bg(&mut self) -> bool {
        let Some(rx) = &self.bg_rx else { return false };
        let Ok((repo, lines, path)) = rx.try_recv() else {
            return false;
        };
        self.bg_rx = None;
        let keep = self.changes().get(self.selected).map(|c| c.path.clone());
        self.repo = repo;
        let n = self.changes().len();
        self.selected = keep
            .and_then(|p| self.changes().iter().position(|c| c.path == p))
            .unwrap_or(self.selected.min(n.saturating_sub(1)));
        let current_path = self.changes().get(self.selected).map(|c| c.path.as_str());
        if current_path == path.as_deref() {
            let same = self.diff_path.as_deref() == path.as_deref();
            self.raw_diff = lines;
            if !same {
                self.scroll = 0;
                self.h_scroll = 0;
            }
            self.diff_path = path;
            self.rebuild_visible_diff();
        } else {
            self.load_diff();
        }
        true
    }

    fn mouse(&mut self, m: MouseEvent) -> Flow {
        let pos = ratatui::layout::Position {
            x: m.column,
            y: m.row,
        };
        match m.kind {
            MouseEventKind::ScrollUp => {
                if self.diff_area.contains(pos) {
                    self.scroll_by(-3);
                } else {
                    self.select(-1);
                }
            }
            MouseEventKind::ScrollDown => {
                if self.diff_area.contains(pos) {
                    self.scroll_by(3);
                } else {
                    self.select(1);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.show_help {
                    self.show_help = false;
                    return Flow::Continue;
                }
                if self.list_area.contains(pos) {
                    self.focus = Focus::List;
                    let idx = self.list_off + (m.row - self.list_area.y) as usize;
                    if idx < self.changes().len() {
                        self.selected = idx;
                        self.load_diff();
                    }
                } else if self.diff_area.contains(pos) {
                    self.focus = Focus::Diff;
                }
            }
            _ => {}
        }
        Flow::Continue
    }

    fn key(&mut self, k: KeyEvent) -> Flow {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if self.show_help {
            if matches!(
                k.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter
            ) || (ctrl && k.code == KeyCode::Char('c'))
            {
                self.show_help = false;
            }
            return Flow::Continue;
        }
        if self.searching {
            match k.code {
                KeyCode::Esc => {
                    self.searching = false;
                    self.search.clear();
                }
                KeyCode::Enter => {
                    self.searching = false;
                    self.jump_search(true);
                }
                KeyCode::Backspace => {
                    self.search.pop();
                }
                KeyCode::Char(c)
                    if (!ctrl && !k.modifiers.contains(KeyModifiers::ALT))
                        || (cfg!(windows) && ctrl && k.modifiers.contains(KeyModifiers::ALT)) =>
                {
                    self.search.push(c);
                }
                _ => {}
            }
            return Flow::Continue;
        }

        let page = self.page.max(1) as isize;
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return Flow::Quit,
            KeyCode::Char('c') if ctrl => return Flow::Quit,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::List {
                    Focus::Diff
                } else {
                    Focus::List
                };
            }
            KeyCode::Char('j') | KeyCode::Down => match self.focus {
                Focus::List => self.select(1),
                Focus::Diff => self.scroll_by(1),
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                Focus::List => self.select(-1),
                Focus::Diff => self.scroll_by(-1),
            },
            KeyCode::PageDown | KeyCode::Char(' ') => match self.focus {
                Focus::List => self.select(5),
                Focus::Diff => self.scroll_by(page),
            },
            KeyCode::PageUp => match self.focus {
                Focus::List => self.select(-5),
                Focus::Diff => self.scroll_by(-page),
            },
            KeyCode::Char('d') if ctrl => self.scroll_by(page / 2),
            KeyCode::Char('u') if ctrl => self.scroll_by(-page / 2),
            KeyCode::Home | KeyCode::Char('g') => match self.focus {
                Focus::List => {
                    self.selected = 0;
                    self.load_diff();
                }
                Focus::Diff => self.scroll = 0,
            },
            KeyCode::End | KeyCode::Char('G') => match self.focus {
                Focus::List => {
                    let n = self.changes().len();
                    if n > 0 {
                        self.selected = n - 1;
                        self.load_diff();
                    }
                }
                Focus::Diff => self.scroll_by(isize::MAX / 2),
            },
            KeyCode::Char(']') => self.jump_hunk(true),
            KeyCode::Char('[') => self.jump_hunk(false),
            KeyCode::Char('h') | KeyCode::Left => self.h_scroll = self.h_scroll.saturating_sub(4),
            KeyCode::Char('l') | KeyCode::Right => self.h_scroll = self.h_scroll.saturating_add(4),
            KeyCode::Char('v') => {
                self.show_headers = !self.show_headers;
                self.rebuild_visible_diff();
            }
            KeyCode::Char('S') => {
                self.scope = self.scope.next();
                self.load_diff();
            }
            KeyCode::Char('s') => self.stage_selected(true),
            KeyCode::Char('u') => self.stage_selected(false),
            KeyCode::Char('e') | KeyCode::Char('o') => self.open_selected(),
            KeyCode::Char('/') => {
                self.searching = true;
                self.search.clear();
            }
            KeyCode::Char('n') => self.jump_search(true),
            KeyCode::Char('N') => self.jump_search(false),
            KeyCode::Char('r') => self.reload_sync(),
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
        Flow::Continue
    }

    fn refresh(&mut self) {
        self.reload_async();
    }

    fn every(&self) -> Duration {
        Duration::from_secs(2)
    }
}

fn branch_text(r: &Repo) -> String {
    let mut s = platform::sanitize_display(&r.branch.name);
    if r.branch.ahead > 0 {
        s += &format!(" ↑{}", r.branch.ahead);
    }
    if r.branch.behind > 0 {
        s += &format!(" ↓{}", r.branch.behind);
    }
    s
}

fn draw(f: &mut Frame, v: &mut DiffView) {
    let t = v.theme;
    let area = f.area();
    if v.in_hub {
        let meta = match &v.repo {
            Some(r) => format!("{} · {} files", branch_text(r), r.changes.len()),
            None => "not git".into(),
        };
        crate::hub::set_pane_meta(&mut v.meta_cache, &meta);
    }

    let active_toast = v
        .toast
        .as_ref()
        .filter(|x| Instant::now() < x.until)
        .cloned();
    let (inner, footer) = if v.in_hub {
        if area.height <= 1 {
            (area, None)
        } else {
            (
                Rect {
                    height: area.height - 1,
                    ..area
                },
                Some(Rect {
                    y: area.y + area.height - 1,
                    height: 1,
                    ..area
                }),
            )
        }
    } else {
        let w = area.width as usize;
        let right = match &v.repo {
            Some(r) => {
                let total_a: u32 = r.changes.iter().filter_map(|c| c.added).sum();
                let total_d: u32 = r.changes.iter().filter_map(|c| c.deleted).sum();
                let room = w.saturating_sub(12);
                let summary = if room >= 28 {
                    format!(
                        "{} files · +{total_a} -{total_d} · {}",
                        r.changes.len(),
                        branch_text(r)
                    )
                } else {
                    branch_text(r)
                };
                if room >= 6 {
                    Line::from(Span::styled(
                        format!(" {} ", cut(&summary, room - 2)),
                        t.title_muted(),
                    ))
                    .right_aligned()
                } else {
                    Line::default()
                }
            }
            None => Line::default(),
        };
        let bottom = match &active_toast {
            Some(toast) => toast_line(toast, w.saturating_sub(4), &t),
            None => hint_line(&v.hints, w.saturating_sub(4), &t),
        };
        let block = Block::bordered()
            .border_type(t.border_type())
            .border_style(t.border_style(v.focused))
            .title_top(
                Line::from(Span::styled(
                    format!(" {} diff ", t.star()),
                    t.title_accent(),
                ))
                .left_aligned(),
            )
            .title_top(right)
            .title_bottom(bottom);
        let inn = block.inner(area);
        f.render_widget(block, area);
        (inn, None)
    };

    if let Some(foot) = footer {
        let fw = foot.width as usize;
        let line = if v.searching {
            let q = cut_tail(&v.search, fw.saturating_sub(4));
            Line::from(vec![
                Span::styled(" /", t.title_accent()),
                Span::raw(q),
                Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
            ])
        } else {
            match &active_toast {
                Some(toast) => toast_line(toast, fw, &t),
                None => hint_line(&v.hints, fw, &t),
            }
        };
        f.render_widget(Paragraph::new(line), foot);
    }

    v.list_area = Rect::default();
    v.diff_area = Rect::default();
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let Some(repo) = &v.repo else {
        center_msg(f, inner, "not a git repository", &t);
        return;
    };
    if repo.changes.is_empty() {
        let check = if t.ascii { "+" } else { "✓" };
        let msg = format!("{check} working tree clean · {}", branch_text(repo));
        center_msg(f, inner, &msg, &t);
        return;
    }

    let n = repo.changes.len();
    let (list_pane, sep, diff_pane) = if inner.width >= SIDE_BY_SIDE {
        let lw = (inner.width / 3).clamp(22, 46);
        let list = Rect { width: lw, ..inner };
        let sep = Rect {
            x: inner.x + lw,
            width: 1,
            ..inner
        };
        let diff = Rect {
            x: inner.x + lw + 1,
            width: inner.width - lw - 1,
            ..inner
        };
        (list, sep, diff)
    } else {
        let lh = (n as u16 + 1)
            .min((inner.height / 3).max(4))
            .min(inner.height);
        let list = Rect {
            height: lh,
            ..inner
        };
        let sep = Rect {
            y: inner.y + lh,
            height: 1.min(inner.height - lh),
            ..inner
        };
        let diff = Rect {
            y: sep.y + sep.height,
            height: inner.height - lh - sep.height,
            ..inner
        };
        (list, sep, diff)
    };

    // Sub-pane divider.
    let div_style = t.fg(Style::default(), t.border_color());
    let sep_lines: Vec<Line> = if sep.width == 1 {
        (0..sep.height)
            .map(|_| Line::from(Span::styled("│", div_style)))
            .collect()
    } else {
        vec![Line::from(Span::styled(
            "─".repeat(sep.width as usize),
            div_style,
        ))]
    };
    f.render_widget(Paragraph::new(sep_lines), sep);

    // Optional focus sub-headers when tall enough.
    let (list_hdr, list_body) = if list_pane.height >= 3 {
        (
            Some(Rect {
                height: 1,
                ..list_pane
            }),
            Rect {
                y: list_pane.y + 1,
                height: list_pane.height - 1,
                ..list_pane
            },
        )
    } else {
        (None, list_pane)
    };
    let (diff_hdr, diff_body) = if diff_pane.height >= 3 {
        (
            Some(Rect {
                height: 1,
                ..diff_pane
            }),
            Rect {
                y: diff_pane.y + 1,
                height: diff_pane.height - 1,
                ..diff_pane
            },
        )
    } else {
        (None, diff_pane)
    };

    if let Some(lh_rect) = list_hdr {
        let st = if v.focused && v.focus == Focus::List {
            t.title_accent()
        } else {
            t.muted().add_modifier(Modifier::BOLD)
        };
        let label = format!(" FILES ({n})");
        f.render_widget(
            Paragraph::new(Span::styled(cut(&label, lh_rect.width as usize), st)),
            lh_rect,
        );
    }

    // File list.
    v.list_area = list_body;
    let lh = list_body.height as usize;
    if v.selected < v.list_off {
        v.list_off = v.selected;
    } else if lh > 0 && v.selected >= v.list_off + lh {
        v.list_off = v.selected + 1 - lh;
    }
    let need_list_sb = n > lh && list_body.width > 12;
    let list_draw = if need_list_sb {
        Rect {
            width: list_body.width - 1,
            ..list_body
        }
    } else {
        list_body
    };
    let lw = list_draw.width as usize;
    let mut lines = Vec::new();
    for (i, c) in repo.changes.iter().enumerate().skip(v.list_off).take(lh) {
        let sel = i == v.selected;
        let mut base = Style::default();
        if sel {
            let bg = if v.focused && v.focus == Focus::List {
                t.sel_bg()
            } else {
                t.sel_bg_dim()
            };
            if let Some(bg) = bg {
                base = base.bg(bg);
            }
        }
        let bar = if sel && v.focus == Focus::List {
            "▌"
        } else {
            " "
        };
        let bar_style = if v.focused {
            t.fg(base, t.accent())
        } else {
            t.fg(base, t.border_color())
        };
        let code = c.code();
        let stat_str = if lw >= 30 {
            match (c.added, c.deleted) {
                (Some(a), Some(d)) if a > 0 || d > 0 => format!(" +{a} -{d}"),
                _ => String::new(),
            }
        } else {
            String::new()
        };
        let stat_w = sw(&stat_str);
        let name_budget = lw.saturating_sub(1 + sw(&code) + 1 + stat_w + 1);
        let name = cut_mid(&git::printable(&c.path), name_budget);
        let name_style = if sel {
            t.fg(base, t.accent()).add_modifier(Modifier::BOLD)
        } else {
            base
        };
        let used = 1 + sw(&code) + 1 + sw(&name) + stat_w;
        let pad = lw.saturating_sub(used);
        let mut spans = vec![
            Span::styled(bar, bar_style),
            Span::styled(
                code,
                t.fg(base, mark_color(c.mark(), &t))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", base),
            Span::styled(name, name_style),
            Span::styled(" ".repeat(pad), base),
        ];
        if !stat_str.is_empty() {
            if let (Some(a), Some(d)) = (c.added, c.deleted) {
                spans.push(Span::styled(format!(" +{a}"), t.fg(base, t.diff_add_fg())));
                spans.push(Span::styled(format!(" -{d}"), t.fg(base, t.diff_del_fg())));
            }
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), list_draw);
    if need_list_sb {
        let sb_area = Rect {
            x: list_body.x + list_body.width - 1,
            width: 1,
            ..list_body
        };
        render_scrollbar(f, sb_area, n, v.list_off, lh, &t);
    }

    // Diff pane header.
    if let Some(dh_rect) = diff_hdr {
        let dw_h = dh_rect.width as usize;
        if v.searching {
            let q = cut_tail(&v.search, dw_h.saturating_sub(4));
            let line = Line::from(vec![
                Span::styled(" /", t.title_accent()),
                Span::raw(q),
                Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
            ]);
            f.render_widget(Paragraph::new(line), dh_rect);
        } else {
            let cur_file = v
                .diff_path
                .as_deref()
                .map(git::printable)
                .unwrap_or_default();
            let scope_tag = if v.scope != DiffScope::All {
                format!(" [{}]", v.scope.label())
            } else {
                String::new()
            };
            let pos = if v.diff.is_empty() {
                String::new()
            } else {
                format!("{}/{}", (v.scroll + 1).min(v.diff.len()), v.diff.len())
            };
            let hdr_style = if v.focused && v.focus == Focus::Diff {
                t.title_accent()
            } else {
                t.muted().add_modifier(Modifier::BOLD)
            };
            let left = cut_mid(
                &format!(" {cur_file}{scope_tag}"),
                dw_h.saturating_sub(sw(&pos) + 2),
            );
            let gap = dw_h.saturating_sub(sw(&left) + sw(&pos) + 1);
            let line = Line::from(vec![
                Span::styled(left, hdr_style),
                Span::raw(" ".repeat(gap)),
                Span::styled(pos, t.muted()),
            ]);
            f.render_widget(Paragraph::new(line), dh_rect);
        }
    }

    // Diff body.
    v.diff_area = diff_body;
    let dh = diff_body.height as usize;
    v.page = dh;
    let need_diff_sb = v.diff.len() > dh && diff_body.width > 12;
    let diff_draw = if need_diff_sb {
        Rect {
            width: diff_body.width - 1,
            ..diff_body
        }
    } else {
        diff_body
    };
    let dw = diff_draw.width as usize;
    let show_gutter = dw >= 48;
    let max_ln = v
        .diff
        .iter()
        .filter_map(|dl| dl.old_no.max(dl.new_no))
        .max()
        .unwrap_or(0);
    let col_w = if max_ln >= 10_000 {
        max_ln.to_string().len()
    } else {
        4
    };
    let gutter_w = if show_gutter { col_w * 2 + 2 } else { 1 };
    let code_w = dw.saturating_sub(gutter_w);
    let q_lower = if !v.search.is_empty() {
        Some(v.search.to_ascii_lowercase())
    } else {
        None
    };

    let lines: Vec<Line> = v
        .diff
        .iter()
        .skip(v.scroll)
        .take(dh)
        .map(|dl| {
            let mut st = line_style(&t, dl.kind);
            if let Some(q) = &q_lower {
                if dl.text.to_ascii_lowercase().contains(q) {
                    st = st.add_modifier(Modifier::BOLD);
                }
            }
            let slice = if v.h_scroll > 0 {
                dl.text.chars().skip(v.h_scroll).collect::<String>()
            } else {
                dl.text.clone()
            };
            let text_shown = cut(&slice, code_w);
            let fill = code_w.saturating_sub(sw(&text_shown));
            let mut spans = Vec::new();
            if show_gutter {
                let old_s = dl
                    .old_no
                    .map_or_else(|| " ".repeat(col_w), |n| format!("{n:>col_w$}"));
                let new_s = dl
                    .new_no
                    .map_or_else(|| " ".repeat(col_w), |n| format!("{n:>col_w$}"));
                let g_style = t.muted();
                spans.push(Span::styled(
                    format!("{old_s} {new_s}{}", t.scroll_track()),
                    g_style,
                ));
            } else {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(text_shown, st));
            if st.bg.is_some() && fill > 0 {
                spans.push(Span::styled(" ".repeat(fill), st));
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), diff_draw);
    if need_diff_sb {
        let sb_area = Rect {
            x: diff_body.x + diff_body.width - 1,
            width: 1,
            ..diff_body
        };
        render_scrollbar(f, sb_area, v.diff.len(), v.scroll, dh, &t);
    }

    if v.show_help {
        let row = |k: &str, d: &str| {
            Line::from(vec![
                Span::styled(format!("{k:<10}"), t.title_accent()),
                Span::raw(d.to_string()),
            ])
        };
        let body = vec![
            row("j/k", "Move selection / scroll diff"),
            row("Tab", "Switch focus Files ↔ Diff"),
            row("[ / ]", "Previous / next hunk"),
            row("g / G", "Jump top / bottom of focused pane"),
            row("h / l", "Scroll diff left / right"),
            row("s / u", "Stage / unstage selected file"),
            row("S", "Cycle scope: all / unstaged / staged"),
            row("v", "Toggle raw git diff headers"),
            row("/", "Search in diff (n/N next/prev)"),
            row("e / o", "Open file"),
        ];
        render_modal(
            f,
            area,
            "diff keys",
            body,
            &[("esc", "close"), ("?", "close")],
            &t,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Branch;
    use crate::tui::App;

    fn repo() -> Repo {
        let ch = |x, y, p: &str| Change {
            x,
            y,
            path: p.into(),
            orig: None,
            added: Some(2),
            deleted: Some(1),
        };
        Repo {
            root: "/r".into(),
            prefix: String::new(),
            branch: Branch {
                name: "main".into(),
                upstream: None,
                ahead: 2,
                behind: 1,
                has_commits: true,
            },
            changes: vec![
                ch(' ', 'M', "src/main.rs"),
                ch('?', '?', "docs/a-very-long-file-name.md"),
                ch('D', ' ', "old.txt"),
            ],
        }
    }

    fn diff() -> Vec<(LineKind, String)> {
        git::classify_all("diff --git a/src/main.rs b/src/main.rs\n@@ -1,2 +1,2 @@\n fn main() {\n-    old();\n+    new_function_with_a_long_name();\n }\n")
    }

    #[test]
    fn render_narrow() {
        let mut v = DiffView::with_repo(Some(repo()), diff());
        let out = tui::render_text(30, 14, |f| draw(f, &mut v));
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].starts_with("╭ ✦ diff ") && lines[0].contains("main ↑2 ↓1"),
            "{}",
            lines[0]
        );
        assert!(lines[2].starts_with("│▌ M src/main.rs"), "{}", lines[2]);
        assert!(
            lines[3].contains("?? docs/") && lines[3].contains("…"),
            "{}",
            lines[3]
        );
        assert!(lines[4].contains("D  old.txt"), "{}", lines[4]);
        assert!(out.contains("+    new_function_with_a_l…"), "{out}");
        for l in &lines {
            assert!(l.chars().count() <= 30, "{l}");
        }
    }

    #[test]
    fn render_wide_and_states() {
        let mut v = DiffView::with_repo(Some(repo()), diff());
        let out = tui::render_text(90, 9, |f| draw(f, &mut v));
        assert!(out.lines().nth(1).unwrap().contains("FILES (3)"), "{out}");
        assert!(out.lines().nth(2).unwrap().contains("│"), "{out}");

        let mut none = DiffView::with_repo(None, Vec::new());
        assert!(tui::render_text(25, 5, |f| draw(f, &mut none)).contains("not a git repository"));
        let mut clean = DiffView::with_repo(
            Some(Repo {
                changes: vec![],
                ..repo()
            }),
            Vec::new(),
        );
        assert!(tui::render_text(50, 5, |f| draw(f, &mut clean)).contains("working tree clean"));
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
        // g/G in List focus selects first/last file.
        v.key(KeyEvent::from(KeyCode::Tab));
        v.key(KeyEvent::from(KeyCode::Char('G')));
        assert_eq!(v.selected, 2);
        v.key(KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(v.selected, 0);
        assert_eq!(v.key(KeyEvent::from(KeyCode::Char('q'))), Flow::Quit);
    }
}
