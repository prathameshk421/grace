//! `grace tree`: a file tree that marks git changes.
use crate::diffview::mark_color;
use crate::git::{self, Mark};
use crate::platform;
use crate::theme::{Theme, ToastKind};
use crate::tui::{self, Flow};
use crate::widgets::{
    center_msg, cut, cut_tail, hint_line, render_modal, render_scrollbar, sw, toast_line, Toast,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const HINTS: [(&str, &str); 5] = [
    ("⏎", "toggle/open"),
    ("/", "filter"),
    ("c", "changed"),
    ("r", "refresh"),
    ("?", "keys"),
];
const SKIP: [&str; 3] = [".git", "node_modules", "target"];
const MAX_FILES: usize = 20_000;
const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub path: String,
    pub dir: bool,
    pub children: Vec<Node>,
}

#[derive(Default)]
struct Building {
    dirs: BTreeMap<String, Building>,
    files: HashSet<String>,
}

/// Build a sorted tree iteratively (depth-safe against deeply nested paths).
pub fn build_tree(paths: &[String]) -> Node {
    let mut root = Building::default();
    for p in paths {
        let parts: Vec<&str> = p
            .split('/')
            .filter(|s| !s.is_empty())
            .take(MAX_DEPTH)
            .collect();
        let Some((file, dirs)) = parts.split_last() else {
            continue;
        };
        let mut cur = &mut root;
        for d in dirs {
            cur = cur.dirs.entry((*d).to_string()).or_default();
        }
        if !cur.dirs.contains_key(*file) {
            cur.files.insert((*file).to_string());
        }
    }
    convert_iterative(root)
}

fn convert_iterative(root: Building) -> Node {
    enum Frame {
        Enter {
            name: String,
            path: String,
            b: Building,
        },
        Exit {
            name: String,
            path: String,
            dir_count: usize,
            files: Vec<String>,
        },
    }
    let key = |s: &str| (s.to_lowercase(), s.to_string());
    let mut stack = vec![Frame::Enter {
        name: String::new(),
        path: String::new(),
        b: root,
    }];
    let mut done: Vec<Node> = Vec::new();
    while let Some(frame) = stack.pop() {
        match frame {
            Frame::Enter { name, path, b } => {
                let mut dirs: Vec<(String, Building)> = b.dirs.into_iter().collect();
                dirs.sort_by_key(|(n, _)| key(n));
                let mut files: Vec<String> = b.files.into_iter().collect();
                files.sort_by_key(|n| key(n));
                let dir_count = dirs.len();
                stack.push(Frame::Exit {
                    name,
                    path: path.clone(),
                    dir_count,
                    files,
                });
                for (dn, db) in dirs.into_iter().rev() {
                    let child_path = if path.is_empty() {
                        dn.clone()
                    } else {
                        format!("{path}/{dn}")
                    };
                    stack.push(Frame::Enter {
                        name: dn,
                        path: child_path,
                        b: db,
                    });
                }
            }
            Frame::Exit {
                name,
                path,
                dir_count,
                files,
            } => {
                let split = done.len().saturating_sub(dir_count);
                let mut children: Vec<Node> = done.drain(split..).collect();
                for fnm in files {
                    let fp = if path.is_empty() {
                        fnm.clone()
                    } else {
                        format!("{path}/{fnm}")
                    };
                    children.push(Node {
                        name: fnm,
                        path: fp,
                        dir: false,
                        children: Vec::new(),
                    });
                }
                done.push(Node {
                    name,
                    path,
                    dir: true,
                    children,
                });
            }
        }
    }
    done.pop().unwrap_or(Node {
        name: String::new(),
        path: String::new(),
        dir: true,
        children: Vec::new(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub depth: usize,
    pub name: String,
    pub path: String,
    pub dir: bool,
    pub open: bool,
    pub mark: Option<Mark>,
    pub dirty: bool,
    pub last_sibling: Vec<bool>,
}

pub fn dirty_dirs(marks: &HashMap<String, Mark>) -> HashSet<String> {
    let mut out = HashSet::new();
    for p in marks.keys() {
        let mut cur = p.as_str();
        while let Some(i) = cur.rfind('/') {
            cur = &cur[..i];
            out.insert(cur.to_string());
        }
    }
    out
}

/// Visible rows iteratively computed (no stack overflow on deep trees).
pub fn visible(root: &Node, open: &HashSet<String>, marks: &HashMap<String, Mark>) -> Vec<Row> {
    let dirty = dirty_dirs(marks);
    let mut out = Vec::new();
    let mut stack: Vec<(&Node, usize, Vec<bool>)> = Vec::new();
    for (idx, c) in root.children.iter().enumerate().rev() {
        let is_last = idx + 1 == root.children.len();
        stack.push((c, 0, vec![is_last]));
    }
    while let Some((n, depth, trail)) = stack.pop() {
        let is_open = n.dir && open.contains(&n.path);
        out.push(Row {
            depth,
            name: n.name.clone(),
            path: n.path.clone(),
            dir: n.dir,
            open: is_open,
            mark: marks.get(&n.path).copied(),
            dirty: n.dir && dirty.contains(&n.path),
            last_sibling: trail.clone(),
        });
        if is_open {
            for (idx, c) in n.children.iter().enumerate().rev() {
                let mut child_trail = trail.clone();
                child_trail.push(idx + 1 == n.children.len());
                stack.push((c, depth + 1, child_trail));
            }
        }
    }
    out
}

fn walk_dir(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((d, rel)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP.contains(&name.as_str()) {
                    stack.push((e.path(), path));
                }
            } else {
                out.push(path);
            }
            if out.len() >= MAX_FILES {
                return out;
            }
        }
    }
    out
}

fn mark_letter(m: Mark) -> &'static str {
    match m {
        Mark::Modified => "M",
        Mark::Added => "A",
        Mark::Deleted => "D",
        Mark::Renamed => "R",
        Mark::Conflict => "U",
    }
}

