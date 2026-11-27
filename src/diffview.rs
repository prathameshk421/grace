//! `grace diff`: changed files and a colored diff of the selected file.
use crate::git::{self, Change, DiffLine, DiffScope, LineKind, Mark, Repo};
use crate::platform;
use crate::theme::{Theme, ToastKind};
use crate::tui::{self, Flow};
use crate::widgets::{
    center_msg, cut, cut_mid, cut_tail, hint_line, is_help_dismiss, is_plain_char, pane_frame,
    render_help_modal, render_scrollbar, slice_scrollbar, spaces, sw, toast_line, Toast,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

const HINTS: [(&str, &str); 6] = [
    ("j/k", "move"),
    ("⇥", "focus"),
    ("[/]", "hunk"),
    ("/", "find"),
    ("s/u", "stage"),
    ("?", "keys"),
];
const HELP_ROWS: [(&str, &str); 10] = [
    ("j/k", "Move selection / scroll diff"),
    ("Tab", "Switch focus Files ↔ Diff"),
    ("[ / ]", "Previous / next hunk"),
    ("g / G", "Jump top / bottom of focused pane"),
    ("h / l", "Scroll diff left / right"),
    ("s / u", "Stage / unstage selected file"),
    ("S", "Cycle scope: all / unstaged / staged"),
    ("v", "Toggle raw git diff headers"),
    ("/", "Search in diff (n/N next/prev)"),
    ("e / o", "Open file"),
];
const SIDE_BY_SIDE: u16 = 70;
const DIFF_DEBOUNCE: Duration = Duration::from_millis(35);
const MIN_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(8);
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Diff,
}

enum BgDiffResult {
    Unchanged(Option<git::RepoKey>, git::StatusFingerprint),
    Updated {
        key: Option<git::RepoKey>,
        fp: Option<git::StatusFingerprint>,
        repo: Option<Repo>,
        scope: DiffScope,
        lines: Vec<DiffLine>,
        path: Option<String>,
    },
}
type FileDiffResult = (u64, DiffScope, Option<String>, Vec<DiffLine>);

pub struct DiffView {
    dir: PathBuf,
    editor: Option<String>,
    repo: Option<Repo>,
    repo_key: Option<git::RepoKey>,
    last_fp: Option<git::StatusFingerprint>,
    selected: usize,
    list_off: usize,
    raw_diff: Vec<DiffLine>,
    diff: Vec<usize>,
    max_ln: u32,
    diff_path: Option<String>,
    scope: DiffScope,
    show_headers: bool,
    scroll: usize,
    h_scroll: usize,
    page: usize,
    focus: Focus,
    focused: bool,
    in_hub: bool,
    hub_vis: crate::hub::HubVisibility,
    meta_cache: String,
    theme: Theme,
    hints: [(&'static str, &'static str); 6],
    searching: bool,
    search: String,
    show_help: bool,
    toast: Option<Toast>,
    list_area: Rect,
    diff_area: Rect,
    bg_rx: Option<Receiver<BgDiffResult>>,
    bg_started: Option<Instant>,
    diff_rx: Option<Receiver<FileDiffResult>>,
    diff_started: Option<Instant>,
    diff_seq: u64,
    pending_diff_at: Option<Instant>,
    last_bg: Instant,
    backoff: Duration,
    force_refresh: bool,
    checked_once: bool,
}

pub fn mark_color(m: Mark, theme: Theme) -> Color {
    match m {
        Mark::Modified => theme.accent(),
        Mark::Added => theme.diff_add_fg(),
        Mark::Deleted => theme.diff_del_fg(),
        Mark::Renamed => theme.diff_hunk_fg(),
        Mark::Conflict => theme.diff_conflict_fg(),
    }
}

