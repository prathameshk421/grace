//! Interactive TUI picker.
//!
//! The TUI draws on stderr, because the shell function captures stdout.
//! By default it draws inline, below the prompt. `--full` uses the alternate screen.
use crate::editors_live::OpenEditor;
use crate::model::{Entry, Session, Status};
use anyhow::Result;
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};
use ratatui::{
    backend::{Backend, ClearType, CrosstermBackend, WindowSize},
    buffer::Cell,
    layout::{Alignment, Position, Rect, Size},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
    Frame, Terminal, TerminalOptions, Viewport,
};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
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
fn status_words(s: Status) -> &'static str {
    match s {
        Status::Waiting => "waiting needs",
        Status::Ready => "ready your turn",
        Status::Working => "working",
        Status::Done => "done",
    }
}

fn haystack(e: &Entry, best: &Best) -> String {
    let mut s = e.path.clone();
    for t in &e.tags {
        s.push(' ');
        s.push_str(&t.tool);
    }
    if let Some(b) = best.get(&e.path) {
        s.push(' ');
        s.push_str(status_words(b.status));
    }
    s
}

/// Return indices into `entries` that match `query`, best first.
fn filter(entries: &[Entry], best: &Best, query: &str) -> Vec<usize> {
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
            let h = haystack(e, best);
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

/// Each tool's tag uses the main color of its brand.
fn tag_color(tool: &str) -> Color {
    match tool {
        // AI tools
        "claude" => Color::Rgb(217, 119, 87),      // Anthropic orange
        "antigravity" => Color::Rgb(66, 133, 244), // Google blue
        "gemini" => Color::Rgb(142, 117, 255),     // Gemini blue-violet
        "codex" => Color::Rgb(16, 163, 127),       // OpenAI green
        "cursor" => Color::Rgb(230, 230, 230),     // Cursor white
        "opencode" => Color::Rgb(250, 178, 131),   // OpenCode peach
        "aider" => Color::Rgb(20, 180, 80),        // Aider green
        "copilot" => Color::Rgb(163, 113, 247),    // GitHub Copilot purple
        "amp" => Color::Rgb(243, 78, 63),          // Amp red
        "goose" => Color::Rgb(255, 214, 0),        // Goose yellow
        "crush" => Color::Rgb(255, 95, 210),       // Charm pink
        "qwen" => Color::Rgb(97, 92, 237),         // Qwen indigo
        "t3code" => Color::Rgb(236, 72, 153),      // T3 pink
        // Editors
        "vscode" => Color::Rgb(31, 156, 240),      // VS Code blue
        "vscodium" => Color::Rgb(70, 190, 230),    // VSCodium light blue
        "windsurf" => Color::Rgb(88, 229, 187),    // Windsurf teal
        "zed" => Color::Rgb(84, 140, 255),         // Zed blue
        "neovim" => Color::Rgb(87, 161, 67),       // Neovim green
        "vim" => Color::Rgb(1, 152, 51),           // Vim green
        "sublime" => Color::Rgb(255, 152, 0),      // Sublime orange
        "jetbrains" => Color::Rgb(255, 49, 140),   // JetBrains magenta
        _ => Color::Gray,
    }
}

pub fn display_path(path: &str, home: Option<&str>) -> String {
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

// ---------------------------------------------------------------------------
// Public API

/// Which list the picker shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// All folders, with status glyphs.
    Folders,
    /// Live agent sessions, grouped by status.
    Sessions,
}

/// What the user asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Enter: go to the folder.
    Travel,
    /// ctrl-o: go to the folder and start the last tool used there.
    Resume,
    /// ctrl-e: open or focus the folder in its editor with this command. No cd.
    Editor(String),
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
}

/// The command for ctrl-e. Gets the entry and its open editor window, if any.
pub type EditorCmd<'a> = Box<dyn Fn(&Entry, Option<&OpenEditor>) -> Option<String> + 'a>;

pub struct Opts<'a> {
    pub view: View,
    pub fullscreen: bool,
    pub editor_cmd: EditorCmd<'a>,
    /// Runs once, after the first frame is on screen. Slow work goes here.
    pub after_first_frame: Option<Box<dyn FnOnce() + 'a>>,
    /// Reads the data again. The picker keeps the query and the selection.
    pub reload: Option<Box<dyn FnMut() -> Option<Data> + 'a>>,
    /// How often to reload. None: reload once, after `after_first_frame`.
    pub reload_every: Option<Duration>,
}

// ---------------------------------------------------------------------------
// Theme

/// Grace gold.
const GOLD: Color = Color::Rgb(232, 185, 74);
const SEL_BG: Color = Color::Rgb(40, 44, 52);
const OPEN_BLUE: Color = Color::Rgb(97, 175, 239);
const MAX_WIDTH: u16 = 110;
const NAME_MAX: usize = 28;
const TAGS_MAX: usize = 32;
const PARENT_MAX: usize = 30;
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const FLASH_FOR: Duration = Duration::from_millis(1500);
const TICK: Duration = Duration::from_millis(100);
const HINTS: [(&str, &str); 4] =
    [("enter", "travel"), ("ctrl-o", "resume tool"), ("ctrl-e", "editor"), ("esc", "quit")];

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// False with NO_COLOR: bold and dim stay, colors go.
    pub color: bool,
    /// COLORTERM says the terminal shows 24-bit color.
    pub truecolor: bool,
}