type TreeLoad = (Node, HashMap<String, Mark>, Option<String>);

pub struct TreeView {
    dir: PathBuf,
    title: String,
    branch: Option<String>,
    root: Node,
    marks: HashMap<String, Mark>,
    open: HashSet<String>,
    rows: Vec<Row>,
    selected: usize,
    offset: usize,
    page: usize,
    theme: Theme,
    in_hub: bool,
    focused: bool,
    meta_cache: String,
    changed_only: bool,
    searching: bool,
    search: String,
    matcher: Matcher,
    show_help: bool,
    initial_expand_done: bool,
    toast: Option<Toast>,
    list_area: Rect,
    last_click: Option<(usize, Instant)>,
    bg_rx: Option<Receiver<TreeLoad>>,
    last_bg: Instant,
    hints: [(&'static str, &'static str); 5],
}

impl TreeView {
    pub fn in_hub(mut self, yes: bool) -> TreeView {
        self.in_hub = yes;
        if yes {
            self.hints[4] = ("q", "hide");
        }
        self
    }

    pub fn new(dir: PathBuf, theme: Theme) -> TreeView {
        let title = dir
            .file_name()
            .map(|n| platform::sanitize_display(&n.to_string_lossy()))
            .unwrap_or_else(|| platform::sanitize_display(&dir.to_string_lossy()));
        let mut v = TreeView {
            dir,
            title,
            branch: None,
            root: build_tree(&[]),
            marks: HashMap::new(),
            open: HashSet::new(),
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            page: 10,
            theme,
            in_hub: false,
            focused: true,
            meta_cache: String::new(),
            changed_only: false,
            searching: false,
            search: String::new(),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            show_help: false,
            initial_expand_done: false,
            toast: None,
            list_area: Rect::default(),
            last_click: None,
            bg_rx: None,
            last_bg: Instant::now(),
            hints: HINTS,
        };
        v.reload_sync();
        v
    }

    #[cfg(test)]
    fn with_data(title: &str, paths: &[&str], marks: &[(&str, Mark)]) -> TreeView {
        let paths: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
        let mut v = TreeView {
            dir: PathBuf::from("."),
            title: title.into(),
            branch: Some("main".into()),
            root: build_tree(&paths),
            marks: marks.iter().map(|(p, m)| (p.to_string(), *m)).collect(),
            open: HashSet::new(),
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            page: 10,
            theme: Theme::dark(),
            in_hub: false,
            focused: true,
            meta_cache: String::new(),
            changed_only: false,
            searching: false,
            search: String::new(),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            show_help: false,
            initial_expand_done: true,
            toast: None,
            list_area: Rect::default(),
            last_click: None,
            bg_rx: None,
            last_bg: Instant::now(),
            hints: HINTS,
        };
        v.rebuild_rows(None);
        v
    }

