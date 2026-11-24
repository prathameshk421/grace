//! Interactive TUI picker.
//!
//! The TUI draws on stderr, because the shell function captures stdout.
//! By default it draws inline, below the prompt. `--full` uses the alternate screen.
use crate::config::Config;
use crate::editors_live::OpenEditor;
use crate::git::{self, RepoSummary};
use crate::model::{Entry, Session, Status};
use crate::platform;
pub use crate::platform::display_path;
use crate::rank::time_ago;
use crate::theme::{Theme, ToastKind};
use crate::widgets::{
    center_msg, cut, cut_tail, fit_end, fit_middle, hint_spans, is_help_dismiss, is_plain_char,
    push_fit_coalesced, render_help_modal, render_modal, render_scrollbar, slice_scrollbar, spaces,
    sw, toast_line, DoubleClick, Toast,
};
use anyhow::{bail, Result};
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config as MatcherConfig, Matcher, Utf32Str,
};
use ratatui::{
    backend::{Backend, ClearType, CrosstermBackend, WindowSize},
    buffer::Cell,
    layout::{Alignment, Position, Rect, Size},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame, Terminal, TerminalOptions, Viewport,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// Sessions by folder, one per folder: the most urgent, then the newest.
pub type Best = HashMap<String, Session>;

/// Higher means more urgent: waiting, then ready, then working, then done.
pub fn urgency(s: Status) -> u8 {
    match s {
        Status::Waiting => 3,
        Status::Ready => 2,
        Status::Working => 1,
        Status::Done => 0,
    }
}

pub fn best_by_path(sessions: &[Session]) -> Best {
    let mut map: Best = HashMap::new();
    for s in sessions {
        let better = match map.get(&s.path) {
            None => true,
            Some(o) => (urgency(s.status), s.updated_at) > (urgency(o.status), o.updated_at),
        };
        if better {
            map.insert(s.path.clone(), s.clone());
        }
    }
    map
}

/// Words that the search matches for a session status.
pub(crate) fn status_words(s: Status) -> &'static str {
    match s {
        Status::Waiting => "waiting needs",
        Status::Ready => "ready your turn",
        Status::Working => "working",
        Status::Done => "done",
    }
}

fn push_haystack(
    s: &mut String,
    e: &Entry,
    best: &Best,
    branches: &HashMap<String, (String, bool)>,
) {
    s.clear();
    s.push_str(&e.path);
    for t in &e.tags {
        s.push(' ');
        s.push_str(&t.tool);
    }
    if let Some((br, _)) = branches.get(&e.path) {
        s.push(' ');
        s.push_str(br);
    }
    if let Some(b) = best.get(&e.path) {
        s.push(' ');
        s.push_str(status_words(b.status));
        if let Some(d) = &b.detail {
            s.push(' ');
            s.push_str(d);
        }
    }
}

/// Return indices into `entries` that match `query`, best first.
pub(crate) fn filter(entries: &[Entry], best: &Best, query: &str) -> Vec<usize> {
    filter_with_branches(entries, best, &HashMap::new(), query)
}

pub(crate) fn filter_with_branches(
    entries: &[Entry],
    best: &Best,
    branches: &HashMap<String, (String, bool)>,
    query: &str,
) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..entries.len()).collect();
    }
    let mut matcher = Matcher::new(MatcherConfig::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut h = String::new();
    let mut scored: Vec<(u32, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            push_haystack(&mut h, e, best, branches);
            pattern
                .score(Utf32Str::new(&h, &mut buf), &mut matcher)
                .map(|s| (s, i))
        })
        .collect();
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    scored.into_iter().map(|(_, i)| i).collect()
}

// ---------------------------------------------------------------------------
// Public API

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Folders,
    Sessions,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditorChoice {
    pub prog: String,
    pub args: Vec<String>,
    pub in_terminal: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Enter: go to the folder.
    Travel,
    /// ctrl-o: go to the folder and start the AI tool used there last.
    Resume,
    /// ctrl-e: open or focus the folder in its editor.
    Editor(EditorChoice),
    /// ctrl-g: open the folder in `grace hub`.
    Hub,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub entry: Entry,
    pub action: Action,
}

/// Everything the picker shows.
#[derive(Debug, Clone, Default)]
pub struct Data {
    pub entries: Vec<Entry>,
    pub sessions: Vec<Session>,
    pub editors: Vec<OpenEditor>,
    pub branches: HashMap<String, (String, bool)>,
}

pub type EditorCmd<'a> = Box<dyn Fn(&Entry, Option<&OpenEditor>) -> Option<EditorChoice> + 'a>;
pub type LabelFn<'a> = Box<dyn Fn(&Entry) -> Option<String> + 'a>;
pub type EditorLabelFn<'a> = Box<dyn Fn(&Entry, Option<&OpenEditor>) -> Option<String> + 'a>;
pub type PinFn<'a> = Box<dyn FnMut(&Entry) -> Result<bool, String> + 'a>;
pub type ForgetFn<'a> = Box<dyn FnMut(&Entry) -> Result<(), String> + 'a>;

pub struct Opts<'a> {
    pub view: View,
    pub fullscreen: bool,
    pub tag_filter: Option<String>,
    pub initial_toast: Option<(String, ToastKind)>,
    pub shell_hook_missing: Option<String>,
    pub editor_cmd: EditorCmd<'a>,
    pub tool_label: LabelFn<'a>,
    pub editor_label: EditorLabelFn<'a>,
    pub toggle_pin: Option<PinFn<'a>>,
    pub forget_dir: Option<ForgetFn<'a>>,
    pub import_all_now: Option<Box<dyn FnMut() -> Result<String, String> + 'a>>,
    pub add_cwd_now: Option<Box<dyn FnMut() -> Result<String, String> + 'a>>,
    /// Runs off the UI thread after frame 1 and sends updated `Data`.
    pub after_first_frame: Option<Box<dyn FnOnce() -> Option<Data> + Send + 'static>>,
    /// Reads the data again for a view.
    pub reload: Option<Box<dyn FnMut(View) -> Option<Data> + 'a>>,
    pub reload_every: Option<Box<dyn Fn(View) -> Option<Duration> + 'a>>,
}

// ---------------------------------------------------------------------------
// Constants & Layout

const MAX_WIDTH: u16 = 116;
const MAX_FULL_WIDTH: u16 = 144;
const NAME_MAX: usize = 28;
const TAGS_MAX: usize = 32;
const PARENT_MAX: usize = 30;
const TICK: Duration = Duration::from_millis(100);
const SUMMARY_DEBOUNCE: Duration = Duration::from_millis(120);
const HELP_ROWS: [(&str, &str); 10] = [
    ("Enter", "Travel to selected folder"),
    ("Ctrl-O", "Travel and start last AI tool"),
    ("Ctrl-E", "Open or focus editor"),
    ("Ctrl-G", "Open folder in grace hub"),
    ("Tab", "Switch Folders / Sessions view"),
    ("Alt-P", "Pin or unpin folder"),
    ("Alt-I", "Toggle Inspector drawer"),
    ("Ctrl-Y", "Copy folder path"),
    ("Ctrl-D", "Forget folder (asks first)"),
    ("Ctrl-W/U", "Delete word / clear search"),
];