impl Theme {
    pub fn from_env() -> Theme {
        let color = std::env::var_os("NO_COLOR").map_or(true, |v| v.is_empty());
        let truecolor = std::env::var("COLORTERM")
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "truecolor" | "24bit"))
            .unwrap_or(false);
        Theme { color, truecolor }
    }

    /// Map a color to what the terminal can show. None means no color.
    fn map(&self, c: Color) -> Option<Color> {
        if !self.color {
            return None;
        }
        match c {
            Color::Rgb(r, g, b) if !self.truecolor => Some(Color::Indexed(rgb_to_256(r, g, b))),
            c => Some(c),
        }
    }

    fn fg(&self, s: Style, c: Color) -> Style {
        match self.map(c) {
            Some(c) => s.fg(c),
            None => s,
        }
    }

    fn accent(&self) -> Color {
        if self.truecolor {
            GOLD
        } else {
            Color::Yellow
        }
    }

    fn sel_bg(&self) -> Option<Color> {
        if !self.color {
            None
        } else if self.truecolor {
            Some(SEL_BG)
        } else {
            Some(Color::DarkGray)
        }
    }

    /// Apply the theme rules (NO_COLOR, 256 colors) to a style that may hold a color.
    fn apply(&self, s: Style) -> Style {
        let mut out = Style::default().add_modifier(s.add_modifier).remove_modifier(s.sub_modifier);
        if let Some(c) = s.fg.and_then(|c| self.map(c)) {
            out = out.fg(c);
        }
        out
    }
}

/// Nearest xterm 256-color index for an RGB color.
fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let q = |v: u8| -> u8 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            (v - 35) / 40
        }
    };
    16 + 36 * q(r) + 6 * q(g) + q(b)
}

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

// ---------------------------------------------------------------------------
// Pure layout helpers

/// Display width of one char.
fn cw(c: char) -> usize {
    let mut b = [0u8; 4];
    Span::raw(&*c.encode_utf8(&mut b)).width()
}

fn sw(s: &str) -> usize {
    Span::raw(s).width()
}

/// Shown chars with their index in the source text. None marks the ellipsis.
pub type Fit = Vec<(char, Option<usize>)>;

fn fit_all(chars: Vec<char>) -> Fit {
    chars.into_iter().enumerate().map(|(i, c)| (c, Some(i))).collect()
}

/// Fit `s` into `w` columns. Cut the end and add "…" if it is too long.
pub fn fit_end(s: &str, w: usize) -> Fit {
    let chars: Vec<char> = s.chars().collect();
    if chars.iter().map(|&c| cw(c)).sum::<usize>() <= w {
        return fit_all(chars);
    }
    if w == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut used = 0;
    for (i, &c) in chars.iter().enumerate() {
        if used + cw(c) > w - 1 {
            break;
        }
        used += cw(c);
        out.push((c, Some(i)));
    }
    out.push(('…', None));
    out
}

/// Fit `s` into `w` columns. Cut the middle and put "…" there if it is too long.
/// The tail gets the extra column, because the end of a path says more.
pub fn fit_middle(s: &str, w: usize) -> Fit {
    let chars: Vec<char> = s.chars().collect();
    if chars.iter().map(|&c| cw(c)).sum::<usize>() <= w {
        return fit_all(chars);
    }
    if w == 0 {
        return Vec::new();
    }
    let budget = w - 1;
    let head_w = budget / 2;
    let tail_w = budget - head_w;
    let mut head = Vec::new();
    let mut used = 0;
    for (i, &c) in chars.iter().enumerate() {
        if used + cw(c) > head_w {
            break;
        }
        used += cw(c);
        head.push((c, Some(i)));
    }
    let mut tail = Vec::new();
    used = 0;
    for (i, &c) in chars.iter().enumerate().rev() {
        if i < head.len() || used + cw(c) > tail_w {
            break;
        }
        used += cw(c);
        tail.push((c, Some(i)));
    }
    tail.reverse();
    head.push(('…', None));
    head.extend(tail);
    head
}

/// Split a shown path into (parent, name, char index where the name starts).
pub fn split_path(shown: &str) -> (String, String, usize) {
    let chars: Vec<char> = shown.chars().collect();
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    match chars.iter().rposition(|&c| is_sep(c)) {
        Some(p) if p + 1 < chars.len() => {
            let parent: String = if p == 0 { "/".into() } else { chars[..p].iter().collect() };
            (parent, chars[p + 1..].iter().collect(), p + 1)
        }
        _ => (String::new(), shown.to_string(), 0),
    }
}

/// Column widths of a row, in display columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cols {
    pub status: usize,
    pub name: usize,
    pub parent: usize,
    pub tags: usize,
    pub time: usize,
}

/// A row is: bar(1) status star(2) name gap(2) parent gap(2) tags gap(>=1) time pad(1).
/// The parent gets at most 30 columns, and keeps at least 12 before the tags get space.
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
    let tags = tags_max.min(TAGS_MAX).min(avail.saturating_sub(parent_max.min(12)));
    let mut parent = parent_max.min(avail - tags);
    if parent < 4 {
        parent = 0;
    }
    Cols { status, name, parent, tags, time }
}

