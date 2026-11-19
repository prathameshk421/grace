//! `grace tree`: a file tree that marks git changes.
use crate::diffview::mark_color;
use crate::git::{self, Mark};
use crate::picker::{dim, sw, Theme};
use crate::tui::{self, cut, hint_line, Flow};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
    Frame,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const HINTS: [(&str, &str); 4] = [("enter", "open/close"), ("o", "open file"), ("r", "refresh"), ("q", "quit")];
/// Folders the walk skips when the folder is not a git repository.
const SKIP: [&str; 3] = [".git", "node_modules", "target"];
/// Max files the walk collects.
const MAX_FILES: usize = 20_000;

/// One file or folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    /// Path relative to the tree root, with '/' separators.
    pub path: String,
    pub dir: bool,
    pub children: Vec<Node>,
}

#[derive(Default)]
struct Building {
    dirs: BTreeMap<String, Building>,
    files: Vec<String>,
}

/// Build a sorted tree from relative file paths. Folders come first, then files, by name.
pub fn build_tree(paths: &[String]) -> Node {
    let mut root = Building::default();
    for p in paths {
        let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
        let Some((file, dirs)) = parts.split_last() else { continue };
        let mut cur = &mut root;
        for d in dirs {
            cur = cur.dirs.entry(d.to_string()).or_default();
        }
        if !cur.files.iter().any(|f| f == file) && !cur.dirs.contains_key(*file) {
            cur.files.push(file.to_string());
        }
    }
    fn convert(b: Building, name: String, path: String) -> Node {
        let key = |s: &str| (s.to_lowercase(), s.to_string());
        let mut dirs: Vec<(String, Building)> = b.dirs.into_iter().collect();
        dirs.sort_by_key(|(n, _)| key(n));
        let mut files = b.files;
        files.sort_by_key(|n| key(n));
        let join = |n: &str| if path.is_empty() { n.to_string() } else { format!("{path}/{n}") };
        let mut children: Vec<Node> = dirs.into_iter().map(|(n, d)| convert(d, n.clone(), join(&n))).collect();
        children.extend(files.into_iter().map(|n| Node { path: join(&n), name: n, dir: false, children: vec![] }));
        Node { name, path, dir: true, children }
    }
    convert(root, String::new(), String::new())
}

/// One visible line of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub depth: usize,
    pub name: String,
    pub path: String,
    pub dir: bool,
    pub open: bool,
    pub mark: Option<Mark>,
    /// A folder that holds changed files.
    pub dirty: bool,
}

/// All folders that hold a changed file.
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

/// The rows to show: children of open folders, in order.
pub fn visible(root: &Node, open: &HashSet<String>, marks: &HashMap<String, Mark>) -> Vec<Row> {
    let dirty = dirty_dirs(marks);
    let mut out = Vec::new();
    fn walk(n: &Node, depth: usize, open: &HashSet<String>, marks: &HashMap<String, Mark>, dirty: &HashSet<String>, out: &mut Vec<Row>) {
        for c in &n.children {
            let is_open = c.dir && open.contains(&c.path);
            out.push(Row {
                depth,
                name: c.name.clone(),
                path: c.path.clone(),
                dir: c.dir,
                open: is_open,
                mark: marks.get(&c.path).copied(),
                dirty: c.dir && dirty.contains(&c.path),
            });
            if is_open {
                walk(c, depth + 1, open, marks, dirty, out);
            }
        }
    }
    walk(root, 0, open, marks, &dirty, &mut out);
    out
}

/// Files below `dir` without git: skip .git, node_modules and target.
fn walk_dir(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((d, rel)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
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

/// The letter for a change mark.
fn mark_letter(m: Mark) -> &'static str {
    match m {
        Mark::Modified => "M",
        Mark::Added => "A",
        Mark::Deleted => "D",
        Mark::Renamed => "R",
        Mark::Conflict => "U",
    }
}

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
    flash: Option<(String, Instant)>,
}