/// Split a shown path into `(parent, name)` without allocating.
pub fn split_path(shown: &str) -> (&str, &str) {
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    match shown.rfind(is_sep) {
        Some(p) if p + 1 < shown.len() => {
            let parent = if p == 0 {
                "/"
            } else if cfg!(windows) && p == 2 && shown.as_bytes().get(1) == Some(&b':') {
                &shown[..=p]
            } else {
                &shown[..p]
            };
            (parent, &shown[p + 1..])
        }
        _ => ("", shown),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cols {
    pub status: usize,
    pub name: usize,
    pub parent: usize,
    pub tags: usize,
    pub time: usize,
}

/// A row is: bar(1) status star(2) name gap(2) parent gap(2) tags gap(>=1) time pad(1).
pub fn layout_cols(
    inner: usize,
    status: usize,
    name_max: usize,
    parent_max: usize,
    tags_max: usize,
    time: usize,
) -> Cols {
    let fixed = 1 + status + 2 + 2 + 2 + 1 + time + 1;
    let mut name = name_max.min(NAME_MAX);
    if fixed + name > inner {
        name = inner.saturating_sub(fixed).max(1).min(name);
    }
    let avail = inner.saturating_sub(fixed + name);
    let parent_max = parent_max.min(PARENT_MAX);
    let tags = tags_max
        .min(TAGS_MAX)
        .min(avail.saturating_sub(parent_max.min(12)));
    let mut parent = parent_max.min(avail - tags);
    if parent < 4 {
        parent = 0;
    }
    Cols {
        status,
        name,
        parent,
        tags,
        time,
    }
}

/// How many tags fit in `w` columns, joined by " · ". The bool says if " +N" follows.
pub fn fit_tags(widths: &[usize], w: usize) -> (usize, bool) {
    let mut used = 0;
    for (i, &tw) in widths.iter().enumerate() {
        let add = if i == 0 { tw } else { 3 + tw };
        let more = widths.len() - i - 1;
        let reserve = if more > 0 {
            2 + more.to_string().len()
        } else {
            0
        };
        if used + add + reserve > w {
            let fits_more = used + 2 + (widths.len() - i).to_string().len() <= w;
            return (i, fits_more && i > 0);
        }
        used += add;
    }
    (widths.len(), false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Header(Status, usize),
    Item(usize),
}

const GROUPS: [Status; 4] = [
    Status::Waiting,
    Status::Ready,
    Status::Working,
    Status::Done,
];

pub fn build_rows(
    view: View,
    matches: &[usize],
    status: impl Fn(usize) -> Option<Status>,
) -> Vec<Row> {
    match view {
        View::Folders => matches.iter().map(|&i| Row::Item(i)).collect(),
        View::Sessions => {
            let mut rows = Vec::new();
            for g in GROUPS {
                let items: Vec<Row> = matches
                    .iter()
                    .filter(|&&i| status(i) == Some(g))
                    .map(|&i| Row::Item(i))
                    .collect();
                if !items.is_empty() {
                    rows.push(Row::Header(g, items.len()));
                    rows.extend(items);
                }
            }
            rows.extend(
                matches
                    .iter()
                    .filter(|&&i| status(i).is_none())
                    .map(|&i| Row::Item(i)),
            );
            rows
        }
    }
}

pub fn first_item(rows: &[Row]) -> Option<usize> {
    rows.iter().position(|r| matches!(r, Row::Item(_)))
}

pub fn step_item(rows: &[Row], from: usize, delta: isize) -> usize {
    let step = delta.signum();
    let mut left = delta.unsigned_abs();
    let mut i = from as isize;
    let mut cur = from;
    while left > 0 {
        i += step;
        if i < 0 || i as usize >= rows.len() {
            break;
        }
        if matches!(rows[i as usize], Row::Item(_)) {
            cur = i as usize;
            left -= 1;
        }
    }
    cur
}

pub fn scroll_by(
    len: usize,
    is_header: impl Fn(usize) -> bool,
    selected: usize,
    offset: usize,
    height: usize,
) -> usize {
    if height == 0 || len == 0 {
        return 0;
    }
    let mut off = offset.min(len - 1);
    if selected < off {
        off = selected;
    } else if selected >= off + height {
        off = selected + 1 - height;
    }
    if off == selected && off > 0 && height > 1 && is_header(off - 1) {
        off -= 1;
    }
    off
}

pub fn scroll(rows: &[Row], selected: usize, offset: usize, height: usize) -> usize {
    scroll_by(
        rows.len(),
        |idx| matches!(rows[idx], Row::Header(_, _)),
        selected,
        offset,
        height,
    )
}

/// Inline viewport height: the rows plus chrome, at most 48% of the terminal (min 10).
pub fn inline_height(rows: usize, term_h: u16) -> u16 {
    let chrome = 4;
    let cap = (term_h as usize * 48 / 100).max(11);
    ((rows.max(3) + chrome).min(cap) as u16).min(term_h.max(1))
}

// ---------------------------------------------------------------------------
// State

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modal {
    None,
    Help,
    ConfirmForget,
}

struct CachedWidths {
    name_max: usize,
    parent_max: usize,
    tags_max: usize,
    time_max: usize,
}

struct State {
    data: Data,
    best: Best,
    open: HashMap<String, Vec<OpenEditor>>,
    query: String,
    pattern: Option<Pattern>,
    atom_patterns: Vec<Pattern>,
    matches: Vec<usize>,
    rows: Vec<Row>,
    selected: usize,
    offset: usize,
    page: usize,
    now: i64,
    home: Option<String>,
    matcher: Matcher,
    utf32_buf: Vec<char>,
    idx_buf: Vec<u32>,
    haystack_buf: String,
    view: View,
    fullscreen: bool,
    tag_filter: Option<String>,
    shell_hook_missing: Option<String>,
    theme: Theme,
    start: Instant,
    toast: Option<Toast>,
    animating: bool,
    modal: Modal,
    inspector: bool,
    summaries: HashMap<String, Option<RepoSummary>>,
    summary_wait: Option<(String, Instant)>,
    pending_summary: Option<String>,
    summary_rx: Option<Receiver<(String, Option<RepoSummary>)>>,
    widths: CachedWidths,
    list_area: Rect,
    tab_folders_area: Rect,
    tab_sessions_area: Rect,
    last_click: DoubleClick,
    footer_hints: Vec<(&'static str, String)>,
}

impl State {
    #[allow(clippy::too_many_arguments)]
    fn new(
        data: Data,
        query: &str,
        now: i64,
        view: View,
        fullscreen: bool,
        tag_filter: Option<String>,
        shell_hook_missing: Option<String>,
        theme: Theme,
        home: Option<String>,
    ) -> State {
        let mut st = State {
            data: Data::default(),
            best: Best::new(),
            open: HashMap::new(),
            query: query.to_string(),
            pattern: None,
            atom_patterns: Vec::new(),
            matches: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            page: 10,
            now,
            home,
            matcher: Matcher::new(MatcherConfig::DEFAULT.match_paths()),
            utf32_buf: Vec::new(),
            idx_buf: Vec::new(),
            haystack_buf: String::new(),
            view,
            fullscreen,
            tag_filter,
            shell_hook_missing,
            theme,
            start: Instant::now(),
            toast: None,
            animating: false,
            modal: Modal::None,
            inspector: fullscreen,
            summaries: HashMap::new(),
            summary_wait: None,
            pending_summary: None,
            summary_rx: None,
            widths: CachedWidths {
                name_max: 0,
                parent_max: 0,
                tags_max: 0,
                time_max: 0,
            },
            list_area: Rect::default(),
            tab_folders_area: Rect::default(),
            tab_sessions_area: Rect::default(),
            last_click: DoubleClick::default(),
            footer_hints: Vec::new(),
        };
        st.update_pattern();
        st.set_data(data);
        st
    }

    fn update_pattern(&mut self) {
        self.atom_patterns.clear();
        if self.query.trim().is_empty() {
            self.pattern = None;
            return;
        }
        self.pattern = Some(Pattern::parse(
            &self.query,
            CaseMatching::Smart,
            Normalization::Smart,
        ));
        let mut words = self.query.split_whitespace();
        let first = words.next();
        if first.is_some() && words.next().is_some() {
            for atom in self.query.split_whitespace() {
                self.atom_patterns.push(Pattern::parse(
                    atom,
                    CaseMatching::Smart,
                    Normalization::Smart,
                ));
            }
        }
    }

    /// Replace the data. The query and the selected folder stay.
    fn set_data(&mut self, data: Data) {
        let keep = self.selected_entry().map(|e| e.path.clone());
        self.best = best_by_path(&data.sessions);
        self.open.clear();
        for o in &data.editors {
            self.open.entry(o.path.clone()).or_default().push(o.clone());
        }
        self.data = data;
        self.recompute_widths();
        self.refilter_keep(keep.as_deref());
    }

    fn recompute_widths(&mut self) {
        let (mut name_max, mut parent_max, mut tags_max, mut time_max) = (0, 0, 0, 0);
        for e in &self.data.entries {
            let raw_shown = display_path(&e.path, self.home.as_deref());
            let shown = platform::sanitize_display(&raw_shown);
            let (parent, name) = split_path(&shown);
            name_max = name_max.max(sw(name));
            parent_max = parent_max.max(sw(parent));

            let open_list = self.open.get(&e.path);
            let mut tw = 0usize;
            let mut n_tags = 0usize;
            for t in &e.tags {
                let w = sw(&platform::sanitize_display(&t.tool));
                tw += w + if n_tags > 0 { 3 } else { 0 };
                n_tags += 1;
            }
            if let Some(list) = open_list {
                for o in list {
                    if !e.tags.iter().any(|t| t.tool == o.tag) {
                        let w = sw(&platform::sanitize_display(&o.tag));
                        tw += w + if n_tags > 0 { 3 } else { 0 };
                        n_tags += 1;
                    }
                }
            }
            tags_max = tags_max.max(tw);

            let then = match (self.view, self.best.get(&e.path)) {
                (View::Sessions, Some(b)) => b.updated_at,
                _ => e.last_visit,
            };
            time_max = time_max.max(sw(&time_ago(then, self.now)));
        }
        self.widths = CachedWidths {
            name_max,
            parent_max,
            tags_max,
            time_max,
        };
    }

    fn status(&self, i: usize) -> Option<Status> {
        self.best.get(&self.data.entries[i].path).map(|s| s.status)
    }

    fn visible_entries_indices(&mut self) -> Vec<usize> {
        let filtered: Vec<usize> = if let Some(pat) = &self.pattern {
            let mut scored: Vec<(u32, usize)> = self
                .data
                .entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| {
                    push_haystack(&mut self.haystack_buf, e, &self.best, &self.data.branches);
                    pat.score(
                        Utf32Str::new(&self.haystack_buf, &mut self.utf32_buf),
                        &mut self.matcher,
                    )
                    .map(|s| (s, i))
                })
                .collect();
            scored.sort_by_key(|s| std::cmp::Reverse(s.0));
            scored.into_iter().map(|(_, i)| i).collect()
        } else {
            (0..self.data.entries.len()).collect()
        };
        if self.view == View::Sessions {
            let mut sess: Vec<usize> = filtered
                .into_iter()
                .filter(|&i| self.best.contains_key(&self.data.entries[i].path))
                .collect();
            if self.query.trim().is_empty() {
                sess.sort_by_key(|&i| {
                    let b = &self.best[&self.data.entries[i].path];
                    std::cmp::Reverse((urgency(b.status), b.updated_at))
                });
            }
            sess
        } else {
            filtered
        }
    }

    fn refilter_keep(&mut self, keep: Option<&str>) {
        self.update_pattern();
        self.matches = self.visible_entries_indices();
        self.rows = build_rows(self.view, &self.matches, |i| self.status(i));
        let entries = &self.data.entries;
        let kept = keep.and_then(|p| {
            self.rows
                .iter()
                .position(|r| matches!(r, Row::Item(i) if entries[*i].path == p))
        });
        self.selected = kept.or_else(|| first_item(&self.rows)).unwrap_or(0);
        if kept.is_none() {
            self.offset = 0;
        }
    }

    fn refilter(&mut self) {
        self.refilter_keep(None);
    }

    fn mv(&mut self, delta: isize) {
        self.selected = step_item(&self.rows, self.selected, delta);
    }

    fn selected_entry(&self) -> Option<&Entry> {
        match self.rows.get(self.selected) {
            Some(Row::Item(i)) => self.data.entries.get(*i),
            _ => None,
        }
    }

    fn selected_open_editor(&self, e: &Entry) -> Option<&OpenEditor> {
        self.open.get(&e.path).and_then(|list| list.first())
    }

    fn flash(&mut self, msg: impl Into<String>, kind: ToastKind) {
        self.toast = Some(Toast::new(msg, kind, 2200));
    }

    fn delete_word(&mut self) {
        let is_sep = |c: char| c.is_whitespace() || c == '/' || c == '\\';
        let trimmed = self.query.trim_end_matches(is_sep);
        let new_len = trimmed
            .char_indices()
            .rfind(|&(_, c)| is_sep(c))
            .map_or(0, |(i, c)| i + c.len_utf8());
        self.query.truncate(new_len);
        self.refilter();
    }

    fn maybe_fetch_summary(&mut self) -> bool {
        let mut received = false;
        if let Some(rx) = &self.summary_rx {
            if let Ok((p, sum)) = rx.try_recv() {
                self.summaries.insert(p.clone(), sum);
                if self.pending_summary.as_deref() == Some(p.as_str()) {
                    self.pending_summary = None;
                }
                self.summary_rx = None;
                received = true;
            }
        }
        if !self.inspector {
            self.summary_wait = None;
            return received;
        }
        let Some(e) = self.selected_entry() else {
            self.summary_wait = None;
            return received;
        };
        let path = e.path.clone();
        if self.summaries.contains_key(&path)
            || self.pending_summary.as_deref() == Some(path.as_str())
        {
            self.summary_wait = None;
            return received;
        }
        if self.summary_rx.is_some() {
            return received;
        }
        match &self.summary_wait {
            Some((p, since)) if p == &path => {
                if since.elapsed() < SUMMARY_DEBOUNCE {
                    return received;
                }
            }
            _ => {
                self.summary_wait = Some((path, Instant::now()));
                return received;
            }
        }
        self.summary_wait = None;
        let (tx, rx) = mpsc::channel();
        self.summary_rx = Some(rx);
        self.pending_summary = Some(path.clone());
        std::thread::spawn(move || {
            let sum = git::summary(Path::new(&path));
            let _ = tx.send((path, sum));
        });
        received
    }

    fn collect_hl(&mut self, target: &str) -> Vec<usize> {
        let Some(pat) = self.pattern.as_ref() else {
            return Vec::new();
        };
        self.idx_buf.clear();
        pat.indices(
            Utf32Str::new(target, &mut self.utf32_buf),
            &mut self.matcher,
            &mut self.idx_buf,
        );
        let mut all: Vec<usize> = self.idx_buf.iter().map(|&x| x as usize).collect();
        if !self.atom_patterns.is_empty() {
            for ap in &self.atom_patterns {
                self.idx_buf.clear();
                ap.indices(
                    Utf32Str::new(target, &mut self.utf32_buf),
                    &mut self.matcher,
                    &mut self.idx_buf,
                );
                all.extend(self.idx_buf.iter().map(|&x| x as usize));
            }
            all.sort_unstable();
            all.dedup();
        }
        all
    }

    /// Row content for entry `i`. With `hl`, computes match positions for name, parent, and tags.
    fn row_view(&mut self, i: usize, hl: bool) -> RowView {
        let e = &self.data.entries[i];
        let raw_shown = display_path(&e.path, self.home.as_deref());
        let shown = platform::sanitize_display(&raw_shown);
        let (parent_s, name_s) = split_path(&shown);
        let parent = parent_s.to_string();
        let name = name_s.to_string();
        let best_opt = self.best.get(&e.path).cloned();
        let open_list = self.open.get(&e.path);
        let editor_open = open_list.is_some();
        let mut tags: Vec<(String, bool)> = e
            .tags
            .iter()
            .map(|t| {
                let is_open = open_list.is_some_and(|list| list.iter().any(|o| o.tag == t.tool));
                (platform::sanitize_display(&t.tool).into_owned(), is_open)
            })
            .collect();
        if let Some(list) = open_list {
            for o in list.iter().rev() {
                if !tags.iter().any(|(t, _)| t == &o.tag) {
                    tags.insert(0, (platform::sanitize_display(&o.tag).into_owned(), true));
                }
            }
        }
        let then = match (self.view, best_opt.as_ref()) {
            (View::Sessions, Some(b)) => b.updated_at,
            _ => e.last_visit,
        };
        let pinned = e.pinned;
        let mut name_hl = Vec::new();
        let mut parent_hl = Vec::new();
        let mut tag_hl = Vec::new();
        if hl && self.pattern.is_some() {
            name_hl = self.collect_hl(&name);
            if !parent.is_empty() {
                parent_hl = self.collect_hl(&parent);
            }
            for (tag, _) in &tags {
                tag_hl.push(self.collect_hl(tag));
            }
        }
        RowView {
            name,
            parent,
            name_hl,
            parent_hl,
            tag_hl,
            pinned,
            session: best_opt,
            editor_open,
            tags,
            time: time_ago(then, self.now),
        }
    }
}

// ---------------------------------------------------------------------------
// Drawing

struct RowView {
    name: String,
    parent: String,
    name_hl: Vec<usize>,
    parent_hl: Vec<usize>,
    tag_hl: Vec<Vec<usize>>,
    pinned: bool,
    session: Option<Session>,
    editor_open: bool,
    tags: Vec<(String, bool)>,
    time: String,
}

struct Ctx {
    theme: Theme,
    view: View,
    frame: usize,
}

struct RowBuf {
    spans: Vec<Span<'static>>,
    used: usize,
    base: Style,
}

impl RowBuf {
    fn push(&mut self, text: impl Into<Cow<'static, str>>, style: Style) {
        let cow = text.into();
        self.used += sw(&cow);
        self.spans.push(Span::styled(cow, style));
    }
    fn pad(&mut self, n: usize) {
        if n > 0 {
            self.used += n;
            self.spans.push(Span::styled(spaces(n), self.base));
        }
    }
    fn pad_to(&mut self, to: usize) {
        self.pad(to.saturating_sub(self.used));
    }
    fn push_fit(&mut self, fit: &crate::widgets::Fit, hl: &[usize], style: Style, hl_style: Style) {
        push_fit_coalesced(&mut self.spans, &mut self.used, fit, hl, style, hl_style);
    }
}

fn compose_row(
    rv: &RowView,
    cols: &Cols,
    selected: bool,
    ctx: &Ctx,
    width: usize,
) -> Line<'static> {
    let t = ctx.theme;
    let mut base = Style::default();
    if selected {
        if let Some(bg) = t.sel_bg() {
            base = base.bg(bg);
        }
    }
    let hl_name = t
        .fg(base, if selected { t.accent_hi() } else { t.accent() })
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let hl_other = t
        .fg(base, t.accent())
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let mut b = RowBuf {
        spans: Vec::new(),
        used: 0,
        base,
    };

    // Accent bar.
    if selected {
        b.push(t.bar_glyph(), t.fg(base, t.accent()));
    } else {
        b.pad(1);
    }

    // Status glyph (+ tool/detail in sessions view). An agent wins over an open editor.
    let end = b.used + cols.status;
    if let Some(s) = &rv.session {
        let (g, _, style) = t.status_look(s.status, ctx.frame);
        let style = base.patch(t.apply(style));
        b.push(g.to_string(), style);
        if ctx.view == View::Sessions {
            let mut label = format!(" {}", platform::sanitize_display(&s.tool));
            if let Some(d) = &s.detail {
                label.push_str(&format!("·{}", platform::sanitize_display(d)));
            }
            let room = cols.status.saturating_sub(1);
            b.push(cut(&label, room), style);
        }
    } else if rv.editor_open {
        b.push(t.editor_glyph().to_string(), t.fg(base, t.open_blue()));
    }
    b.pad_to(end);

    // Pin.
    if rv.pinned {
        b.push(format!("{} ", t.pin_glyph()), t.fg(base, t.accent()));
    } else {
        b.pad(2);
    }

    // Name.
    let name_style = if selected {
        t.fg(base, t.accent()).add_modifier(Modifier::BOLD)
    } else {
        base.add_modifier(Modifier::BOLD)
    };
    let end = b.used + cols.name;
    b.push_fit(
        &fit_end(&rv.name, cols.name),
        &rv.name_hl,
        name_style,
        hl_name,
    );
    b.pad_to(end);

    // Parent.
    if cols.parent > 0 {
        b.pad(2);
        let end = b.used + cols.parent;
        let p_style = base.patch(t.muted());
        b.push_fit(
            &fit_middle(&rv.parent, cols.parent),
            &rv.parent_hl,
            p_style,
            hl_other,
        );
        b.pad_to(end);
    }

    // Tags.
    if cols.tags > 0 && !rv.tags.is_empty() {
        b.pad(2);
        let widths: Vec<usize> = rv.tags.iter().map(|(tag, _)| sw(tag)).collect();
        let (kept, more) = fit_tags(&widths, cols.tags);
        for (k, (tag, open)) in rv.tags.iter().take(kept).enumerate() {
            if k > 0 {
                b.push(t.dot_sep(), base.patch(t.muted()));
            }
            let mut s = t.fg(base, t.tag_color(tag));
            if *open {
                s = s.add_modifier(Modifier::BOLD);
            }
            let thl = rv.tag_hl.get(k).map(Vec::as_slice).unwrap_or(&[]);
            b.push_fit(&fit_end(tag, sw(tag)), thl, s, hl_other);
        }
        if more {
            b.push(format!(" +{}", rv.tags.len() - kept), base.patch(t.muted()));
        }
    }

    // Time, right aligned, one column from the edge (guarded against narrow overflow).
    let tw = sw(&rv.time);
    if b.used + tw < width {
        b.pad(width.saturating_sub(b.used + tw + 1));
        b.push(rv.time.clone(), base.patch(t.muted()));
    }
    b.pad_to(width);
    Line::from(b.spans)
}

fn draw(f: &mut Frame, st: &mut State) {
    let t = st.theme;
    let area = f.area();
    let cap_w = if st.fullscreen && st.inspector {
        MAX_FULL_WIDTH
    } else {
        MAX_WIDTH
    };
    let w = area.width.min(cap_w);
    let x = if st.fullscreen && area.width > w {
        area.x + (area.width - w) / 2
    } else {
        area.x
    };
    let outer = Rect {
        x,
        width: w,
        ..area
    };

    // Build top title with inline clickable tab pills.
    let live_count = st
        .best
        .values()
        .filter(|s| s.status != Status::Done)
        .count();
    let mut top_spans = vec![Span::styled(
        format!(" {} grace ", t.star()),
        t.title_accent(),
    )];
    let tab_style = |active: bool| {
        if active {
            let mut s = t
                .fg(Style::default(), t.accent())
                .add_modifier(Modifier::BOLD);
            if let Some(bg) = t.pill_bg() {
                s = s.bg(bg);
            }
            s.remove_modifier(Modifier::DIM)
        } else {
            t.title_muted()
        }
    };
    let f_label = " Folders ";
    let s_label = if live_count > 0 {
        format!(" Sessions · {live_count} live ")
    } else {
        " Sessions ".into()
    };
    let title_str = format!(" {} grace ", t.star());
    let mut tab_x = outer.x + 1 + sw(&title_str) as u16;
    let count = format!(" {}/{} ", st.matches.len(), st.data.entries.len());
    let tag_badge = st.tag_filter.as_ref().map(|tag| format!(" tag:{tag} "));
    let right_w = tag_badge.as_deref().map_or(0, sw) + sw(&count);
    let tabs_need = sw(&title_str) + 2 + sw(f_label) + 1 + sw(&s_label) + 1 + right_w + 4;
    if (w as usize) >= tabs_need {
        top_spans.push(Span::raw(" "));
        tab_x += 1;
        st.tab_folders_area = Rect {
            x: tab_x,
            y: outer.y,
            width: sw(f_label) as u16,
            height: 1,
        };
        top_spans.push(Span::styled(f_label, tab_style(st.view == View::Folders)));
        tab_x += (sw(f_label) + 1) as u16;
        top_spans.push(Span::raw(" "));
        st.tab_sessions_area = Rect {
            x: tab_x,
            y: outer.y,
            width: sw(&s_label) as u16,
            height: 1,
        };
        top_spans.push(Span::styled(s_label, tab_style(st.view == View::Sessions)));
        top_spans.push(Span::raw(" "));
    } else {
        st.tab_folders_area = Rect::default();
        st.tab_sessions_area = Rect::default();
    }

    let mut right_spans = Vec::new();
    let max_right = (w as usize).saturating_sub(sw(&title_str) + 4);
    if right_w <= max_right {
        if let (Some(tag), Some(badge)) = (&st.tag_filter, tag_badge) {
            right_spans.push(Span::styled(
                badge,
                t.fg(Style::default(), t.tag_color(tag))
                    .remove_modifier(Modifier::DIM),
            ));
        }
        right_spans.push(Span::styled(count, t.title_muted()));
    } else if sw(&count) <= max_right {
        right_spans.push(Span::styled(count, t.title_muted()));
    }

    let active_toast = st
        .toast
        .as_ref()
        .filter(|toast| Instant::now() < toast.until)
        .cloned();
    let max_bw = (outer.width as usize).saturating_sub(4);
    let bottom = match &active_toast {
        Some(toast) => toast_line(toast, max_bw, t),
        None => {
            let owned: Vec<(&str, &str)> = st
                .footer_hints
                .iter()
                .map(|(k, v)| (*k, v.as_str()))
                .collect();
            let fallback = [
                ("⏎", "travel"),
                ("^o", "resume"),
                ("^e", "editor"),
                ("^g", "hub"),
                ("⇥", "view"),
                ("?", "keys"),
            ];
            if owned.is_empty() {
                Line::from(hint_spans(&fallback, max_bw, t))
            } else {
                Line::from(hint_spans(&owned, max_bw, t))
            }
        }
    };

    let block = Block::bordered()
        .border_type(t.border_type())
        .border_style(t.border_style(true))
        .title_top(Line::from(top_spans).left_aligned())
        .title_top(Line::from(right_spans).right_aligned())
        .title_bottom(bottom);
    let inner = block.inner(outer);
    f.render_widget(block, outer);
    st.animating = false;
    st.list_area = Rect::default();
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let iw = inner.width as usize;
    let mut cur_y = inner.y;

    // Optional shell-hook banner when stdout is a TTY (user didn't install shell hook yet).
    if let Some(init_line) = &st.shell_hook_missing {
        if inner.height >= 5 {
            let warn = cut(&format!(" ! Shell hook inactive — add: {init_line}"), iw);
            f.render_widget(
                Paragraph::new(Span::styled(warn, t.fg(Style::default(), t.accent()))),
                Rect {
                    y: cur_y,
                    height: 1,
                    ..inner
                },
            );
            cur_y += 1;
        }
    }

    // Query line with tail scrolling.
    let q_avail = iw.saturating_sub(6);
    let q_shown = cut_tail(&st.query, q_avail);
    let mut q_spans = vec![
        Span::raw(" "),
        Span::styled("❯ ", t.title_accent()),
        Span::raw(q_shown),
        Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
    ];
    if st.query.is_empty() && iw > 26 {
        q_spans.push(Span::styled(" search folders, tags, branches", t.muted()));
    }
    f.render_widget(
        Paragraph::new(Line::from(q_spans)),
        Rect {
            y: cur_y,
            height: 1,
            ..inner
        },
    );
    cur_y += 1;

    // Subtle hairline separator when space allows.
    if cur_y < inner.y + inner.height && inner.height >= 6 {
        f.render_widget(
            Paragraph::new(Span::styled(
                t.h_rule().repeat(iw),
                t.fg(Style::default(), t.border_color()),
            )),
            Rect {
                y: cur_y,
                height: 1,
                ..inner
            },
        );
        cur_y += 1;
    }

    let body = Rect {
        x: inner.x,
        y: cur_y,
        width: inner.width,
        height: (inner.y + inner.height).saturating_sub(cur_y),
    };
    if body.height == 0 {
        return;
    }

    if st.data.entries.is_empty() {
        if let Some(tag) = &st.tag_filter {
            center_msg(f, body, &format!("No folders tagged \"{tag}\""), t);
        } else {
            draw_empty_welcome(f, body, t);
        }
        draw_active_modal(f, outer, st);
        return;
    }

    // Optional Inspector split on wide screens.
    let show_inspector = st.inspector && body.width >= 94;
    let (list_full, insp_area) = if show_inspector {
        let lw = (body.width * 62 / 100).max(52);
        let sep_x = body.x + lw;
        let sep = Rect {
            x: sep_x,
            y: body.y,
            width: 1,
            height: body.height,
        };
        let vr = t.v_rule();
        let sep_lines: Vec<Line> = (0..sep.height)
            .map(|_| Line::from(Span::styled(vr, t.fg(Style::default(), t.border_color()))))
            .collect();
        f.render_widget(Paragraph::new(sep_lines), sep);
        (
            Rect { width: lw, ..body },
            Some(Rect {
                x: sep_x + 1,
                width: body.width - lw - 1,
                ..body
            }),
        )
    } else {
        (body, None)
    };

    if st.rows.is_empty() {
        let msg = if st.view == View::Sessions {
            "No live agent sessions matching filter"
        } else {
            "No matching folders"
        };
        center_msg(f, list_full, msg, t);
        if let Some(ia) = insp_area {
            draw_inspector(f, ia, st);
        }
        draw_active_modal(f, outer, st);
        return;
    }

    let height = list_full.height as usize;
    st.page = height;
    st.offset = scroll(&st.rows, st.selected, st.offset, height);
    let (row_rect, sb_area) = slice_scrollbar(list_full, st.rows.len(), height);
    st.list_area = row_rect;
    let width = row_rect.width as usize;

    let status_w = if st.view == View::Sessions {
        if width >= 72 {
            15
        } else if width >= 44 {
            12
        } else {
            10
        }
    } else {
        2
    };
    let cols = layout_cols(
        width,
        status_w,
        st.widths.name_max,
        st.widths.parent_max,
        st.widths.tags_max,
        st.widths.time_max,
    );

    let frame = (st.start.elapsed().as_millis() / TICK.as_millis()) as usize;
    let ctx = Ctx {
        theme: t,
        view: st.view,
        frame,
    };
    let visible: Vec<(usize, Row)> = st
        .rows
        .iter()
        .copied()
        .enumerate()
        .skip(st.offset)
        .take(height)
        .collect();
    let mut lines = Vec::new();
    for (ri, row) in visible {
        match row {
            Row::Header(s, count) => {
                let (g, _, st_style) = t.status_look(s, frame);
                let label = format!(" {g} {} ({count})", s.label().to_uppercase());
                lines.push(Line::from(Span::styled(
                    cut(&label, width),
                    t.apply(st_style).add_modifier(Modifier::BOLD),
                )));
            }
            Row::Item(i) => {
                let rv = st.row_view(i, true);
                st.animating |= rv
                    .session
                    .as_ref()
                    .is_some_and(|s| s.status == Status::Working);
                lines.push(compose_row(&rv, &cols, ri == st.selected, &ctx, width));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), row_rect);

    if let Some(sb_area) = sb_area {
        render_scrollbar(f, sb_area, st.rows.len(), st.offset, height, t);
    }
    if let Some(ia) = insp_area {
        draw_inspector(f, ia, st);
    }
    draw_active_modal(f, outer, st);
}

fn draw_empty_welcome(f: &mut Frame, area: Rect, t: Theme) {
    if area.height < 4 {
        center_msg(
            f,
            area,
            "No folders yet. Press i to import or a to add here.",
            t,
        );
        return;
    }
    let lines = vec![
        Line::from(Span::styled(
            format!("{}  No sites of grace yet", t.star()),
            t.title_accent(),
        ))
        .alignment(Alignment::Center),
        Line::raw(""),
        Line::from(vec![
            Span::styled("[i]", t.keycap_style()),
            Span::styled(" import from editors & Claude   ", t.muted()),
            Span::styled("[a]", t.keycap_style()),
            Span::styled(" add current folder", t.muted()),
        ])
        .alignment(Alignment::Center),
    ];
    let top = area.y + area.height.saturating_sub(lines.len() as u16) / 2;
    f.render_widget(
        Paragraph::new(lines),
        Rect {
            y: top,
            height: 3.min(area.height),
            ..area
        },
    );
}

fn draw_inspector(f: &mut Frame, area: Rect, st: &State) {
    if area.width < 12 || area.height < 3 {
        return;
    }
    let t = st.theme;
    let pad = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(2),
        ..area
    };
    let w = pad.width as usize;
    let Some(e) = st.selected_entry() else {
        center_msg(f, pad, "Nothing selected", t);
        return;
    };
    let disp = display_path(&e.path, st.home.as_deref());
    let shown = platform::sanitize_display(&disp);
    let (_, name) = split_path(&shown);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{} ", t.star()), t.title_accent()),
            Span::styled(
                cut(name, w.saturating_sub(2)),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(cut(&shown, w), t.muted())),
    ];

    if let Some(s) = st.best.get(&e.path) {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "SESSION",
            t.muted().add_modifier(Modifier::BOLD),
        )));
        let (g, lbl, style) = t.status_look(
            s.status,
            (st.start.elapsed().as_millis() / TICK.as_millis()) as usize,
        );
        let detail = s
            .detail
            .as_deref()
            .map(|d| format!(" · {}", platform::sanitize_display(d)))
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(
                format!("{g} {} ", platform::sanitize_display(&s.tool)),
                t.apply(style),
            ),
            Span::styled(
                format!("({lbl}{detail}) · {}", time_ago(s.updated_at, st.now)),
                t.muted(),
            ),
        ]));
    }

    if let Some(Some(sum)) = st.summaries.get(&e.path) {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "GIT",
            t.muted().add_modifier(Modifier::BOLD),
        )));
        let mut br = format!(
            "{} {}",
            t.branch_glyph(),
            platform::sanitize_display(&sum.branch.name)
        );
        if sum.worktree {
            br.push_str(" (worktree)");
        }
        if sum.branch.ahead > 0 {
            br.push_str(&format!(" ↑{}", sum.branch.ahead));
        }
        if sum.branch.behind > 0 {
            br.push_str(&format!(" ↓{}", sum.branch.behind));
        }
        if sum.changed > 0 {
            br.push_str(&format!(" · {} changed", sum.changed));
        }
        if sum.stashes > 0 {
            br.push_str(&format!(" · {} stash", sum.stashes));
        }
        lines.push(Line::from(Span::styled(
            cut(&br, w),
            t.fg(Style::default(), t.accent()),
        )));
        for c in sum.commits.iter().take(3) {
            lines.push(Line::from(Span::styled(
                cut(&format!("  {c}"), w),
                t.muted(),
            )));
        }
    } else if let Some((br, wt)) = st.data.branches.get(&e.path) {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "GIT",
            t.muted().add_modifier(Modifier::BOLD),
        )));
        let br_safe = platform::sanitize_display(br);
        let label = if *wt {
            format!("{} {br_safe} (worktree)", t.branch_glyph())
        } else {
            format!("{} {br_safe}", t.branch_glyph())
        };
        lines.push(Line::from(Span::styled(
            cut(&label, w),
            t.fg(Style::default(), t.accent()),
        )));
    }

    if !e.tags.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "TOOLS",
            t.muted().add_modifier(Modifier::BOLD),
        )));
        for tag in e.tags.iter().take(5) {
            let clean = platform::sanitize_display(&tag.tool);
            let open_mark = if st
                .open
                .get(&e.path)
                .is_some_and(|l| l.iter().any(|o| o.tag == tag.tool))
            {
                format!("  {} open", t.editor_glyph())
            } else {
                String::new()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{clean:<12}"),
                    t.fg(Style::default(), t.tag_color(&clean)),
                ),
                Span::styled(
                    cut(
                        &format!(
                            "{} runs · {}{open_mark}",
                            tag.count,
                            time_ago(tag.last_used, st.now)
                        ),
                        w.saturating_sub(12),
                    ),
                    t.muted(),
                ),
            ]));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "STATS",
        t.muted().add_modifier(Modifier::BOLD),
    )));
    let pin_s = if e.pinned {
        format!("{} Pinned · ", t.pin_glyph())
    } else {
        String::new()
    };
    lines.push(Line::from(Span::styled(
        cut(
            &format!(
                "{pin_s}{} visits · {}",
                e.visits,
                time_ago(e.last_visit, st.now)
            ),
            w,
        ),
        t.muted(),
    )));

    f.render_widget(Paragraph::new(lines), pad);
}