/// How many tags fit in `w` columns, joined by " · ". The bool says if " +N" follows.
pub fn fit_tags(widths: &[usize], w: usize) -> (usize, bool) {
    let mut used = 0;
    for (i, &tw) in widths.iter().enumerate() {
        let add = if i == 0 { tw } else { 3 + tw };
        let more = widths.len() - i - 1;
        // Keep room for " +N" while tags stay hidden.
        let reserve = if more > 0 { 2 + more.to_string().len() } else { 0 };
        if used + add + reserve > w {
            let fits_more = used + 2 + (widths.len() - i).to_string().len() <= w;
            return (i, fits_more && i > 0);
        }
        used += add;
    }
    (widths.len(), false)
}

/// One line of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// A section title in the sessions view. Not selectable.
    Header(Status),
    /// Index into the entries.
    Item(usize),
}

const GROUPS: [Status; 4] = [Status::Waiting, Status::Ready, Status::Working, Status::Done];

/// Build the list rows. The sessions view groups the matches by status, in rank order.
pub fn build_rows(view: View, matches: &[usize], status: impl Fn(usize) -> Option<Status>) -> Vec<Row> {
    match view {
        View::Folders => matches.iter().map(|&i| Row::Item(i)).collect(),
        View::Sessions => {
            let mut rows = Vec::new();
            for g in GROUPS {
                let items: Vec<Row> =
                    matches.iter().filter(|&&i| status(i) == Some(g)).map(|&i| Row::Item(i)).collect();
                if !items.is_empty() {
                    rows.push(Row::Header(g));
                    rows.extend(items);
                }
            }
            rows.extend(matches.iter().filter(|&&i| status(i).is_none()).map(|&i| Row::Item(i)));
            rows
        }
    }
}

pub fn first_item(rows: &[Row]) -> Option<usize> {
    rows.iter().position(|r| matches!(r, Row::Item(_)))
}

/// Move `delta` items from row `from`. Headers are skipped. Stops at the ends.
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

/// First visible row so that `selected` is on screen. Shows a header above the first item.
pub fn scroll(rows: &[Row], selected: usize, offset: usize, height: usize) -> usize {
    if height == 0 || rows.is_empty() {
        return 0;
    }
    let mut off = offset.min(rows.len() - 1);
    if selected < off {
        off = selected;
    } else if selected >= off + height {
        off = selected + 1 - height;
    }
    if off == selected && off > 0 && height > 1 && matches!(rows[off - 1], Row::Header(_)) {
        off -= 1;
    }
    off
}

/// Inline viewport height: the rows plus chrome, at most 45% of the terminal (min 10).
pub fn inline_height(rows: usize, term_h: u16) -> u16 {
    let chrome = 3;
    let cap = (term_h as usize * 45 / 100).max(10);
    ((rows.max(1) + chrome).min(cap) as u16).min(term_h.max(1))
}

/// Key hints for the bottom border. Hints that do not fit in `w` columns are left out.
fn hint_spans(w: usize) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(" ")];
    let mut used = 2;
    let mut first = true;
    for (k, l) in HINTS {
        let add = sw(k) + 1 + sw(l) + if first { 0 } else { 3 };
        if used + add > w {
            continue;
        }
        if !first {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(Span::styled(k, Style::default().add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(" {l}"), dim()));
        used += add;
        first = false;
    }
    spans.push(Span::raw(" "));
    spans
}

// ---------------------------------------------------------------------------
// State

struct State {
    data: Data,
    best: Best,
    open: HashMap<String, OpenEditor>,
    query: String,
    matches: Vec<usize>,
    rows: Vec<Row>,
    selected: usize,
    offset: usize,
    now: i64,
    home: Option<String>,
    matcher: Matcher,
    view: View,
    theme: Theme,
    start: Instant,
    flash: Option<(String, Instant)>,
    /// A working spinner is on screen, so redraw on each tick.
    animating: bool,
}

impl State {
    fn new(data: Data, query: &str, now: i64, view: View, theme: Theme, home: Option<String>) -> State {
        let mut st = State {
            data: Data::default(),
            best: Best::new(),
            open: HashMap::new(),
            query: query.to_string(),
            matches: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            now,
            home,
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            view,
            theme,
            start: Instant::now(),
            flash: None,
            animating: false,
        };
        st.set_data(data);
        st
    }

    /// Replace the data. The query and the selected folder stay.
    fn set_data(&mut self, data: Data) {
        let keep = self.selected_entry().map(|e| e.path.clone());
        self.best = best_by_path(&data.sessions);
        self.open = data.editors.iter().map(|o| (o.path.clone(), o.clone())).collect();
        self.data = data;
        self.refilter_keep(keep.as_deref());
    }

    fn status(&self, i: usize) -> Option<Status> {
        self.best.get(&self.data.entries[i].path).map(|s| s.status)
    }