fn line_style(t: Theme, k: LineKind) -> Style {
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

fn contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.is_ascii() {
        let nb = needle.as_bytes();
        return haystack
            .as_bytes()
            .windows(nb.len())
            .any(|w| w.eq_ignore_ascii_case(nb));
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn slice_cols(s: &str, skip: usize) -> &str {
    if skip == 0 || s.is_empty() {
        return s;
    }
    if s.is_ascii() {
        return s.get(skip..).unwrap_or("");
    }
    let mut col = 0usize;
    for (i, ch) in s.char_indices() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if col >= skip && w > 0 {
            return &s[i..];
        }
        col += w;
    }
    ""
}

pub fn open_in_editor_or_default(
    full: &std::path::Path,
    editor: Option<&str>,
) -> Result<bool, &'static str> {
    if let Some(ed) = editor.map(str::trim).filter(|s| !s.is_empty()) {
        let mut parts = ed.split_whitespace();
        if let Some(prog) = parts.next() {
            if !crate::tools::is_terminal_editor(prog) {
                let mut cmd = std::process::Command::new(prog);
                cmd.args(parts).arg(full);
                platform::apply_no_window(&mut cmd);
                if cmd
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .is_ok()
                {
                    return Ok(true);
                }
            }
        }
    }
    if platform::open_path(full) {
        Ok(true)
    } else {
        Err("no opener found")
    }
}

impl DiffView {
    pub fn in_hub(mut self, yes: bool) -> DiffView {
        self.in_hub = yes;
        self.hub_vis = crate::hub::HubVisibility::new(yes);
        if yes {
            self.hints[5] = ("q", "hide");
        }
        self
    }

    pub fn with_editor(mut self, editor: Option<String>) -> DiffView {
        self.editor = editor;
        self
    }

    pub fn new(dir: PathBuf, theme: Theme) -> DiffView {
        let mut v = DiffView {
            dir,
            editor: None,
            repo: None,
            repo_key: None,
            last_fp: None,
            selected: 0,
            list_off: 0,
            raw_diff: Vec::new(),
            diff: Vec::new(),
            max_ln: 0,
            diff_path: None,
            scope: DiffScope::All,
            show_headers: false,
            scroll: 0,
            h_scroll: 0,
            page: 10,
            focus: Focus::List,
            focused: true,
            in_hub: false,
            hub_vis: crate::hub::HubVisibility::new(false),
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
            bg_started: None,
            diff_rx: None,
            diff_started: None,
            diff_seq: 0,
            pending_diff_at: None,
            last_bg: Instant::now() - Duration::from_secs(30),
            backoff: MIN_BACKOFF,
            force_refresh: false,
            checked_once: false,
        };
        v.reload_async(false);
        v
    }

    #[cfg(test)]
    fn with_repo(repo: Option<Repo>, raw_diff: Vec<DiffLine>) -> DiffView {
        let path = repo
            .as_ref()
            .and_then(|r| r.changes.first())
            .map(|c| c.path.clone());
        let mut v = DiffView {
            dir: PathBuf::from("."),
            editor: None,
            repo,
            repo_key: None,
            last_fp: None,
            selected: 0,
            list_off: 0,
            raw_diff,
            diff: Vec::new(),
            max_ln: 0,
            diff_path: path,
            scope: DiffScope::All,
            show_headers: false,
            scroll: 0,
            h_scroll: 0,
            page: 10,
            focus: Focus::List,
            focused: true,
            in_hub: false,
            hub_vis: crate::hub::HubVisibility::new(false),
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
            bg_started: None,
            diff_rx: None,
            diff_started: None,
            diff_seq: 0,
            pending_diff_at: None,
            last_bg: Instant::now(),
            backoff: MIN_BACKOFF,
            force_refresh: false,
            checked_once: true,
        };
        v.rebuild_visible_diff();
        v
    }

    fn changes(&self) -> &[Change] {
        self.repo.as_ref().map_or(&[], |r| &r.changes)
    }