fn draw_active_modal(f: &mut Frame, outer: Rect, st: &State) {
    let t = st.theme;
    match st.modal {
        Modal::None => {}
        Modal::Help => render_help_modal(f, outer, "keys", &HELP_ROWS, t),
        Modal::ConfirmForget => {
            let name = st
                .selected_entry()
                .map(|e| {
                    let clean = platform::sanitize_display(&e.path);
                    split_path(&clean).1.to_string()
                })
                .unwrap_or_default();
            let body = vec![
                Line::from(format!("Remove \"{name}\" from grace history?")),
                Line::from(Span::styled("Files on disk are never deleted.", t.muted())),
            ];
            render_modal(
                f,
                outer,
                "forget folder",
                body,
                &[("y", "forget"), ("n / esc", "cancel")],
                t,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal

struct TtyBackend(CrosstermBackend<io::Stderr>);

impl Backend for TtyBackend {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.0.draw(content)
    }
    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.0.append_lines(n)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.0.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.0.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        #[cfg(unix)]
        {
            Backend::flush(&mut self.0)?;
            tty_cursor_position()
        }
        #[cfg(not(unix))]
        {
            self.0.get_cursor_position()
        }
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, p: P) -> io::Result<()> {
        self.0.set_cursor_position(p)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.0.clear()
    }
    fn clear_region(&mut self, t: ClearType) -> io::Result<()> {
        self.0.clear_region(t)
    }
    fn size(&self) -> io::Result<Size> {
        self.0.size()
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.0.window_size()
    }
    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.0)
    }
}