    fn refilter_keep(&mut self, keep: Option<&str>) {
        self.matches = filter(&self.data.entries, &self.best, &self.query);
        self.rows = build_rows(self.view, &self.matches, |i| self.status(i));
        let entries = &self.data.entries;
        let kept = keep.and_then(|p| {
            self.rows.iter().position(|r| matches!(r, Row::Item(i) if entries[*i].path == p))
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

    fn flash(&mut self, msg: &str) {
        self.flash = Some((msg.to_string(), Instant::now() + FLASH_FOR));
    }

    /// Row content for entry `i`. With `hl`, also the match positions.
    fn row_view(&mut self, i: usize, hl: bool) -> RowView {
        let e = &self.data.entries[i];
        let shown = display_path(&e.path, self.home.as_deref());
        let (parent, name, name_off) = split_path(&shown);
        let mut name_hl = Vec::new();
        let mut parent_hl = Vec::new();
        if hl {
            // Match positions refer to the full path. Map them onto the shown text.
            let skip = e.path.chars().count() as i64 - shown.chars().count() as i64;
            let parent_len = parent.chars().count();
            for p in match_positions(&e.path, &self.query, &mut self.matcher) {
                let q = p as i64 - skip;
                // Chars inside the home prefix have no match in the shown text.
                if q < 0 || (skip != 0 && q < 1) {
                    continue;
                }
                let q = q as usize;
                if q >= name_off {
                    name_hl.push(q - name_off);
                } else if name_off > 0 && q < parent_len {
                    parent_hl.push(q);
                }
            }
        }
        let best = self.best.get(&e.path);
        let open = self.open.get(&e.path);
        let mut tags: Vec<(String, bool)> =
            e.tags.iter().map(|t| (t.tool.clone(), open.is_some_and(|o| o.tag == t.tool))).collect();
        if let Some(o) = open {
            if !e.has_tag(&o.tag) {
                tags.insert(0, (o.tag.clone(), true));
            }
        }
        let then = match (self.view, best) {
            (View::Sessions, Some(b)) => b.updated_at,
            _ => e.last_visit,
        };
        RowView {
            name,
            parent,
            name_hl,
            parent_hl,
            pinned: e.pinned,
            status: best.map(|b| b.status),
            editor_open: open.is_some(),
            tags,
            time: time_ago(self.now, then),
        }
    }
}

// ---------------------------------------------------------------------------
// Drawing

/// What one row shows, before layout.
struct RowView {
    name: String,
    parent: String,
    name_hl: Vec<usize>,
    parent_hl: Vec<usize>,
    pinned: bool,
    status: Option<Status>,
    editor_open: bool,
    /// (tag, the editor window of this tag is open now)
    tags: Vec<(String, bool)>,
    time: String,
}

fn tags_width(tags: &[(String, bool)]) -> usize {
    if tags.is_empty() {
        0
    } else {
        tags.iter().map(|(t, _)| sw(t)).sum::<usize>() + 3 * (tags.len() - 1)
    }
}

struct Ctx {
    theme: Theme,
    view: View,
    frame: usize,
}

/// Glyph, word and style for a status.
fn status_look(s: Status, frame: usize) -> (char, &'static str, Style) {
    match s {
        Status::Waiting => ('●', "needs you", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Status::Ready => ('○', "your turn", Style::default()),
        Status::Working => (SPINNER[frame % SPINNER.len()], "working", Style::default().fg(Color::Cyan)),
        Status::Done => ('✓', "done", Style::default().fg(Color::Green).add_modifier(Modifier::DIM)),
    }
}

/// Spans of one row, with a known width.
struct RowBuf {
    spans: Vec<Span<'static>>,
    used: usize,
    base: Style,
}

impl RowBuf {
    fn push(&mut self, text: String, style: Style) {
        self.used += sw(&text);
        self.spans.push(Span::styled(text, style));
    }
    fn pad(&mut self, n: usize) {
        if n > 0 {
            self.used += n;
            self.spans.push(Span::styled(" ".repeat(n), self.base));
        }
    }
    /// Pad up to column `to`.
    fn pad_to(&mut self, to: usize) {
        self.pad(to.saturating_sub(self.used));
    }
    fn push_fit(&mut self, fit: Fit, hl: &[usize], style: Style, hl_style: Style) {
        for (c, i) in fit {
            let s = if i.is_some_and(|i| hl.contains(&i)) { hl_style } else { style };
            self.push(c.to_string(), s);
        }
    }
}

fn compose_row(rv: &RowView, cols: &Cols, selected: bool, ctx: &Ctx, width: usize) -> Line<'static> {
    let t = &ctx.theme;
    let mut base = Style::default();
    if selected {
        if let Some(bg) = t.sel_bg() {
            base = base.bg(bg);
        }
    }
    let hl = t.fg(base, t.accent()).add_modifier(Modifier::BOLD);
    let mut b = RowBuf { spans: Vec::new(), used: 0, base };

    // Accent bar.
    if selected {
        b.push("▌".into(), t.fg(base, t.accent()));
    } else {
        b.pad(1);
    }

    // Status glyph, and the word in the sessions view. An agent wins over an editor.
    let end = b.used + cols.status;
    if let Some(s) = rv.status {
        let (g, word, style) = status_look(s, ctx.frame);
        let style = base.patch(t.apply(style));
        b.push(g.to_string(), style);
        if ctx.view == View::Sessions {
            b.push(format!(" {word}"), style);
        }
    } else if rv.editor_open {
        b.push("▣".into(), t.fg(base, OPEN_BLUE));
    }
    b.pad_to(end);

    // Pin.
    if rv.pinned {
        b.push("★ ".into(), t.fg(base, t.accent()));
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
    b.push_fit(fit_end(&rv.name, cols.name), &rv.name_hl, name_style, hl);
    b.pad_to(end);

    // Parent.
    if cols.parent > 0 {
        b.pad(2);
        let end = b.used + cols.parent;
        b.push_fit(fit_middle(&rv.parent, cols.parent), &rv.parent_hl, base.add_modifier(Modifier::DIM), hl);
        b.pad_to(end);
    }

    // Tags.
    if cols.tags > 0 && !rv.tags.is_empty() {
        b.pad(2);
        let widths: Vec<usize> = rv.tags.iter().map(|(t, _)| sw(t)).collect();
        let (kept, more) = fit_tags(&widths, cols.tags);
        for (k, (tag, open)) in rv.tags.iter().take(kept).enumerate() {
            if k > 0 {
                b.push(" · ".into(), base.add_modifier(Modifier::DIM));
            }
            // An open editor keeps its brand color, in bold.
            let s = t.fg(base, tag_color(tag));
            let s = if *open { s.add_modifier(Modifier::BOLD) } else { s };
            b.push(tag.clone(), s);
        }
        if more {
            b.push(format!(" +{}", rv.tags.len() - kept), base.add_modifier(Modifier::DIM));
        }
    }

    // Time, right aligned, one column from the edge.
    let tw = sw(&rv.time);
    b.pad(width.saturating_sub(b.used + tw + 1).max(1));
    b.push(rv.time.clone(), base.add_modifier(Modifier::DIM));
    b.pad_to(width);
    Line::from(b.spans)
}

fn draw(f: &mut Frame, st: &mut State) {
    let t = st.theme;
    let area = f.area();
    let outer = Rect { width: area.width.min(MAX_WIDTH), ..area };
    let accent = t.fg(Style::default(), t.accent());
    let count = format!(" {}/{} ", st.matches.len(), st.data.entries.len());

    let flash = st.flash.as_ref().filter(|(_, until)| Instant::now() < *until).map(|(m, _)| m.clone());
    let bottom = match flash {
        Some(m) => Line::from(vec![Span::raw(" "), Span::styled(m, dim()), Span::raw(" ")]),
        None => Line::from(hint_spans((outer.width as usize).saturating_sub(4))),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(dim())
        .title_top(Line::from(Span::styled(" grace ", accent.add_modifier(Modifier::BOLD))).left_aligned())
        .title_top(Line::from(Span::styled(count, dim())).right_aligned())
        .title_bottom(bottom);
    let inner = block.inner(outer);
    f.render_widget(block, outer);
    st.animating = false;
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Query line.
    let mut q = vec![
        Span::raw(" "),
        Span::styled("❯ ", accent.add_modifier(Modifier::BOLD)),
        Span::raw(st.query.clone()),
        Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
    ];
    if st.query.is_empty() {
        q.push(Span::styled(" search folders, tags, tools", dim()));
    }
    f.render_widget(Paragraph::new(Line::from(q)), Rect { height: 1, ..inner });

    let list = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
    if list.height == 0 {
        return;
    }
    let width = list.width as usize;
    if st.data.entries.is_empty() {
        let msg = "No folders yet. cd around, or run grace import.";
        let y = list.y + list.height.saturating_sub(1) / 2;
        let p = Paragraph::new(Span::styled(msg, dim())).alignment(Alignment::Center);
        f.render_widget(p, Rect { y, height: 1, ..list });
        return;
    }
    if st.rows.is_empty() {
        f.render_widget(Paragraph::new(Span::styled("   No matches", dim())), Rect { height: 1, ..list });
        return;
    }

    // Column widths come from all entries, so the layout stays still while you type.
    let status_w = if st.view == View::Sessions { 12 } else { 2 };
    let (mut name_max, mut parent_max, mut tags_max, mut time_max) = (0, 0, 0, 0);
    for i in 0..st.data.entries.len() {
        let rv = st.row_view(i, false);
        name_max = name_max.max(sw(&rv.name));
        parent_max = parent_max.max(sw(&rv.parent));
        tags_max = tags_max.max(tags_width(&rv.tags));
        time_max = time_max.max(sw(&rv.time));
    }
    let cols = layout_cols(width, status_w, name_max, parent_max, tags_max, time_max);

    let height = list.height as usize;
    st.offset = scroll(&st.rows, st.selected, st.offset, height);
    let frame = (st.start.elapsed().as_millis() / TICK.as_millis()) as usize;
    let ctx = Ctx { theme: t, view: st.view, frame };
    let visible: Vec<(usize, Row)> = st.rows.iter().copied().enumerate().skip(st.offset).take(height).collect();
    let mut lines = Vec::new();
    for (ri, row) in visible {
        match row {
            Row::Header(s) => {
                let label = s.label().to_uppercase();
                lines.push(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(label, dim().add_modifier(Modifier::BOLD)),
                ]));
            }
            Row::Item(i) => {
                let rv = st.row_view(i, true);
                st.animating |= rv.status == Some(Status::Working);
                lines.push(compose_row(&rv, &cols, ri == st.selected, &ctx, width));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), list);
}

// ---------------------------------------------------------------------------
// Terminal

/// Crossterm on stderr. On unix it asks the cursor position through /dev/tty,
/// because crossterm writes that query to stdout, and the shell captures stdout.
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
fn parse_cpr(buf: &[u8]) -> Option<Position> {
    let end = buf.iter().rposition(|&b| b == b'R')?;
    let start = buf[..end].windows(2).rposition(|w| w == b"\x1b[")?;
    let body = std::str::from_utf8(&buf[start + 2..end]).ok()?;
    let (row, col) = body.split_once(';')?;
    let row: u16 = row.parse().ok()?;
    let col: u16 = col.parse().ok()?;
    Some(Position { x: col.saturating_sub(1), y: row.saturating_sub(1) })
}

#[cfg(unix)]
fn tty_cursor_position() -> io::Result<Position> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    let mut tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty")?;
    tty.write_all(b"\x1b[6n")?;
    tty.flush()?;
    let fd = tty.as_raw_fd();
    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut buf = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "no cursor position reply"));
        }
        // select works on ttys on macOS. poll does not.
        // SAFETY: the fd_set is zeroed and holds one open fd below FD_SETSIZE.
        let ready = unsafe {
            let mut set: libc::fd_set = std::mem::zeroed();
            libc::FD_ZERO(&mut set);
            libc::FD_SET(fd, &mut set);
            let mut tv = libc::timeval { tv_sec: left.as_secs() as _, tv_usec: left.subsec_micros() as _ };
            libc::select(fd + 1, &mut set, std::ptr::null_mut(), std::ptr::null_mut(), &mut tv)
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
        buf.extend_from_slice(&b[..n]);
        if let Some(p) = parse_cpr(&buf) {
            return Ok(p);
        }
    }
}