    fn rebuild_visible_diff(&mut self) {
        self.diff.clear();
        if self.show_headers {
            self.diff.extend(0..self.raw_diff.len());
        } else {
            for (i, l) in self.raw_diff.iter().enumerate() {
                if l.kind != LineKind::Header {
                    self.diff.push(i);
                }
            }
            if self.diff.is_empty() {
                self.diff.extend(0..self.raw_diff.len());
            }
        }
        self.max_ln = self
            .diff
            .iter()
            .filter_map(|&idx| {
                let dl = &self.raw_diff[idx];
                dl.old_no.max(dl.new_no)
            })
            .max()
            .unwrap_or(0);
        self.scroll = self.scroll.min(self.diff.len().saturating_sub(1));
    }

    fn reload_sync(&mut self) {
        self.bg_rx = None;
        self.bg_started = None;
        self.last_fp = None;
        self.repo_key = git::resolve_repo(&self.dir);
        self.backoff = MIN_BACKOFF;
        self.last_bg = Instant::now();
        self.checked_once = true;
        let keep = self.changes().get(self.selected).map(|c| c.path.clone());
        if let Some(ref key) = self.repo_key {
            if let Some((Some(repo), fp)) = git::status_with_key(key, true, None) {
                self.repo = Some(repo);
                self.last_fp = Some(fp);
            } else {
                self.repo = None;
            }
        } else {
            self.repo = None;
        }
        let n = self.changes().len();
        self.selected = keep
            .and_then(|p| self.changes().iter().position(|c| c.path == p))
            .unwrap_or(self.selected.min(n.saturating_sub(1)));
        self.load_diff();
    }

    fn reload_async(&mut self, force: bool) {
        if self.bg_rx.is_some() {
            return;
        }
        let vis = self.hub_vis.poll(force, false);
        if !vis.visible() {
            return;
        }
        if vis.became_visible || force {
            self.backoff = MIN_BACKOFF;
        } else if self.last_bg.elapsed() < self.backoff {
            return;
        }
        self.last_bg = Instant::now();
        let dir = self.dir.clone();
        let mut repo_key = self.repo_key.clone();
        let prev_fp = if self.repo.is_some() && !force {
            self.last_fp
        } else {
            None
        };
        let had_repo = self.repo.is_some();
        let checked_once = self.checked_once;
        let keep = self.changes().get(self.selected).map(|c| c.path.clone());
        let selected_idx = self.selected;
        let scope = self.scope;
        let (tx, rx) = mpsc::channel();
        self.bg_rx = Some(rx);
        self.bg_started = Some(Instant::now());
        std::thread::spawn(move || {
            if repo_key.is_none() {
                repo_key = git::resolve_repo(&dir);
            }
            let status = if let Some(ref k) = repo_key {
                match git::status_with_key(k, true, prev_fp.as_ref()) {
                    Some(r) => Some(r),
                    None => {
                        repo_key = git::resolve_repo(&dir);
                        repo_key
                            .as_ref()
                            .and_then(|k2| git::status_with_key(k2, true, None))
                    }
                }
            } else {
                None
            };
            let msg = match status {
                Some((None, fp)) => BgDiffResult::Unchanged(repo_key, fp),
                Some((Some(r), fp)) => {
                    let idx = keep
                        .as_ref()
                        .and_then(|p| r.changes.iter().position(|c| &c.path == p))
                        .unwrap_or(selected_idx.min(r.changes.len().saturating_sub(1)));
                    let (lines, path) = if let Some(c) = r.changes.get(idx) {
                        (git::file_diff(&r, c, scope), Some(c.path.clone()))
                    } else {
                        (Vec::new(), None)
                    };
                    BgDiffResult::Updated {
                        key: repo_key,
                        fp: Some(fp),
                        repo: Some(r),
                        scope,
                        lines,
                        path,
                    }
                }
                None => {
                    if !had_repo && checked_once {
                        BgDiffResult::Unchanged(None, git::StatusFingerprint::default())
                    } else {
                        BgDiffResult::Updated {
                            key: None,
                            fp: None,
                            repo: None,
                            scope,
                            lines: Vec::new(),
                            path: None,
                        }
                    }
                }
            };
            let _ = tx.send(msg);
        });
    }