/// Parse the last "ESC [ row ; col R" reply in `buf`. Returns a 0-based position.
#[cfg(any(unix, test))]
fn parse_cpr(buf: &[u8]) -> Option<Position> {
    let end = buf.iter().rposition(|&b| b == b'R')?;
    let start = buf[..end].windows(2).rposition(|w| w == b"\x1b[")?;
    let body = std::str::from_utf8(&buf[start + 2..end]).ok()?;
    let (row, col) = body.split_once(';')?;
    let row: u16 = row.parse().ok()?;
    let col: u16 = col.parse().ok()?;
    Some(Position {
        x: col.saturating_sub(1),
        y: row.saturating_sub(1),
    })
}

#[cfg(unix)]
fn tty_cursor_position() -> io::Result<Position> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    tty.write_all(b"\x1b[6n")?;
    tty.flush()?;
    let fd = tty.as_raw_fd();
    if fd < 0 || fd >= libc::FD_SETSIZE as libc::c_int {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "tty fd exceeds FD_SETSIZE",
        ));
    }
    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut buf = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no cursor position reply",
            ));
        }
        // SAFETY: `fd` was verified `< FD_SETSIZE` above and `set` is zero-initialized.
        let ready = unsafe {
            let mut set: libc::fd_set = std::mem::zeroed();
            libc::FD_ZERO(&mut set);
            libc::FD_SET(fd, &mut set);
            let mut tv = libc::timeval {
                tv_sec: left.as_secs() as _,
                tv_usec: left.subsec_micros() as _,
            };
            libc::select(
                fd + 1,
                &mut set,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if ready < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if ready == 0 {
            continue;
        }
        let mut b = [0u8; 64];
        let n = tty.read(&mut b)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "tty closed"));
        }
        if buf.len() + n > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "tty CPR overflow",
            ));
        }
        buf.extend_from_slice(&b[..n]);
        if let Some(p) = parse_cpr(&buf) {
            return Ok(p);
        }
    }
}