    fn compute_tree_data(dir: &Path) -> TreeLoad {
        let (files_raw, repo) = match git::ls_files(dir) {
            Some(f) => (f, git::status(dir)),
            None => (walk_dir(dir), None),
        };
        let mut seen: HashSet<String> = files_raw.iter().cloned().collect();
        let mut files = files_raw;
        let mut marks = HashMap::new();
        let branch = repo.as_ref().map(|r| r.branch.name.clone());
        if let Some(r) = &repo {
            for c in &r.changes {
                let Some(rel) = c.path.strip_prefix(&r.prefix) else {
                    continue;
                };
                if seen.insert(rel.to_string()) {
                    files.push(rel.to_string());
                }
                marks.insert(rel.to_string(), c.mark());
            }
        }
        (build_tree(&files), marks, branch)
    }

    fn apply_tree_data(&mut self, data: TreeLoad) {
        let keep = self.rows.get(self.selected).map(|r| r.path.clone());
        self.root = data.0;
        self.marks = data.1;
        self.branch = data.2;
        if !self.initial_expand_done {
            self.initial_expand_done = true;
            let mut dirty: Vec<String> = dirty_dirs(&self.marks).into_iter().collect();
            dirty.sort();
            for d in dirty.into_iter().take(6) {
                self.open.insert(d);
            }
        }
        self.rebuild_rows(keep.as_deref());
    }

    fn reload_sync(&mut self) {
        let data = Self::compute_tree_data(&self.dir);
        self.apply_tree_data(data);
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
        let (tx, rx) = mpsc::channel();
        self.bg_rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(Self::compute_tree_data(&dir));
        });
    }

    fn rebuild_rows(&mut self, keep: Option<&str>) {
        if !self.search.trim().is_empty() {
            let pat = Pattern::parse(&self.search, CaseMatching::Smart, Normalization::Smart);
            let mut buf = Vec::new();
            let mut matched_files: Vec<(u32, String)> = Vec::new();
            let mut stack = vec![&self.root];
            while let Some(n) = stack.pop() {
                for c in &n.children {
                    if c.dir {
                        stack.push(c);
                    } else if let Some(sc) =
                        pat.score(Utf32Str::new(&c.path, &mut buf), &mut self.matcher)
                    {
                        matched_files.push((sc, c.path.clone()));
                    }
                }
            }
            matched_files.sort_by_key(|(sc, p)| (std::cmp::Reverse(*sc), p.clone()));
            let keep_set: HashSet<String> = matched_files.iter().map(|(_, p)| p.clone()).collect();
            let mut keep_dirs = HashSet::new();
            for p in &keep_set {
                let mut cur = p.as_str();
                while let Some(i) = cur.rfind('/') {
                    cur = &cur[..i];
                    keep_dirs.insert(cur.to_string());
                }
            }
            self.rows = visible(&self.root, &keep_dirs, &self.marks)
                .into_iter()
                .filter(|r| {
                    if r.dir {
                        keep_dirs.contains(&r.path)
                    } else {
                        keep_set.contains(&r.path)
                    }
                })
                .collect();
        } else if self.changed_only {
            let dirty = dirty_dirs(&self.marks);
            self.rows = visible(&self.root, &dirty, &self.marks)
                .into_iter()
                .filter(|r| {
                    if r.dir {
                        dirty.contains(&r.path)
                    } else {
                        self.marks.contains_key(&r.path)
                    }
                })
                .collect();
        } else {
            self.rows = visible(&self.root, &self.open, &self.marks);
        }

        if let Some(i) = keep.and_then(|p| self.rows.iter().position(|r| r.path == p)) {
            self.selected = i;
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn mv(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected =
            (self.selected as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize;
    }

    fn set_open(&mut self, open: bool) {
        let Some(r) = self.rows.get(self.selected).cloned() else {
            return;
        };
        if !r.dir {
            return;
        }
        if open {
            self.open.insert(r.path.clone());
        } else {
            self.open.remove(&r.path);
        }
        self.rebuild_rows(Some(&r.path));
    }

    fn expand_all(&mut self) {
        let mut stack = vec![&self.root];
        let mut count = 0;
        'walk: while let Some(n) = stack.pop() {
            for c in &n.children {
                if c.dir {
                    self.open.insert(c.path.clone());
                    stack.push(c);
                    count += 1;
                    if count >= 200 {
                        break 'walk;
                    }
                }
            }
        }
        let keep = self.rows.get(self.selected).map(|r| r.path.clone());
        self.rebuild_rows(keep.as_deref());
    }

    fn collapse_all(&mut self) {
        self.open.clear();
        let keep = self.rows.get(self.selected).map(|r| r.path.clone());
        self.rebuild_rows(keep.as_deref());
    }

    fn right(&mut self) {
        match self.rows.get(self.selected) {
            Some(r) if r.dir && !r.open => self.set_open(true),
            Some(r) if r.dir => self.mv(1),
            _ => {}
        }
    }

    fn left(&mut self) {
        let Some(r) = self.rows.get(self.selected).cloned() else {
            return;
        };
        if r.dir && r.open {
            self.set_open(false);
            return;
        }
        if let Some((parent, _)) = r.path.rsplit_once('/') {
            if let Some(i) = self.rows.iter().position(|x| x.path == parent) {
                self.selected = i;
            }
        }
    }

    fn safe_full_path(&self, rel: &str) -> Option<PathBuf> {
        let rel_p = Path::new(rel);
        if !rel_p
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            return None;
        }
        Some(self.dir.join(rel_p))
    }

    fn flash(&mut self, msg: impl Into<String>, kind: ToastKind) {
        self.toast = Some(Toast::new(msg, kind, 1800));
    }

    fn open_file(&mut self) {
        let Some(r) = self.rows.get(self.selected).cloned() else {
            return;
        };
        if r.mark == Some(Mark::Deleted) {
            self.flash("file is deleted", ToastKind::Warn);
            return;
        }
        let Some(full) = self.safe_full_path(&r.path) else {
            self.flash("unsafe path skipped", ToastKind::Error);
            return;
        };
        if platform::open_path(&full) {
            self.flash(format!("opened {}", r.name), ToastKind::Success);
        } else {
            self.flash("no opener found", ToastKind::Warn);
        }
    }
}