    fn load_diff(&mut self) {
        self.pending_diff_at = None;
        self.diff_rx = None;
        self.diff_started = None;
        let Some(repo) = self.repo.as_ref() else {
            self.raw_diff.clear();
            self.diff.clear();
            self.max_ln = 0;
            self.diff_path = None;
            return;
        };
        let Some(c) = repo.changes.get(self.selected) else {
            self.raw_diff.clear();
            self.diff.clear();
            self.max_ln = 0;
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

    fn schedule_diff_load(&mut self) {
        let Some(next_path) = self.changes().get(self.selected).map(|c| c.path.clone()) else {
            self.raw_diff.clear();
            self.diff.clear();
            self.max_ln = 0;
            self.diff_path = None;
            self.pending_diff_at = None;
            return;
        };
        if self.diff_path.as_deref() != Some(next_path.as_str()) {
            self.scroll = 0;
            self.h_scroll = 0;
            self.diff_path = Some(next_path);
        }
        self.diff_seq = self.diff_seq.wrapping_add(1);
        self.pending_diff_at = Some(Instant::now() + DIFF_DEBOUNCE);
    }

    fn spawn_diff_load(&mut self) {
        self.pending_diff_at = None;
        let Some(repo) = self.repo.as_ref() else {
            self.load_diff();
            return;
        };
        let Some(c) = repo.changes.get(self.selected) else {
            self.load_diff();
            return;
        };
        if self.diff_path.as_deref() != Some(c.path.as_str()) {
            self.scroll = 0;
            self.h_scroll = 0;
            self.diff_path = Some(c.path.clone());
        }
        self.diff_seq = self.diff_seq.wrapping_add(1);
        let seq = self.diff_seq;
        let scope = self.scope;
        let change = c.clone();
        let min_repo = Repo {
            root: repo.root.clone(),
            branch: git::Branch {
                has_commits: repo.branch.has_commits,
                ..git::Branch::default()
            },
            ..Repo::default()
        };
        let (tx, rx) = mpsc::channel();
        self.diff_rx = Some(rx);
        self.diff_started = Some(Instant::now());
        std::thread::spawn(move || {
            let path = Some(change.path.clone());
            let lines = git::file_diff(&min_repo, &change, scope);
            let _ = tx.send((seq, scope, path, lines));
        });
    }

    fn select(&mut self, delta: isize) {
        let n = self.changes().len();
        if n == 0 {
            return;
        }
        let s = (self.selected as isize + delta).clamp(0, n as isize - 1) as usize;
        if s != self.selected {
            self.selected = s;
            self.schedule_diff_load();
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
                .find(|&(_, &idx)| self.raw_diff[idx].kind == LineKind::Hunk)
                .map(|(i, _)| i)
            {
                self.scroll = pos;
            }
        } else if let Some(pos) = self.diff[..self.scroll]
            .iter()
            .rposition(|&idx| self.raw_diff[idx].kind == LineKind::Hunk)
        {
            self.scroll = pos;
        }
    }

    fn jump_search(&mut self, forward: bool) {
        if self.search.is_empty() || self.diff.is_empty() {
            return;
        }
        let q = &self.search;
        let raw = &self.raw_diff;
        let matches = |&idx: &usize| contains_ci(&raw[idx].text, q);
        if forward {
            let pos = self
                .diff
                .iter()
                .enumerate()
                .skip(self.scroll + 1)
                .find(|(_, idx)| matches(idx))
                .or_else(|| self.diff.iter().enumerate().find(|(_, idx)| matches(idx)))
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
        self.backoff = MIN_BACKOFF;
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
        match open_in_editor_or_default(&full, self.editor.as_deref()) {
            Ok(_) => self.flash(format!("opened {}", c.path), ToastKind::Success),
            Err(msg) => self.flash(msg, ToastKind::Warn),
        }
    }
    fn check_timeouts(&mut self) -> bool {
        let mut timed_out = false;
        if self.bg_rx.is_some() && self.bg_started.is_some_and(|t| t.elapsed() >= GIT_TIMEOUT) {
            self.bg_rx = None;
            self.bg_started = None;
            self.last_bg = Instant::now();
            self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
            self.flash("git refresh timed out", ToastKind::Warn);
            timed_out = true;
        }
        if self.diff_rx.is_some()
            && self
                .diff_started
                .is_some_and(|t| t.elapsed() >= GIT_TIMEOUT)
        {
            self.diff_rx = None;
            self.diff_started = None;
            self.flash("git diff timed out", ToastKind::Warn);
            timed_out = true;
        }
        timed_out
    }
}

impl tui::App for DiffView {
    fn draw(&mut self, f: &mut Frame) {
        draw(f, self);
    }

    fn focus(&mut self, focused: bool) {
        let gained = focused && !self.focused;
        self.focused = focused;
        if gained {
            self.backoff = MIN_BACKOFF;
            if self.last_bg.elapsed() >= MIN_BACKOFF {
                self.reload_async(true);
            }
        }
    }

    fn loading(&self) -> bool {
        self.bg_rx.is_some() || self.diff_rx.is_some() || self.pending_diff_at.is_some()
    }

    fn poll_bg(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.bg_rx {
            match rx.try_recv() {
                Ok(res) => {
                    self.bg_rx = None;
                    self.bg_started = None;
                    match res {
                        BgDiffResult::Unchanged(key, fp) => {
                            self.checked_once = true;
                            self.repo_key = key;
                            self.last_fp = if self.repo.is_some() { Some(fp) } else { None };
                            self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
                        }
                        BgDiffResult::Updated {
                            key,
                            fp,
                            repo,
                            scope: res_scope,
                            lines,
                            path,
                        } => {
                            self.checked_once = true;
                            self.repo_key = key;
                            self.last_fp = fp;
                            self.backoff = MIN_BACKOFF;
                            let keep = self.changes().get(self.selected).map(|c| c.path.clone());
                            self.repo = repo;
                            let n = self.changes().len();
                            self.selected = keep
                                .and_then(|p| self.changes().iter().position(|c| c.path == p))
                                .unwrap_or(self.selected.min(n.saturating_sub(1)));
                            let current_path =
                                self.changes().get(self.selected).map(|c| c.path.as_str());
                            if current_path == path.as_deref() && res_scope == self.scope {
                                let same = self.diff_path.as_deref() == path.as_deref();
                                self.raw_diff = lines;
                                if !same {
                                    self.scroll = 0;
                                    self.h_scroll = 0;
                                }
                                self.diff_path = path;
                                self.rebuild_visible_diff();
                            } else {
                                self.spawn_diff_load();
                            }
                            changed = true;
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.bg_rx = None;
                    self.bg_started = None;
                    if self.repo.is_none() {
                        changed = true;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.diff_rx {
            match rx.try_recv() {
                Ok((seq, res_scope, path, lines)) => {
                    self.diff_rx = None;
                    self.diff_started = None;
                    let current_path = self.changes().get(self.selected).map(|c| c.path.as_str());
                    if seq == self.diff_seq
                        && res_scope == self.scope
                        && current_path == path.as_deref()
                    {
                        let same = self.diff_path.as_deref() == path.as_deref();
                        self.raw_diff = lines;
                        if !same {
                            self.scroll = 0;
                            self.h_scroll = 0;
                        }
                        self.diff_path = path;
                        self.rebuild_visible_diff();
                        changed = true;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.diff_rx = None;
                    self.diff_started = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.check_timeouts() {
            changed = true;
        }
        if let Some(at) = self.pending_diff_at {
            if Instant::now() >= at {
                self.spawn_diff_load();
            }
        }
        changed
    }

    fn mouse(&mut self, m: MouseEvent) -> Flow {
        if !matches!(m.kind, MouseEventKind::Moved) {
            self.backoff = MIN_BACKOFF;
        }
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
                        self.spawn_diff_load();
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
        self.backoff = MIN_BACKOFF;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if self.show_help {
            if is_help_dismiss(&k) {
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
                KeyCode::Char('c') if ctrl => {
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
                KeyCode::Char(c) if is_plain_char(&k) => {
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
                    self.spawn_diff_load();
                }
                Focus::Diff => self.scroll = 0,
            },
            KeyCode::End | KeyCode::Char('G') => match self.focus {
                Focus::List => {
                    let n = self.changes().len();
                    if n > 0 {
                        self.selected = n - 1;
                        self.spawn_diff_load();
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
                self.spawn_diff_load();
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

    fn refresh(&mut self) -> bool {
        let expired = self
            .toast
            .as_ref()
            .is_some_and(|x| Instant::now() >= x.until);
        if expired {
            self.toast = None;
        }
        let timed_out = self.check_timeouts();
        let force = std::mem::take(&mut self.force_refresh);
        self.reload_async(force);
        expired || timed_out
    }

    fn every(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn wake(&mut self) {
        if !self.hub_vis.is_visible() {
            self.force_refresh = true;
            self.backoff = MIN_BACKOFF;
        }
    }

    fn wants_refresh_now(&self) -> bool {
        self.force_refresh
    }
}

fn branch_text(r: &Repo) -> String {
    let mut s = platform::sanitize_display(&r.branch.name).into_owned();
    if r.branch.ahead > 0 {
        s += &format!(" ↑{}", r.branch.ahead);
    }
    if r.branch.behind > 0 {
        s += &format!(" ↓{}", r.branch.behind);
    }
    s
}

fn search_line(q: &str, max_w: usize, t: Theme) -> Line<'static> {
    let shown = cut_tail(q, max_w.saturating_sub(4));
    Line::from(vec![
        Span::styled(" /", t.title_accent()),
        Span::raw(shown),
        Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
    ])
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
    let bw = if v.in_hub { w } else { w.saturating_sub(4) };
    let has_diff_hdr = !v.in_hub && {
        let inn_w = area.width.saturating_sub(2);
        let inn_h = area.height.saturating_sub(2);
        let n = v.repo.as_ref().map_or(0, |r| r.changes.len());
        inn_w > 0
            && inn_h > 0
            && n > 0
            && if inn_w >= SIDE_BY_SIDE {
                inn_h >= 3
            } else {
                let lh = (n as u16 + 1).min((inn_h / 3).max(4)).min(inn_h);
                let sep_h = 1.min(inn_h - lh);
                inn_h - lh - sep_h >= 3
            }
    };
    let bottom = if !v.in_hub && v.searching && !has_diff_hdr {
        search_line(&v.search, bw, t)
    } else {
        match &active_toast {
            Some(toast) => toast_line(toast, bw, t),
            None => hint_line(&v.hints, bw, t),
        }
    };
    let foot_line = |fw: usize| -> Line<'static> {
        if v.searching {
            search_line(&v.search, fw, t)
        } else {
            bottom.clone()
        }
    };
    let title_left = Line::from(Span::styled(
        format!(" {} diff ", t.star()),
        t.title_accent(),
    ))
    .left_aligned();
    let (inner, footer) = pane_frame(
        f,
        area,
        v.in_hub,
        v.focused,
        title_left,
        right,
        bottom.clone(),
        t,
    );

    v.list_area = Rect::default();
    v.diff_area = Rect::default();
    if inner.width == 0 || inner.height == 0 {
        if let Some(foot) = footer {
            f.render_widget(Paragraph::new(foot_line(foot.width as usize)), foot);
        }
        return;
    }
    let Some(repo) = &v.repo else {
        if let Some(foot) = footer {
            f.render_widget(Paragraph::new(foot_line(foot.width as usize)), foot);
        }
        let msg = if v.bg_rx.is_some() {
            "loading diff…"
        } else {
            "not a git repository"
        };
        center_msg(f, inner, msg, t);
        return;
    };
    if repo.changes.is_empty() {
        if let Some(foot) = footer {
            f.render_widget(Paragraph::new(foot_line(foot.width as usize)), foot);
        }
        let msg = format!(
            "{} working tree clean · {}",
            t.sym("✓", "+"),
            branch_text(repo)
        );
        center_msg(f, inner, &msg, t);
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
        let vr = t.v_rule();
        (0..sep.height)
            .map(|_| Line::from(Span::styled(vr, div_style)))
            .collect()
    } else {
        vec![Line::from(Span::styled(
            t.h_rule().repeat(sep.width as usize),
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

    if let Some(foot) = footer {
        let fw = foot.width as usize;
        let line = if v.searching && diff_hdr.is_none() {
            search_line(&v.search, fw, t)
        } else {
            bottom
        };
        f.render_widget(Paragraph::new(line), foot);
    }

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
    let (list_draw, list_sb) = slice_scrollbar(list_body, n, lh);
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
            t.bar_glyph()
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
        let name = cut_mid(&platform::sanitize_display(&c.path), name_budget);
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
                t.fg(base, mark_color(c.mark(), t))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", base),
            Span::styled(name, name_style),
            Span::styled(spaces(pad), base),
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
    if let Some(sb_area) = list_sb {
        render_scrollbar(f, sb_area, n, v.list_off, lh, t);
    }

    // Diff pane header.
    if let Some(dh_rect) = diff_hdr {
        let dw_h = dh_rect.width as usize;
        if v.searching {
            f.render_widget(Paragraph::new(search_line(&v.search, dw_h, t)), dh_rect);
        } else {
            let cur_file = v
                .diff_path
                .as_deref()
                .map(|p| platform::sanitize_display(p).into_owned())
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
                Span::raw(spaces(gap)),
                Span::styled(pos, t.muted()),
            ]);
            f.render_widget(Paragraph::new(line), dh_rect);
        }
    }

    // Diff body.
    v.diff_area = diff_body;
    let dh = diff_body.height as usize;
    v.page = dh;
    let (diff_draw, diff_sb) = slice_scrollbar(diff_body, v.diff.len(), dh);
    let dw = diff_draw.width as usize;
    let show_gutter = dw >= 48;
    let col_w = if v.max_ln >= 10_000 {
        v.max_ln.to_string().len()
    } else {
        4
    };
    let gutter_w = if show_gutter { col_w * 2 + 2 } else { 1 };
    let code_w = dw.saturating_sub(gutter_w);
    let empty_col = spaces(col_w);

    let lines: Vec<Line> = v
        .diff
        .iter()
        .skip(v.scroll)
        .take(dh)
        .map(|&idx| {
            let dl = &v.raw_diff[idx];
            let mut st = line_style(t, dl.kind);
            if !v.search.is_empty() && contains_ci(&dl.text, &v.search) {
                st = st.add_modifier(Modifier::BOLD);
            }
            let slice = slice_cols(&dl.text, v.h_scroll);
            let text_shown = cut(slice, code_w);
            let fill = code_w.saturating_sub(sw(&text_shown));
            let mut spans = Vec::new();
            if show_gutter {
                let g_style = t.muted();
                let gutter = match (dl.old_no, dl.new_no) {
                    (Some(o), Some(n)) => format!("{o:>col_w$} {n:>col_w$}{}", t.scroll_track()),
                    (Some(o), None) => {
                        format!("{o:>col_w$} {empty_col}{}", t.scroll_track())
                    }
                    (None, Some(n)) => {
                        format!("{empty_col} {n:>col_w$}{}", t.scroll_track())
                    }
                    (None, None) => {
                        format!("{empty_col} {empty_col}{}", t.scroll_track())
                    }
                };
                spans.push(Span::styled(gutter, g_style));
            } else {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(text_shown, st));
            if st.bg.is_some() && fill > 0 {
                spans.push(Span::styled(spaces(fill), st));
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), diff_draw);
    if let Some(sb_area) = diff_sb {
        render_scrollbar(f, sb_area, v.diff.len(), v.scroll, dh, t);
    }

    if v.show_help {
        render_help_modal(f, area, "diff keys", &HELP_ROWS, t);
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

    fn diff() -> Vec<DiffLine> {
        git::classify_lines("diff --git a/src/main.rs b/src/main.rs\n@@ -1,2 +1,2 @@\n fn main() {\n-    old();\n+    new_function_with_a_long_name();\n }\n")
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

    #[test]
    fn adaptive_backoff_and_reset() {
        let mut v = DiffView::with_repo(Some(repo()), diff());
        assert_eq!(v.backoff, MIN_BACKOFF);

        for expected in [
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(8),
        ] {
            let (tx, rx) = mpsc::channel();
            v.bg_rx = Some(rx);
            v.bg_started = Some(Instant::now());
            tx.send(BgDiffResult::Unchanged(
                None,
                git::StatusFingerprint::default(),
            ))
            .unwrap();
            assert!(!v.poll_bg());
            assert_eq!(v.backoff, expected);
        }

        // Elapsed < backoff skips reload_async without spawning.
        v.last_bg = Instant::now() - Duration::from_secs(4);
        v.reload_async(false);
        assert!(v.bg_rx.is_none());

        // Key interaction resets backoff to MIN_BACKOFF.
        v.key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(v.backoff, MIN_BACKOFF);

        // Updated result also resets backoff to MIN_BACKOFF.
        v.backoff = MAX_BACKOFF;
        let (tx, rx) = mpsc::channel();
        v.bg_rx = Some(rx);
        v.bg_started = Some(Instant::now());
        tx.send(BgDiffResult::Updated {
            key: None,
            fp: None,
            repo: Some(repo()),
            scope: DiffScope::All,
            lines: diff(),
            path: Some("src/main.rs".into()),
        })
        .unwrap();
        assert!(v.poll_bg());
        assert_eq!(v.backoff, MIN_BACKOFF);

        // Focus gained resets backoff.
        v.backoff = MAX_BACKOFF;
        v.last_bg = Instant::now();
        v.focus(false);
        v.focus(true);
        assert_eq!(v.backoff, MIN_BACKOFF);
    }

    #[test]
    fn timeout_clears_channels_and_sets_toast() {
        let mut v = DiffView::with_repo(Some(repo()), diff());
        let (_bg_tx, bg_rx) = mpsc::channel();
        v.bg_rx = Some(bg_rx);
        v.bg_started = Some(Instant::now() - GIT_TIMEOUT - Duration::from_secs(1));
        assert!(v.poll_bg());
        assert!(v.bg_rx.is_none());
        assert!(v.bg_started.is_none());
        assert_eq!(
            v.toast.as_ref().map(|t| t.msg.as_str()),
            Some("git refresh timed out")
        );

        v.toast = None;
        let (_diff_tx, diff_rx) = mpsc::channel();
        v.diff_rx = Some(diff_rx);
        v.diff_started = Some(Instant::now() - GIT_TIMEOUT - Duration::from_secs(1));
        assert!(v.refresh());
        assert!(v.diff_rx.is_none());
        assert!(v.diff_started.is_none());
        assert_eq!(
            v.toast.as_ref().map(|t| t.msg.as_str()),
            Some("git diff timed out")
        );
    }
}