static FULLSCREEN: AtomicBool = AtomicBool::new(false);

struct PickerGuard;

impl Drop for PickerGuard {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stderr(), DisableMouseCapture);
    if FULLSCREEN.swap(false, Ordering::SeqCst) {
        let _ = execute!(io::stderr(), LeaveAlternateScreen, Show);
    } else {
        let _ = execute!(io::stderr(), Show);
    }
}

fn update_footer_hints(st: &mut State, opts: &Opts) {
    let mut hints = vec![("⏎", "travel".to_string())];
    if let Some(e) = st.selected_entry() {
        if let Some(tool) = (opts.tool_label)(e) {
            hints.push(("^o", tool));
        } else {
            hints.push(("^o", "resume".into()));
        }
        if let Some(ed) = (opts.editor_label)(e, st.selected_open_editor(e)) {
            hints.push(("^e", ed));
        }
    }
    hints.push(("^g", "hub".into()));
    hints.push((
        "⇥",
        if st.view == View::Folders {
            "sessions".into()
        } else {
            "folders".into()
        },
    ));
    hints.push(("?", "keys".into()));
    st.footer_hints = hints;
}

fn reload_interval(opts: &Opts, view: View) -> Option<Duration> {
    opts.reload.as_ref()?;
    opts.reload_every.as_ref().and_then(|f| f(view))
}

fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    st: &mut State,
    opts: &mut Opts,
) -> Result<Option<Choice>> {
    let mut first = true;
    let mut bg_rx: Option<(View, Receiver<Option<Data>>)> = None;
    let mut next_reload: Option<Instant> = None;
    let mut dirty = true;
    let mut hints_dirty = true;
    let mut last_frame = 0usize;

    loop {
        if let Some((bg_view, rx)) = &bg_rx {
            if let Ok(maybe) = rx.try_recv() {
                if *bg_view == st.view {
                    if let Some(d) = maybe {
                        st.now = crate::model::now();
                        st.set_data(d);
                        dirty = true;
                        hints_dirty = true;
                    }
                }
                bg_rx = None;
            }
        }
        if st.maybe_fetch_summary() {
            dirty = true;
        }
        if dirty {
            if hints_dirty {
                update_footer_hints(st, opts);
                hints_dirty = false;
            }
            terminal.draw(|f| draw(f, st))?;
            dirty = false;
        }

        if first {
            first = false;
            if let Some(job) = opts.after_first_frame.take() {
                let launch_view = st.view;
                let (tx, rx) = mpsc::channel();
                bg_rx = Some((launch_view, rx));
                std::thread::spawn(move || {
                    let _ = tx.send(job());
                });
            }
            next_reload = reload_interval(opts, st.view).map(|d| Instant::now() + d);
        }

        let now = Instant::now();
        if st.toast.as_ref().is_some_and(|t| now >= t.until) {
            st.toast = None;
            dirty = true;
        }
        if next_reload.is_some_and(|at| now >= at) {
            if let Some(d) = opts.reload.as_mut().and_then(|r| r(st.view)) {
                st.now = crate::model::now();
                st.set_data(d);
                dirty = true;
                hints_dirty = true;
            }
            next_reload = reload_interval(opts, st.view).map(|d| Instant::now() + d);
        }

        let mut wait = Duration::from_secs(60);
        if st.animating {
            let into = (now - st.start).as_millis() % TICK.as_millis();
            wait = wait.min(TICK - Duration::from_millis(into as u64));
        }
        if bg_rx.is_some() || st.pending_summary.is_some() || st.summary_wait.is_some() {
            wait = wait.min(Duration::from_millis(40));
        }
        if let Some(toast) = &st.toast {
            wait = wait.min(toast.until.saturating_duration_since(now));
        }
        if let Some(at) = next_reload {
            wait = wait.min(at.saturating_duration_since(now));
        }

        if event::poll(wait)? {
            match event::read()? {
                Event::Mouse(m) => {
                    if let Some(c) = handle_mouse(st, opts, m, &mut next_reload) {
                        return Ok(Some(c));
                    }
                    if !matches!(m.kind, MouseEventKind::Moved) {
                        dirty = true;
                        hints_dirty = true;
                    }
                }
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    if let Some(res) = handle_key(st, opts, key, &mut next_reload) {
                        return Ok(res);
                    }
                    dirty = true;
                    hints_dirty = true;
                }
                Event::Resize(_, _) => {
                    dirty = true;
                }
                _ => {}
            }
        }
        if st.animating {
            let frame =
                (Instant::now().saturating_duration_since(st.start)).as_millis() / TICK.as_millis();
            if frame as usize != last_frame {
                last_frame = frame as usize;
                dirty = true;
            }
        }
    }
}

fn handle_mouse(
    st: &mut State,
    opts: &mut Opts,
    m: MouseEvent,
    next_reload: &mut Option<Instant>,
) -> Option<Choice> {
    if st.modal != Modal::None {
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            st.modal = Modal::None;
        }
        return None;
    }
    match m.kind {
        MouseEventKind::ScrollUp => st.mv(-1),
        MouseEventKind::ScrollDown => st.mv(1),
        MouseEventKind::Down(MouseButton::Left) => {
            let pos = Position {
                x: m.column,
                y: m.row,
            };
            if st.tab_folders_area.contains(pos) && st.view != View::Folders {
                switch_view(st, opts, View::Folders, next_reload);
                return None;
            }
            if st.tab_sessions_area.contains(pos) && st.view != View::Sessions {
                switch_view(st, opts, View::Sessions, next_reload);
                return None;
            }
            if st.list_area.contains(pos) {
                let rel_y = (m.row - st.list_area.y) as usize;
                let idx = st.offset + rel_y;
                if matches!(st.rows.get(idx), Some(Row::Item(_))) {
                    let is_dbl = st.last_click.click(idx);
                    st.selected = idx;
                    if is_dbl {
                        return st.selected_entry().map(|e| Choice {
                            entry: e.clone(),
                            action: Action::Travel,
                        });
                    }
                }
            }
        }
        _ => {}
    }
    None
}

fn switch_view(st: &mut State, opts: &mut Opts, next: View, next_reload: &mut Option<Instant>) {
    st.view = next;
    *next_reload = reload_interval(opts, next).map(|d| Instant::now() + d);
    if let Some(d) = opts.reload.as_mut().and_then(|r| r(next)) {
        st.now = crate::model::now();
        st.set_data(d);
    } else {
        st.refilter();
    }
}