static FULLSCREEN: AtomicBool = AtomicBool::new(false);

fn restore() {
    let _ = disable_raw_mode();
    if FULLSCREEN.load(Ordering::SeqCst) {
        let _ = execute!(io::stderr(), LeaveAlternateScreen, Show);
    } else {
        let _ = execute!(io::stderr(), Show);
    }
}

fn run<B: Backend>(terminal: &mut Terminal<B>, st: &mut State, opts: &mut Opts) -> Result<Option<Choice>> {
    let mut first = true;
    let mut next_reload: Option<Instant> = None;
    loop {
        terminal.draw(|f| draw(f, st))?;
        if first {
            first = false;
            if let Some(job) = opts.after_first_frame.take() {
                job();
            }
            if opts.reload.is_some() {
                next_reload = Some(Instant::now() + opts.reload_every.unwrap_or(Duration::ZERO));
            }
        }

        // Wait for a key, or for the next timer: spinner tick, flash end, reload.
        let now = Instant::now();
        let mut wait = Duration::from_secs(60);
        if st.animating {
            let into = (now - st.start).as_millis() % TICK.as_millis();
            wait = wait.min(TICK - Duration::from_millis(into as u64));
        }
        if let Some((_, until)) = &st.flash {
            wait = wait.min(until.saturating_duration_since(now));
        }
        if let Some(at) = next_reload {
            wait = wait.min(at.saturating_duration_since(now));
        }
        if !event::poll(wait)? {
            let now = Instant::now();
            if st.flash.as_ref().is_some_and(|(_, until)| now >= *until) {
                st.flash = None;
            }
            if next_reload.is_some_and(|at| now >= at) {
                if let Some(d) = opts.reload.as_mut().and_then(|reload| reload()) {
                    st.now = crate::model::now();
                    st.set_data(d);
                }
                next_reload = opts.reload_every.map(|d| Instant::now() + d);
            }
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let choose = |st: &State, action: Action| {
            st.selected_entry().map(|e| Choice { entry: e.clone(), action })
        };
        match key.code {
            KeyCode::Esc => return Ok(None),
            KeyCode::Char('c') if ctrl => return Ok(None),
            KeyCode::Enter => {
                if let Some(c) = choose(st, Action::Travel) {
                    return Ok(Some(c));
                }
            }
            KeyCode::Char('o') if ctrl => {
                if let Some(c) = choose(st, Action::Resume) {
                    return Ok(Some(c));
                }
            }
            KeyCode::Char('e') if ctrl => {
                if let Some(e) = st.selected_entry().cloned() {
                    match (opts.editor_cmd)(&e, st.open.get(&e.path)) {
                        Some(cmd) => return Ok(Some(Choice { entry: e, action: Action::Editor(cmd) })),
                        None => st.flash("no editor used here"),
                    }
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

fn run_fullscreen(st: &mut State, opts: &mut Opts) -> Result<Option<Choice>> {
    FULLSCREEN.store(true, Ordering::SeqCst);
    execute!(io::stderr(), EnterAlternateScreen, Hide)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stderr()))?;
    run(&mut terminal, st, opts)
}

fn run_inline(st: &mut State, opts: &mut Opts) -> Result<Option<Choice>> {
    let (_, term_h) = crossterm::terminal::size()?;
    let all: Vec<usize> = (0..st.data.entries.len()).collect();
    let rows = build_rows(st.view, &all, |i| st.status(i)).len();
    let h = inline_height(rows, term_h);
    // Without a cursor position reply, use the full screen.
    let mut terminal = match Terminal::with_options(
        TtyBackend(CrosstermBackend::new(io::stderr())),
        TerminalOptions { viewport: Viewport::Inline(h) },
    ) {
        Ok(t) => t,
        Err(_) => return run_fullscreen(st, opts),
    };
    terminal.hide_cursor()?;
    let result = run(&mut terminal, st, opts);
    // Clear the drawing, so the prompt comes back where the picker was.
    let _ = terminal.clear();
    let _ = terminal.show_cursor();
    result
}

pub fn pick(data: Data, query: &str, now: i64, mut opts: Opts) -> Result<Option<Choice>> {
    let home = crate::platform::home_dir().map(|h| h.to_string_lossy().into_owned());
    let mut st = State::new(data, query, now, opts.view, Theme::from_env(), home);
    // Fast travel: one match for a given query, so skip the UI.
    if !query.trim().is_empty() && st.matches.len() == 1 {
        let entry = st.data.entries[st.matches[0]].clone();
        return Ok(Some(Choice { entry, action: Action::Travel }));
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        default_hook(info);
    }));

    enable_raw_mode()?;
    let result =
        if opts.fullscreen { run_fullscreen(&mut st, &mut opts) } else { run_inline(&mut st, &mut opts) };
    restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Tag;
    use ratatui::backend::TestBackend;

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

    fn session(id: i64, path: &str, tool: &str, status: Status, updated_at: i64) -> Session {
        Session { id, path: path.into(), tool: tool.into(), status, shell_pid: None, started_at: 0, updated_at }
    }

    fn fit_text(f: &Fit) -> String {
        f.iter().map(|(c, _)| c).collect()
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
        let done = Session { id: 2, status: Status::Done, updated_at: 9, ..s.clone() };
        let best = best_by_path(&[done, s]);
        assert_eq!(best["/home/u/beta"].status, Status::Waiting);
        assert_eq!(filter(&sample(), &best, "needs"), vec![1]);
    }

    #[test]
    fn ready_badge_and_search() {
        let s = session(1, "/home/u/beta", "codex", Status::Ready, 5);
        let work = Session { id: 2, status: Status::Working, updated_at: 9, ..s.clone() };
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
    fn time_ago_units() {
        assert_eq!(time_ago(100, 90), "now");
        assert_eq!(time_ago(1000, 700), "5m");
        assert_eq!(time_ago(3 * 3600, 0), "3h");
        assert_eq!(time_ago(2 * 86400, 0), "2d");
    }

    #[test]
    fn truncation() {
        assert_eq!(fit_text(&fit_end("knn", 5)), "knn");
        assert_eq!(fit_text(&fit_end("kmeans-clustering", 8)), "kmeans-…");
        assert_eq!(fit_text(&fit_end("abc", 0)), "");
        assert_eq!(fit_text(&fit_middle("~/code/aiml", 20)), "~/code/aiml");
        let m = fit_middle("~/code/very/deep/nested/aiml", 12);
        assert_eq!(fit_text(&m), "~/cod…d/aiml");
        assert_eq!(m.iter().map(|(c, _)| cw(*c)).sum::<usize>(), 12);
        // The ellipsis has no source index. The other chars keep theirs.
        assert_eq!(m[5], ('…', None));
        assert_eq!(m[6].1, Some(22));
    }

    #[test]
    fn split_paths() {
        assert_eq!(split_path("~/code/knn"), ("~/code".into(), "knn".into(), 7));
        assert_eq!(split_path("/tmp"), ("/".into(), "tmp".into(), 1));
        assert_eq!(split_path("~"), ("".into(), "~".into(), 0));
    }

    #[test]
    fn columns() {
        let c = layout_cols(108, 2, 40, 20, 14, 3);
        assert_eq!((c.name, c.parent, c.tags), (28, 20, 14));
        // Narrow: the parent keeps 12 columns before the tags grow.
        let c = layout_cols(60, 2, 10, 30, 30, 3);
        assert_eq!((c.name, c.parent, c.tags), (10, 12, 60 - 14 - 10 - 12));
        // Very narrow: no parent at all.
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
        let st = |i: usize| [Some(Status::Done), Some(Status::Waiting), Some(Status::Done)][i];
        let rows = build_rows(View::Sessions, &[0, 1, 2], st);
        assert_eq!(
            rows,
            vec![Row::Header(Status::Waiting), Row::Item(1), Row::Header(Status::Done), Row::Item(0), Row::Item(2)]
        );
        assert_eq!(first_item(&rows), Some(1));
        assert_eq!(step_item(&rows, 1, 1), 3);
        assert_eq!(step_item(&rows, 3, -1), 1);
        assert_eq!(step_item(&rows, 1, -1), 1);
        assert_eq!(step_item(&rows, 4, 1), 4);
        // The header above the selected item stays on screen.
        assert_eq!(scroll(&rows, 1, 0, 3), 0);
        assert_eq!(scroll(&rows, 3, 0, 2), 2);
        assert_eq!(scroll(&rows, 3, 3, 2), 2);
        assert_eq!(build_rows(View::Folders, &[2, 0], st), vec![Row::Item(2), Row::Item(0)]);
    }

    #[test]
    fn heights() {
        assert_eq!(inline_height(3, 50), 6);
        assert_eq!(inline_height(100, 50), 22);
        assert_eq!(inline_height(100, 12), 10);
        assert_eq!(inline_height(100, 6), 6);
        assert_eq!(inline_height(0, 50), 4);
    }

    #[test]
    fn cursor_reply() {
        assert_eq!(parse_cpr(b"x\x1b[12;5R"), Some(Position { x: 4, y: 11 }));
        assert_eq!(parse_cpr(b"\x1b[12;"), None);
    }

    #[test]
    fn colors_follow_the_terminal() {
        let t = Theme { color: true, truecolor: false };
        assert_eq!(t.accent(), Color::Yellow);
        assert_eq!(t.sel_bg(), Some(Color::DarkGray));
        assert_eq!(t.map(Color::Rgb(0, 122, 204)), Some(Color::Indexed(32)));
        let t = Theme { color: false, truecolor: true };
        assert_eq!(t.map(Color::Red), None);
        assert_eq!(t.sel_bg(), None);
        assert_ne!(tag_color("vscode"), tag_color("antigravity"));
    }

    fn render(st: &mut State, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, st)).unwrap();
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

    const NOW: i64 = 1_000_000;

    fn demo() -> Data {
        let mut e = vec![
            entry("/home/u/code/aiml/knn", &["claude", "vscode"]),
            entry("/home/u/code/aiml/svm", &["claude"]),
            entry("/home/u/work/api-server", &["codex", "cursor"]),
            entry("/home/u/code/aiml/kmeans-clustering-experiments", &["antigravity"]),
            entry("/home/u/notes", &[]),
            entry("/srv/data/archive/very/deep/nested/folder/structure/old-site", &["zed", "neovim", "gemini"]),
        ];
        let ago = [120, 300, 30, 720, 3 * 86400, 40 * 86400];
        for (x, a) in e.iter_mut().zip(ago) {
            x.last_visit = NOW - a;
        }
        e[0].pinned = true;
        let sessions = vec![
            session(1, "/home/u/code/aiml/knn", "claude", Status::Waiting, NOW - 120),
            session(2, "/home/u/code/aiml/svm", "claude", Status::Ready, NOW - 300),
            session(3, "/home/u/work/api-server", "codex", Status::Working, NOW - 30),
            session(4, "/home/u/code/aiml/kmeans-clustering-experiments", "antigravity", Status::Done, NOW - 720),
        ];
        let editors = vec![OpenEditor {
            path: "/home/u/notes".into(),
            tag: "vscode".into(),
            open_cmd: "code '/home/u/notes'".into(),
        }];
        Data { entries: e, sessions, editors }
    }

    fn theme() -> Theme {
        Theme { color: true, truecolor: true }
    }

    #[test]
    fn render_folders() {
        let mut st = State::new(demo(), "", NOW, View::Folders, theme(), Some("/home/u".into()));
        let out = render(&mut st, 100, 10);
        println!("{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("╭ grace ─") && lines[0].ends_with(" 6/6 ╮"), "{}", lines[0]);
        assert!(lines[1].contains("❯   search folders, tags, tools"), "{}", lines[1]);
        assert!(lines[2].starts_with("│▌● ★ knn"), "{}", lines[2]);
        assert!(lines[2].contains("~/code/aiml") && lines[2].contains("claude · vscode"), "{}", lines[2]);
        assert!(lines[2].ends_with("2m │"), "{}", lines[2]);
        assert!(lines[4].contains("⠋") && lines[4].contains("codex · cursor"), "{}", lines[4]);
        assert!(lines[5].contains("kmeans-clustering-experimen…"), "{}", lines[5]);
        assert!(lines[6].contains("▣") && lines[6].contains("notes") && lines[6].contains("vscode"));
        assert!(lines[7].contains("old-site") && lines[7].contains("…"), "{}", lines[7]);
        assert!(lines[9].starts_with("╰ enter travel · ctrl-o resume tool · ctrl-e editor · esc quit ─"));
        assert_eq!(lines[0].chars().count(), 100);

        // Matches in a narrow terminal.
        st.query = "aiml".into();
        st.refilter();
        let out = render(&mut st, 70, 7);
        println!("{out}");
        assert!(out.lines().next().unwrap().ends_with(" 3/6 ╮"));

        st.query = "zzzqqq".into();
        st.refilter();
        let out = render(&mut st, 70, 4);
        println!("{out}");
        assert!(out.contains("No matches"));

        let mut empty = State::new(Data::default(), "", 0, View::Folders, theme(), None);
        let out = render(&mut empty, 70, 5);
        println!("{out}");
        assert!(out.contains("No folders yet. cd around, or run grace import."));
    }

    #[test]
    fn render_sessions() {
        let mut d = demo();
        d.entries.truncate(4);
        let mut st = State::new(d, "", NOW, View::Sessions, theme(), Some("/home/u".into()));
        let out = render(&mut st, 100, 11);
        println!("{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[2].trim_matches('│').trim(), "NEEDS YOU");
        assert!(lines[3].starts_with("│▌● needs you ★ knn"), "{}", lines[3]);
        assert!(lines[4].contains("YOUR TURN"));
        assert!(lines[5].contains("○ your turn") && lines[5].contains("svm"));
        assert!(lines[7].contains("working") && lines[7].contains("api-server"));
        assert!(lines[9].contains("✓ done"));
        // The cursor skips the header.
        st.mv(1);
        assert_eq!(st.selected_entry().unwrap().path, "/home/u/code/aiml/svm");
        // A reload keeps the selection by path.
        let mut d = demo();
        d.entries.truncate(4);
        d.sessions[1].status = Status::Waiting;
        st.set_data(d);
        assert_eq!(st.selected_entry().unwrap().path, "/home/u/code/aiml/svm");
        assert_eq!(st.rows[0], Row::Header(Status::Waiting));
        assert_eq!(st.rows[3], Row::Header(Status::Working));
    }

    #[test]
    fn flash_shows_in_the_border() {
        let mut st = State::new(demo(), "", NOW, View::Folders, theme(), Some("/home/u".into()));
        st.flash("no editor used here");
        let out = render(&mut st, 100, 6);
        assert!(out.lines().last().unwrap().starts_with("╰ no editor used here ─"), "{out}");
    }
}