impl TreeView {
    pub fn new(dir: PathBuf, theme: Theme) -> TreeView {
        let title = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.to_string_lossy().into_owned());
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
            flash: None,
        };
        v.reload();
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
            theme: Theme { color: true, truecolor: true },
            flash: None,
        };
        v.rebuild_rows(None);
        v
    }

    fn reload(&mut self) {
        let keep = self.rows.get(self.selected).map(|r| r.path.clone());
        let (mut files, repo) = match git::ls_files(&self.dir) {
            Some(f) => (f, git::status(&self.dir)),
            None => (walk_dir(&self.dir), None),
        };
        self.marks.clear();
        self.branch = repo.as_ref().map(|r| r.branch.name.clone());
        if let Some(r) = &repo {
            for c in &r.changes {
                let Some(rel) = c.path.strip_prefix(&r.prefix) else { continue };
                // Deleted files are not on disk, but they still show.
                if !files.iter().any(|f| f == rel) {
                    files.push(rel.to_string());
                }
                self.marks.insert(rel.to_string(), c.mark());
            }
        }
        self.root = build_tree(&files);
        self.rebuild_rows(keep.as_deref());
    }

    fn rebuild_rows(&mut self, keep: Option<&str>) {
        self.rows = visible(&self.root, &self.open, &self.marks);
        if let Some(i) = keep.and_then(|p| self.rows.iter().position(|r| r.path == p)) {
            self.selected = i;
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn mv(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize;
    }

    fn set_open(&mut self, open: bool) {
        let Some(r) = self.rows.get(self.selected).cloned() else { return };
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

    fn right(&mut self) {
        match self.rows.get(self.selected) {
            Some(r) if r.dir && !r.open => self.set_open(true),
            Some(r) if r.dir => self.mv(1),
            _ => {}
        }
    }

    fn left(&mut self) {
        let Some(r) = self.rows.get(self.selected).cloned() else { return };
        if r.dir && r.open {
            self.set_open(false);
            return;
        }
        // Go to the parent folder.
        if let Some((parent, _)) = r.path.rsplit_once('/') {
            if let Some(i) = self.rows.iter().position(|x| x.path == parent) {
                self.selected = i;
            }
        }
    }

    fn open_file(&mut self) {
        let Some(r) = self.rows.get(self.selected) else { return };
        let full = self.dir.join(&r.path);
        let msg = if crate::platform::open_path(&full) { format!("opened {}", r.name) } else { "no opener found".into() };
        self.flash = Some((msg, Instant::now() + Duration::from_millis(1500)));
    }
}

impl tui::App for TreeView {
    fn draw(&mut self, f: &mut Frame) {
        draw(f, self);
    }

    fn key(&mut self, k: KeyEvent) -> Flow {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.page.max(1) as isize;
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return Flow::Quit,
            KeyCode::Char('c') if ctrl => return Flow::Quit,
            KeyCode::Char('j') | KeyCode::Down => self.mv(1),
            KeyCode::Char('k') | KeyCode::Up => self.mv(-1),
            KeyCode::PageDown => self.mv(page),
            KeyCode::PageUp => self.mv(-page),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.mv(isize::MAX / 2),
            KeyCode::Enter | KeyCode::Char(' ') => {
                let open = self.rows.get(self.selected).is_some_and(|r| r.open);
                self.set_open(!open);
            }
            KeyCode::Char('l') | KeyCode::Right => self.right(),
            KeyCode::Char('h') | KeyCode::Left => self.left(),
            KeyCode::Char('o') => self.open_file(),
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

fn draw(f: &mut Frame, v: &mut TreeView) {
    let t = v.theme;
    let area = f.area();
    let w = area.width as usize;
    let accent = t.fg(Style::default(), t.accent()).add_modifier(Modifier::BOLD);
    let mut right = v.title.clone();
    if let Some(b) = &v.branch {
        right = format!("{right} · {b}");
    }
    let room = w.saturating_sub(10);
    let right = if room >= 4 { Line::from(Span::styled(format!(" {} ", cut(&right, room - 2)), dim())).right_aligned() } else { Line::default() };
    let flash = v.flash.as_ref().filter(|(_, until)| Instant::now() < *until).map(|(m, _)| m.clone());
    let bottom = match flash {
        Some(m) => Line::from(Span::styled(format!(" {} ", cut(&m, w.saturating_sub(4))), dim())),
        None => hint_line(&HINTS, w.saturating_sub(4)),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(dim())
        .title_top(Line::from(Span::styled(" tree ", accent)).left_aligned())
        .title_top(right)
        .title_bottom(bottom);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if v.rows.is_empty() {
        let y = inner.y + inner.height.saturating_sub(1) / 2;
        let p = Paragraph::new(Span::styled("no files", dim())).alignment(Alignment::Center);
        f.render_widget(p, Rect { y, height: 1, ..inner });
        return;
    }
    let h = inner.height as usize;
    v.page = h;
    if v.selected < v.offset {
        v.offset = v.selected;
    } else if v.selected >= v.offset + h {
        v.offset = v.selected + 1 - h;
    }
    let iw = inner.width as usize;
    let mut lines = Vec::new();
    for (i, r) in v.rows.iter().enumerate().skip(v.offset).take(h) {
        let sel = i == v.selected;
        let mut base = Style::default();
        if sel {
            if let Some(bg) = t.sel_bg() {
                base = base.bg(bg);
            }
        }
        // Indent 2 per level, but keep half the width for the name.
        let indent = (r.depth * 2).min(iw / 2);
        let icon = match (r.dir, r.open) {
            (true, true) => "▾ ",
            (true, false) => "▸ ",
            _ => "  ",
        };
        let (tail, tail_style) = match (r.mark, r.dirty) {
            (Some(m), _) => (mark_letter(m), t.fg(base, mark_color(m)).add_modifier(Modifier::BOLD)),
            (None, true) => ("•", t.fg(base, Color::Yellow)),
            _ => ("", base),
        };
        let bar = if sel { "▌" } else { " " };
        let fixed = 1 + indent + 2 + if tail.is_empty() { 0 } else { 2 };
        let name = cut(&r.name, iw.saturating_sub(fixed));
        let name_style = match (sel, r.dir) {
            (true, _) => t.fg(base, t.accent()).add_modifier(Modifier::BOLD),
            (false, true) => base.add_modifier(Modifier::BOLD),
            _ => r.mark.map_or(base, |m| t.fg(base, mark_color(m))),
        };
        let used = fixed - if tail.is_empty() { 0 } else { 2 } + sw(&name);
        let pad = iw.saturating_sub(used + sw(tail) + 1);
        lines.push(Line::from(vec![
            Span::styled(bar, t.fg(base, t.accent())),
            Span::styled(" ".repeat(indent), base),
            Span::styled(icon, base.add_modifier(Modifier::DIM)),
            Span::styled(name, name_style),
            Span::styled(" ".repeat(pad), base),
            Span::styled(tail, tail_style),
            Span::styled(" ", base),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::App;

    fn paths() -> Vec<String> {
        ["src/main.rs", "src/ui/view.rs", "README.md", "Cargo.toml", "assets/logo.svg", "src/a.rs"].iter().map(|s| s.to_string()).collect()
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
        assert_eq!(shown[1..5], [(0, "src"), (1, "ui"), (2, "view.rs"), (1, "a.rs")]);
        assert_eq!(rows[3].mark, Some(Mark::Modified));
        // A closed parent hides an open child.
        let open: HashSet<String> = ["src/ui".to_string()].into();
        assert_eq!(visible(&root, &open, &marks).len(), 4);
    }

    #[test]
    fn keys_open_close_and_parent() {
        let mut v = TreeView::with_data("grace", &["src/main.rs", "src/ui/view.rs", "README.md"], &[]);
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
            &["src/main.rs", "src/a-really-long-module-name.rs", "README.md", "new.txt"],
            &[("src/main.rs", Mark::Modified), ("new.txt", Mark::Added)],
        );
        v.open.insert("src".into());
        v.rebuild_rows(None);
        let out = tui::render_text(25, 8, |f| draw(f, &mut v));
        println!("{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("╭ tree ") && lines[0].contains("grace"), "{}", lines[0]);
        assert!(lines[1].starts_with("│▌▾ src") && lines[1].ends_with("• │"), "{}", lines[1]);
        assert!(lines[2].contains("a-really-long-mo") && lines[2].ends_with("…│"), "{}", lines[2]);
        assert!(lines[3].contains("main.rs") && lines[3].ends_with("M │"), "{}", lines[3]);
        assert!(lines[4].contains("new.txt") && lines[4].ends_with("A │"), "{}", lines[4]);
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