/// Returns `Some(res)` to exit the picker with `res`, `None` to keep running.
fn handle_key(
    st: &mut State,
    opts: &mut Opts,
    key: KeyEvent,
    next_reload: &mut Option<Instant>,
) -> Option<Option<Choice>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    // Modal intercept.
    match st.modal {
        Modal::Help => {
            if is_help_dismiss(&key) {
                st.modal = Modal::None;
            }
            return None;
        }
        Modal::ConfirmForget => {
            match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                    st.modal = Modal::None;
                    if let Some(e) = st.selected_entry().cloned() {
                        if let Some(cb) = opts.forget_dir.as_mut() {
                            match cb(&e) {
                                Ok(()) => {
                                    st.data.entries.retain(|x| x.path != e.path);
                                    st.summaries.remove(&e.path);
                                    st.recompute_widths();
                                    st.refilter_keep(None);
                                    st.flash(
                                        format!("forgot {}", split_path(&e.path).1),
                                        ToastKind::Success,
                                    );
                                }
                                Err(err) => st.flash(err, ToastKind::Error),
                            }
                        }
                    }
                }
                KeyCode::Esc | KeyCode::Char('n' | 'N') => st.modal = Modal::None,
                KeyCode::Char('c') if ctrl => st.modal = Modal::None,
                _ => {}
            }
            return None;
        }
        Modal::None => {}
    }

    // Empty-DB quick welcome keys.
    if st.tag_filter.is_none() && st.data.entries.is_empty() && st.query.is_empty() && !ctrl && !alt
    {
        match key.code {
            KeyCode::Char('i' | 'I') => {
                if let Some(cb) = opts.import_all_now.as_mut() {
                    match cb() {
                        Ok(msg) => {
                            if let Some(d) = opts.reload.as_mut().and_then(|r| r(st.view)) {
                                st.set_data(d);
                            }
                            st.flash(msg, ToastKind::Success);
                        }
                        Err(e) => st.flash(e, ToastKind::Error),
                    }
                    return None;
                }
            }
            KeyCode::Char('a' | 'A') => {
                if let Some(cb) = opts.add_cwd_now.as_mut() {
                    match cb() {
                        Ok(msg) => {
                            if let Some(d) = opts.reload.as_mut().and_then(|r| r(st.view)) {
                                st.set_data(d);
                            }
                            st.flash(msg, ToastKind::Success);
                        }
                        Err(e) => st.flash(e, ToastKind::Error),
                    }
                    return None;
                }
            }
            _ => {}
        }
    }

    let choose = |st: &State, action: Action| {
        st.selected_entry().map(|e| Choice {
            entry: e.clone(),
            action,
        })
    };

    match key.code {
        KeyCode::Esc => return Some(None),
        KeyCode::Char('c') if ctrl => return Some(None),
        KeyCode::Enter => {
            if let Some(c) = choose(st, Action::Travel) {
                return Some(Some(c));
            }
        }
        KeyCode::Char('o') if ctrl => {
            if let Some(c) = choose(st, Action::Resume) {
                return Some(Some(c));
            }
        }
        KeyCode::Char('g') if ctrl => {
            if let Some(c) = choose(st, Action::Hub) {
                return Some(Some(c));
            }
        }
        KeyCode::Char('e') if ctrl => {
            if let Some(e) = st.selected_entry().cloned() {
                match (opts.editor_cmd)(&e, st.selected_open_editor(&e)) {
                    Some(cmd) => {
                        return Some(Some(Choice {
                            entry: e,
                            action: Action::Editor(cmd),
                        }))
                    }
                    None => st.flash("no editor used here", ToastKind::Warn),
                }
            }
        }
        KeyCode::Char('y') if ctrl => {
            if let Some(e) = st.selected_entry() {
                if platform::copy_to_clipboard(&e.path) {
                    st.flash(
                        format!("copied {}", split_path(&e.path).1),
                        ToastKind::Success,
                    );
                } else {
                    st.flash("clipboard copy unavailable", ToastKind::Warn);
                }
            }
        }
        KeyCode::Char('d') if ctrl => {
            if st.selected_entry().is_some() && opts.forget_dir.is_some() {
                st.modal = Modal::ConfirmForget;
            }
        }
        KeyCode::Char('p') if alt => {
            if let Some(e) = st.selected_entry().cloned() {
                if let Some(cb) = opts.toggle_pin.as_mut() {
                    match cb(&e) {
                        Ok(now_pinned) => {
                            if let Some(item) =
                                st.data.entries.iter_mut().find(|x| x.path == e.path)
                            {
                                item.pinned = now_pinned;
                            }
                            crate::rank::sort_entries(&mut st.data.entries, st.now);
                            st.refilter_keep(Some(&e.path));
                            let verb = if now_pinned { "pinned" } else { "unpinned" };
                            st.flash(
                                format!("{verb} {}", split_path(&e.path).1),
                                ToastKind::Success,
                            );
                        }
                        Err(err) => st.flash(err, ToastKind::Error),
                    }
                }
            }
        }
        KeyCode::Char('i') if alt => {
            st.inspector = !st.inspector;
            if st.inspector {
                let term_w = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(100);
                if term_w < 96 {
                    st.flash(
                        "Widen terminal (>= 96 cols) to view inspector",
                        ToastKind::Info,
                    );
                }
            }
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let next = if st.view == View::Folders {
                View::Sessions
            } else {
                View::Folders
            };
            switch_view(st, opts, next, next_reload);
        }
        KeyCode::F(1) => st.modal = Modal::Help,
        KeyCode::Char('?') if st.query.is_empty() && !ctrl && !alt => st.modal = Modal::Help,
        KeyCode::Up => st.mv(-1),
        KeyCode::Down => st.mv(1),
        KeyCode::PageUp => st.mv(-(st.page.max(1) as isize)),
        KeyCode::PageDown => st.mv(st.page.max(1) as isize),
        KeyCode::Home => st.selected = first_item(&st.rows).unwrap_or(0),
        KeyCode::End => st.mv(st.rows.len() as isize),
        KeyCode::Char('p') | KeyCode::Char('k') if ctrl && !alt => st.mv(-1),
        KeyCode::Char('n') | KeyCode::Char('j') if ctrl && !alt => st.mv(1),
        KeyCode::Char('w') if ctrl => st.delete_word(),
        KeyCode::Char('u') if ctrl => {
            st.query.clear();
            st.refilter();
        }
        KeyCode::Backspace => {
            st.query.pop();
            st.refilter();
        }
        KeyCode::Char(c) if is_plain_char(&key) => {
            st.query.push(c);
            st.refilter();
        }
        _ => {}
    }
    None
}

fn run_fullscreen(st: &mut State, opts: &mut Opts) -> Result<Option<Choice>> {
    FULLSCREEN.store(true, Ordering::SeqCst);
    execute!(io::stderr(), EnterAlternateScreen, EnableMouseCapture, Hide)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stderr()))?;
    run(&mut terminal, st, opts)
}

fn run_inline(st: &mut State, opts: &mut Opts) -> Result<Option<Choice>> {
    let (_, term_h) = crossterm::terminal::size()?;
    let rows = st.rows.len();
    let h = inline_height(rows, term_h);
    let mut terminal = match Terminal::with_options(
        TtyBackend(CrosstermBackend::new(io::stderr())),
        TerminalOptions {
            viewport: Viewport::Inline(h),
        },
    ) {
        Ok(t) => t,
        Err(_) => return run_fullscreen(st, opts),
    };
    let _ = execute!(io::stderr(), EnableMouseCapture);
    terminal.hide_cursor()?;
    let result = run(&mut terminal, st, opts);
    let _ = terminal.clear();
    let _ = terminal.show_cursor();
    result
}