impl tui::App for TreeView {
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
        let Ok(data) = rx.try_recv() else {
            return false;
        };
        self.bg_rx = None;
        self.apply_tree_data(data);
        true
    }

    fn mouse(&mut self, m: MouseEvent) -> Flow {
        match m.kind {
            MouseEventKind::ScrollUp => self.mv(-1),
            MouseEventKind::ScrollDown => self.mv(1),
            MouseEventKind::Down(MouseButton::Left) => {
                if self.show_help {
                    self.show_help = false;
                    return Flow::Continue;
                }
                let pos = ratatui::layout::Position {
                    x: m.column,
                    y: m.row,
                };
                if self.list_area.contains(pos) {
                    let idx = self.offset + (m.row - self.list_area.y) as usize;
                    if idx < self.rows.len() {
                        let dbl = self.last_click.is_some_and(|(p, at)| {
                            p == idx && at.elapsed() < Duration::from_millis(400)
                        });
                        self.selected = idx;
                        self.last_click = Some((idx, Instant::now()));
                        let is_dir = self.rows[idx].dir;
                        let is_open = self.rows[idx].open;
                        if is_dir && dbl {
                            self.set_open(!is_open);
                        } else if !is_dir && dbl {
                            self.open_file();
                        }
                    }
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
                    self.rebuild_rows(None);
                }
                KeyCode::Enter => {
                    self.searching = false;
                }
                KeyCode::Backspace => {
                    self.search.pop();
                    self.rebuild_rows(None);
                }
                KeyCode::Char(c)
                    if (!ctrl && !k.modifiers.contains(KeyModifiers::ALT))
                        || (cfg!(windows) && ctrl && k.modifiers.contains(KeyModifiers::ALT)) =>
                {
                    self.search.push(c);
                    self.rebuild_rows(None);
                }
                _ => {}
            }
            return Flow::Continue;
        }

        let page = self.page.max(1) as isize;
        match k.code {
            KeyCode::Esc if !self.search.is_empty() => {
                self.search.clear();
                self.rebuild_rows(None);
            }
            KeyCode::Char('q') | KeyCode::Esc => return Flow::Quit,
            KeyCode::Char('c') if ctrl => return Flow::Quit,
            KeyCode::Char('j') | KeyCode::Down => self.mv(1),
            KeyCode::Char('k') | KeyCode::Up => self.mv(-1),
            KeyCode::PageDown => self.mv(page),
            KeyCode::PageUp => self.mv(-page),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.mv(isize::MAX / 2),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(r) = self.rows.get(self.selected) {
                    if r.dir {
                        let open = r.open;
                        self.set_open(!open);
                    } else if k.code == KeyCode::Enter {
                        self.open_file();
                    }
                }
            }
            KeyCode::Char('l') | KeyCode::Right => self.right(),
            KeyCode::Char('h') | KeyCode::Left => self.left(),
            KeyCode::Char('o') => self.open_file(),
            KeyCode::Char('c') => {
                self.changed_only = !self.changed_only;
                let keep = self.rows.get(self.selected).map(|r| r.path.clone());
                self.rebuild_rows(keep.as_deref());
            }
            KeyCode::Char('/') => {
                self.searching = true;
            }
            KeyCode::Char('E') => self.expand_all(),
            KeyCode::Char('C') => self.collapse_all(),
            KeyCode::Char('y') => {
                if let Some(r) = self.rows.get(self.selected) {
                    if platform::copy_to_clipboard(&r.path) {
                        self.flash(format!("copied {}", r.name), ToastKind::Success);
                    } else {
                        self.flash("clipboard copy unavailable", ToastKind::Warn);
                    }
                }
            }
            KeyCode::Char('i') if self.in_hub => {
                if let Some(r) = self.rows.get(self.selected) {
                    if crate::hub::paste_to_main(&r.path) {
                        self.flash(format!("sent {} to main", r.name), ToastKind::Success);
                    } else {
                        self.flash("could not send to main pane", ToastKind::Warn);
                    }
                }
            }
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

fn draw(f: &mut Frame, v: &mut TreeView) {
    let t = v.theme;
    let area = f.area();
    if v.in_hub {
        let mut meta = v.title.clone();
        if let Some(b) = &v.branch {
            meta = format!("{meta} · {}", platform::sanitize_display(b));
        }
        if !v.marks.is_empty() {
            meta.push_str(&format!(" · {} mod", v.marks.len()));
        }
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
        let mut right = v.title.clone();
        if let Some(b) = &v.branch {
            right = format!("{right} · {}", platform::sanitize_display(b));
        }
        if v.changed_only {
            right.push_str(" [changed]");
        }
        let room = w.saturating_sub(12);
        let right = if room >= 4 {
            Line::from(Span::styled(
                format!(" {} ", cut(&right, room - 2)),
                t.title_muted(),
            ))
            .right_aligned()
        } else {
            Line::default()
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
                    format!(" {} tree ", t.star()),
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
        let line = match &active_toast {
            Some(toast) => toast_line(toast, fw, &t),
            None => hint_line(&v.hints, fw, &t),
        };
        f.render_widget(Paragraph::new(line), foot);
    }

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let mut list_full = inner;
    if v.searching || !v.search.is_empty() {
        let q = cut_tail(&v.search, (inner.width as usize).saturating_sub(5));
        let mut spans = vec![
            Span::raw(" "),
            Span::styled("/", t.title_accent()),
            Span::raw(q),
        ];
        if v.searching {
            spans.push(Span::styled(
                " ",
                Style::default().add_modifier(Modifier::REVERSED),
            ));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect { height: 1, ..inner },
        );
        list_full = Rect {
            y: inner.y + 1,
            height: inner.height - 1,
            ..inner
        };
    }
    v.list_area = list_full;
    if list_full.height == 0 {
        return;
    }
    if v.rows.is_empty() {
        let msg = if v.changed_only {
            "no changed files"
        } else {
            "no files"
        };
        center_msg(f, list_full, msg, &t);
        return;
    }

    let h = list_full.height as usize;
    v.page = h;
    if v.selected < v.offset {
        v.offset = v.selected;
    } else if v.selected >= v.offset + h {
        v.offset = v.selected + 1 - h;
    }
    let need_sb = v.rows.len() > h && list_full.width > 12;
    let draw_area = if need_sb {
        Rect {
            width: list_full.width - 1,
            ..list_full
        }
    } else {
        list_full
    };
    let iw = draw_area.width as usize;
    let guide_style = t.fg(Style::default(), t.border_color());

    let mut lines = Vec::new();
    for (i, r) in v.rows.iter().enumerate().skip(v.offset).take(h) {
        let sel = i == v.selected;
        let mut base = Style::default();
        if sel {
            let bg = if v.focused {
                t.sel_bg()
            } else {
                t.sel_bg_dim()
            };
            if let Some(bg) = bg {
                base = base.bg(bg);
            }
        }
        let max_indent_levels = (iw / 4).min(r.depth);
        let indent_w = max_indent_levels * 2;
        let icon = match (r.dir, r.open) {
            (true, true) => {
                if t.ascii {
                    "- "
                } else {
                    "▾ "
                }
            }
            (true, false) => {
                if t.ascii {
                    "+ "
                } else {
                    "▸ "
                }
            }
            _ => "  ",
        };
        let (tail, tail_style) = match (r.mark, r.dirty) {
            (Some(m), _) => (
                mark_letter(m),
                t.fg(base, mark_color(m, &t)).add_modifier(Modifier::BOLD),
            ),
            (None, true) => (if t.ascii { "*" } else { "•" }, t.fg(base, t.accent())),
            _ => ("", base),
        };
        let tail_space = if tail.is_empty() { 0 } else { sw(tail) + 1 };
        let bar = if sel { t.bar_glyph() } else { " " };
        let bar_style = if v.focused {
            t.fg(base, t.accent())
        } else {
            t.fg(base, t.border_color())
        };
        let prefix_w = 1 + indent_w + 2;
        let name_budget = iw.saturating_sub(prefix_w + tail_space + 1);
        let raw_name = git::printable(&r.name);
        let display = if r.dir {
            format!("{raw_name}/")
        } else {
            raw_name
        };
        let name = cut(&display, name_budget);
        let is_dot = r.name.starts_with('.');
        let name_style = match (sel, r.dir) {
            (true, _) => t.fg(base, t.accent()).add_modifier(Modifier::BOLD),
            (false, true) => base.add_modifier(Modifier::BOLD),
            (false, false) => match r.mark {
                Some(m) => t.fg(base, mark_color(m, &t)),
                None if is_dot => base.patch(t.muted()),
                None => base,
            },
        };
        let used = prefix_w + sw(&name) + tail_space;
        let pad = iw.saturating_sub(used + 1);
        let mut spans = vec![Span::styled(bar, bar_style)];
        for d in 0..max_indent_levels {
            let is_last = r.last_sibling.get(d).copied().unwrap_or(false);
            let sym = if is_last {
                "  "
            } else if t.ascii {
                "| "
            } else {
                "│ "
            };
            spans.push(Span::styled(sym, base.patch(guide_style)));
        }
        spans.push(Span::styled(icon, base.patch(t.muted())));
        spans.push(Span::styled(name.clone(), name_style));
        spans.push(Span::styled(" ".repeat(pad), base));
        if !tail.is_empty() {
            spans.push(Span::styled(" ", base));
            spans.push(Span::styled(tail, tail_style));
        }
        if prefix_w + sw(&name) + pad + tail_space < iw {
            spans.push(Span::styled(" ", base));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), draw_area);
    if need_sb {
        let sb_area = Rect {
            x: list_full.x + list_full.width - 1,
            width: 1,
            ..list_full
        };
        render_scrollbar(f, sb_area, v.rows.len(), v.offset, h, &t);
    }

    if v.show_help {
        let row = |k: &str, d: &str| {
            Line::from(vec![
                Span::styled(format!("{k:<10}"), t.title_accent()),
                Span::raw(d.to_string()),
            ])
        };
        let mut body = vec![
            row("j / k", "Move selection"),
            row("h / l", "Collapse / expand or parent"),
            row("Enter", "Toggle folder or open file"),
            row("o", "Open file or folder"),
            row("/", "Fuzzy filter files"),
            row("c", "Toggle changed files only"),
            row("E / C", "Expand all / collapse all"),
            row("y", "Copy relative path"),
        ];
        if v.in_hub {
            body.push(row("i", "Insert path into main pane"));
        }
        render_modal(
            f,
            area,
            "tree keys",
            body,
            &[("esc", "close"), ("?", "close")],
            &t,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::App;

    fn paths() -> Vec<String> {
        [
            "src/main.rs",
            "src/ui/view.rs",
            "README.md",
            "Cargo.toml",
            "assets/logo.svg",
            "src/a.rs",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn builds_sorted_tree() {
        let root = build_tree(&paths());
        let names: Vec<&str> = root.children.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["assets", "src", "Cargo.toml", "README.md"]);
        let src = &root.children[1];
        assert_eq!(src.path, "src");
        let names: Vec<&str> = src.children.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["ui", "a.rs", "main.rs"]);
        assert_eq!(src.children[0].children[0].path, "src/ui/view.rs");
    }

    #[test]
    fn expansion_and_marks() {
        let root = build_tree(&paths());
        let marks: HashMap<String, Mark> = [("src/ui/view.rs".to_string(), Mark::Modified)].into();
        let rows = visible(&root, &HashSet::new(), &marks);
        assert_eq!(rows.len(), 4);
        assert!(rows[1].dirty && !rows[0].dirty);
        let open: HashSet<String> = ["src".to_string(), "src/ui".to_string()].into();
        let rows = visible(&root, &open, &marks);
        let shown: Vec<(usize, &str)> = rows.iter().map(|r| (r.depth, r.name.as_str())).collect();
        assert_eq!(
            shown[1..5],
            [(0, "src"), (1, "ui"), (2, "view.rs"), (1, "a.rs")]
        );
        assert_eq!(rows[3].mark, Some(Mark::Modified));
        let open: HashSet<String> = ["src/ui".to_string()].into();
        assert_eq!(visible(&root, &open, &marks).len(), 4);
    }

    #[test]
    fn keys_open_close_and_parent() {
        let mut v = TreeView::with_data(
            "grace",
            &["src/main.rs", "src/ui/view.rs", "README.md"],
            &[],
        );
        assert_eq!(v.rows.len(), 2);
        v.key(KeyEvent::from(KeyCode::Char('l')));
        assert_eq!(v.rows.len(), 4);
        v.key(KeyEvent::from(KeyCode::Char('l')));
        assert_eq!(v.rows[v.selected].path, "src/ui");
        v.key(KeyEvent::from(KeyCode::Char('h')));
        assert_eq!(v.rows[v.selected].path, "src");
        v.key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(v.rows.len(), 2);
    }

    #[test]
    fn render_narrow() {
        let mut v = TreeView::with_data(
            "grace",
            &[
                "src/main.rs",
                "src/a-really-long-module-name.rs",
                "README.md",
                "new.txt",
            ],
            &[("src/main.rs", Mark::Modified), ("new.txt", Mark::Added)],
        );
        v.open.insert("src".into());
        v.rebuild_rows(None);
        let out = tui::render_text(25, 8, |f| draw(f, &mut v));
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].starts_with("╭ ✦ tree ") && lines[0].contains("grace"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].starts_with("│▌▾ src/") && lines[1].ends_with("• │"),
            "{}",
            lines[1]
        );
        assert!(
            lines[2].contains("a-really-long-") && lines[2].ends_with("… │"),
            "{}",
            lines[2]
        );
        assert!(
            lines[3].contains("main.rs") && lines[3].ends_with("M │"),
            "{}",
            lines[3]
        );
        assert!(
            lines[4].contains("new.txt") && lines[4].ends_with("A │"),
            "{}",
            lines[4]
        );
        for l in &lines {
            assert_eq!(l.chars().count(), 25, "{l}");
        }
    }

    #[test]
    fn walks_without_git() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("node_modules/x")).unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("node_modules/x/i.js"), "").unwrap();
        std::fs::write(d.path().join("src/m.rs"), "").unwrap();
        assert_eq!(walk_dir(d.path()), vec!["src/m.rs".to_string()]);
    }
}