pub fn pick(
    data: Data,
    query: &str,
    now: i64,
    cfg: &Config,
    mut opts: Opts,
) -> Result<Option<Choice>> {
    let home = platform::home_dir().map(|h| h.to_string_lossy().into_owned());
    let theme = Theme::from_env_and_config(cfg);
    let mut st = State::new(
        data,
        query,
        now,
        opts.view,
        opts.fullscreen,
        opts.tag_filter.clone(),
        opts.shell_hook_missing.clone(),
        theme,
        home,
    );
    if let Some((msg, kind)) = opts.initial_toast.take() {
        st.toast = Some(Toast::new(msg, kind, 4000));
    }
    // Fast travel: one match for a given query, so skip the UI.
    if !query.trim().is_empty() && st.matches.len() == 1 {
        let entry = st.data.entries[st.matches[0]].clone();
        return Ok(Some(Choice {
            entry,
            action: Action::Travel,
        }));
    }
    if !io::stderr().is_terminal() {
        bail!("grace requires an interactive terminal for the picker");
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        default_hook(info);
    }));

    enable_raw_mode()?;
    let _guard = PickerGuard;
    if opts.fullscreen {
        run_fullscreen(&mut st, &mut opts)
    } else {
        run_inline(&mut st, &mut opts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Tag;

    fn render_pick(st: &mut State, w: u16, h: u16) -> String {
        crate::tui::render_text(w, h, |f| draw(f, st))
    }

    fn entry(path: &str, tools: &[&str]) -> Entry {
        Entry {
            path: path.into(),
            visits: 1,
            last_visit: 0,
            pinned: false,
            tags: tools
                .iter()
                .map(|t| Tag {
                    tool: t.to_string(),
                    count: 1,
                    last_used: 0,
                })
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

    fn session(id: i64, path: &str, tool: &str, status: Status, updated_at: i64) -> Session {
        Session {
            id,
            path: path.into(),
            tool: tool.into(),
            status,
            shell_pid: None,
            started_at: 0,
            updated_at,
            session_id: None,
            detail: None,
            tmux_pane: None,
        }
    }

    #[test]
    fn empty_query_keeps_order() {
        assert_eq!(filter(&sample(), &Best::new(), ""), vec![0, 1, 2]);
    }

    #[test]
    fn query_narrows() {
        assert_eq!(filter(&sample(), &Best::new(), "beta"), vec![1]);
    }

    #[test]
    fn tag_text_matches() {
        assert_eq!(filter(&sample(), &Best::new(), "claude"), vec![0]);
    }

    #[test]
    fn status_word_matches() {
        let s = session(1, "/home/u/beta", "codex", Status::Waiting, 5);
        let done = Session {
            id: 2,
            status: Status::Done,
            updated_at: 9,
            ..s.clone()
        };
        let best = best_by_path(&[done, s]);
        assert_eq!(best["/home/u/beta"].status, Status::Waiting);
        assert_eq!(filter(&sample(), &best, "needs"), vec![1]);
    }

    #[test]
    fn ready_badge_and_search() {
        let s = session(1, "/home/u/beta", "codex", Status::Ready, 5);
        let work = Session {
            id: 2,
            status: Status::Working,
            updated_at: 9,
            ..s.clone()
        };
        let best = best_by_path(&[work, s]);
        assert_eq!(best["/home/u/beta"].status, Status::Ready);
        assert!(urgency(Status::Waiting) > urgency(Status::Ready));
        assert!(urgency(Status::Ready) > urgency(Status::Working));
        assert_eq!(filter(&sample(), &best, "turn"), vec![1]);
    }

    #[test]
    fn no_match_is_empty() {
        assert!(filter(&sample(), &Best::new(), "zzzqqq").is_empty());
    }

    #[test]
    fn split_paths() {
        assert_eq!(split_path("~/code/knn"), ("~/code", "knn"));
        assert_eq!(split_path("/tmp"), ("/", "tmp"));
        assert_eq!(split_path("~"), ("", "~"));
    }

    #[test]
    fn columns() {
        let c = layout_cols(108, 2, 40, 20, 14, 3);
        assert_eq!((c.name, c.parent, c.tags), (28, 20, 14));
        let c = layout_cols(60, 2, 10, 30, 30, 3);
        assert_eq!((c.name, c.parent, c.tags), (10, 12, 60 - 14 - 10 - 12));
        let c = layout_cols(25, 2, 10, 30, 0, 3);
        assert_eq!(c.parent, 0);
    }

    #[test]
    fn tags_fit() {
        assert_eq!(fit_tags(&[6, 5], 14), (2, false));
        assert_eq!(fit_tags(&[6, 5, 6], 14), (1, true));
        assert_eq!(fit_tags(&[6], 3), (0, false));
        assert_eq!(fit_tags(&[], 10), (0, false));
    }

    #[test]
    fn rows_group_and_skip_headers() {
        let st = |i: usize| {
            [
                Some(Status::Done),
                Some(Status::Waiting),
                Some(Status::Done),
            ][i]
        };
        let rows = build_rows(View::Sessions, &[0, 1, 2], st);
        assert_eq!(
            rows,
            vec![
                Row::Header(Status::Waiting, 1),
                Row::Item(1),
                Row::Header(Status::Done, 2),
                Row::Item(0),
                Row::Item(2)
            ]
        );
        assert_eq!(first_item(&rows), Some(1));
        assert_eq!(step_item(&rows, 1, 1), 3);
        assert_eq!(step_item(&rows, 3, -1), 1);
        assert_eq!(step_item(&rows, 1, -1), 1);
        assert_eq!(step_item(&rows, 4, 1), 4);
        assert_eq!(scroll(&rows, 1, 0, 3), 0);
        assert_eq!(scroll(&rows, 3, 0, 2), 2);
        assert_eq!(scroll(&rows, 3, 3, 2), 2);
        assert_eq!(
            build_rows(View::Folders, &[2, 0], st),
            vec![Row::Item(2), Row::Item(0)]
        );
    }

    #[test]
    fn heights() {
        assert_eq!(inline_height(3, 50), 7);
        assert_eq!(inline_height(100, 50), 24);
        assert_eq!(inline_height(100, 12), 11);
        assert_eq!(inline_height(100, 6), 6);
        assert_eq!(inline_height(0, 50), 7);
    }

    #[test]
    fn cursor_reply() {
        assert_eq!(parse_cpr(b"x\x1b[12;5R"), Some(Position { x: 4, y: 11 }));
        assert_eq!(parse_cpr(b"\x1b[12;"), None);
    }

    const NOW: i64 = 1_000_000;

    fn demo() -> Data {
        let mut e = vec![
            entry("/home/u/code/aiml/knn", &["claude", "vscode"]),
            entry("/home/u/code/aiml/svm", &["claude"]),
            entry("/home/u/work/api-server", &["codex", "cursor"]),
            entry(
                "/home/u/code/aiml/kmeans-clustering-experiments",
                &["antigravity"],
            ),
            entry("/home/u/notes", &[]),
            entry(
                "/srv/data/archive/very/deep/nested/folder/structure/old-site",
                &["zed", "neovim", "gemini"],
            ),
        ];
        let ago = [120, 300, 30, 720, 3 * 86400, 40 * 86400];
        for (x, a) in e.iter_mut().zip(ago) {
            x.last_visit = NOW - a;
        }
        e[0].pinned = true;
        let sessions = vec![
            session(
                1,
                "/home/u/code/aiml/knn",
                "claude",
                Status::Waiting,
                NOW - 120,
            ),
            session(
                2,
                "/home/u/code/aiml/svm",
                "claude",
                Status::Ready,
                NOW - 300,
            ),
            session(
                3,
                "/home/u/work/api-server",
                "codex",
                Status::Working,
                NOW - 30,
            ),
            session(
                4,
                "/home/u/code/aiml/kmeans-clustering-experiments",
                "antigravity",
                Status::Done,
                NOW - 720,
            ),
        ];
        let editors = vec![OpenEditor {
            path: "/home/u/notes".into(),
            tag: "vscode".into(),
            prog: "code".into(),
            args: vec!["/home/u/notes".into()],
        }];
        Data {
            entries: e,
            sessions,
            editors,
            branches: HashMap::new(),
        }
    }

    #[test]
    fn render_folders() {
        let mut st = State::new(
            demo(),
            "",
            NOW,
            View::Folders,
            false,
            None,
            None,
            Theme::dark(),
            Some("/home/u".into()),
        );
        let out = render_pick(&mut st, 100, 11);
        println!("{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].starts_with("╭ ✦ grace ")
                && lines[0].contains("Folders")
                && lines[0].ends_with(" 6/6 ╮"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].contains("❯   search folders, tags, branches"),
            "{}",
            lines[1]
        );
        assert!(lines[2].contains("────"), "{}", lines[2]);
        assert!(lines[3].starts_with("│▌● ★ knn"), "{}", lines[3]);
        assert!(
            lines[3].contains("~/code/aiml") && lines[3].contains("claude · vscode"),
            "{}",
            lines[3]
        );
        assert!(lines[3].ends_with("2m │"), "{}", lines[3]);
        assert!(
            lines[5].contains("⠋") && lines[5].contains("codex · cursor"),
            "{}",
            lines[5]
        );
        assert!(
            lines[6].contains("kmeans-clustering-experimen…"),
            "{}",
            lines[6]
        );
        assert!(
            lines[7].contains("▣") && lines[7].contains("notes") && lines[7].contains("vscode")
        );
        assert!(
            lines[8].contains("old-site") && lines[8].contains("…"),
            "{}",
            lines[8]
        );
        assert_eq!(lines[0].chars().count(), 100);

        st.query = "aiml".into();
        st.refilter();
        let out = render_pick(&mut st, 70, 8);
        assert!(out.lines().next().unwrap().ends_with(" 3/6 ╮"));

        st.query = "zzzqqq".into();
        st.refilter();
        let out = render_pick(&mut st, 70, 6);
        assert!(out.contains("No matching folders"));

        let mut empty = State::new(
            Data::default(),
            "",
            0,
            View::Folders,
            false,
            None,
            None,
            Theme::dark(),
            None,
        );
        let out = render_pick(&mut empty, 72, 8);
        assert!(out.contains("No sites of grace yet"));

        let mut tagged_empty = State::new(
            Data::default(),
            "",
            0,
            View::Folders,
            false,
            Some("claude".into()),
            None,
            Theme::dark(),
            None,
        );
        let out = render_pick(&mut tagged_empty, 72, 8);
        assert!(out.contains("No folders tagged \"claude\""));
    }

    #[test]
    fn render_sessions() {
        let mut d = demo();
        d.entries.truncate(4);
        let mut st = State::new(
            d,
            "",
            NOW,
            View::Sessions,
            false,
            None,
            None,
            Theme::dark(),
            Some("/home/u".into()),
        );
        let out = render_pick(&mut st, 100, 12);
        println!("{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[3].contains("NEEDS YOU (1)"), "{}", lines[3]);
        assert!(
            lines[4].starts_with("│▌● claude") && lines[4].contains("★ knn"),
            "{}",
            lines[4]
        );
        assert!(lines[5].contains("YOUR TURN (1)"));
        assert!(lines[6].contains("○ claude") && lines[6].contains("svm"));
        assert!(lines[8].contains("codex") && lines[8].contains("api-server"));
        assert!(lines[10].contains("✓ antigravity"));
        st.mv(1);
        assert_eq!(st.selected_entry().unwrap().path, "/home/u/code/aiml/svm");
    }

    #[test]
    fn flash_shows_in_the_border() {
        let mut st = State::new(
            demo(),
            "",
            NOW,
            View::Folders,
            false,
            None,
            None,
            Theme::dark(),
            Some("/home/u".into()),
        );
        st.flash("no editor used here", ToastKind::Warn);
        let out = render_pick(&mut st, 100, 10);
        assert!(out.lines().last().unwrap().contains("no editor used here"));
    }

    #[test]
    fn narrow_title_and_multiword_highlights() {
        let mut st = State::new(
            demo(),
            "knn claude",
            NOW,
            View::Folders,
            false,
            Some("claude".into()),
            None,
            Theme::dark(),
            Some("/home/u".into()),
        );
        let rv = st.row_view(0, true);
        assert!(
            !rv.name_hl.is_empty(),
            "expected multi-word highlight in name"
        );
        assert!(
            !rv.tag_hl.is_empty() && !rv.tag_hl[0].is_empty(),
            "expected multi-word highlight in tag"
        );
        // Very narrow terminal with tag filter hides tab pills cleanly instead of colliding.
        let out = render_pick(&mut st, 44, 8);
        let first_line = out.lines().next().unwrap();
        assert_eq!(first_line.chars().count(), 44, "{first_line}");
        assert!(first_line.contains("✦ grace"), "{first_line}");
        assert!(!first_line.contains("Folders"), "{first_line}");
        assert_eq!(st.tab_folders_area, Rect::default());
        assert_eq!(st.tab_sessions_area, Rect::default());

        // Unicode non-ASCII whitespace in delete_word does not panic on truncate.
        st.query = "foo\u{3000}bar/".into();
        st.delete_word();
        assert_eq!(st.query, "foo\u{3000}");
    }
}
