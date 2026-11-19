//! The hub sidebar: live agents by status, then folders by frecency. Type to filter.
use crate::model::{Entry, Status};
use crate::picker::{self, best_by_path, dim, status_look, sw, tag_color, Best, Data, Theme};
use crate::tui::{self, cut, hint_line, Flow};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
    Frame,
};
use std::time::{Duration, Instant};

const HINTS: [(&str, &str); 2] = [("enter", "go"), ("ctrl-o", "resume")];
const GROUPS: [Status; 4] = [Status::Waiting, Status::Ready, Status::Working, Status::Done];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Agents(Status),
    Folders,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Header(Section),
    /// Index into the entries.
    Item(usize),
}

/// Agents grouped by status first, then the other folders. Both keep the match order.
pub fn build_rows(entries: &[Entry], best: &Best, query: &str) -> Vec<Row> {
    let matches = picker::filter(entries, best, query);
    let status = |i: usize| best.get(&entries[i].path).map(|s| s.status);
    let mut rows = Vec::new();
    for g in GROUPS {
        let items: Vec<Row> = matches.iter().filter(|&&i| status(i) == Some(g)).map(|&i| Row::Item(i)).collect();
        if !items.is_empty() {
            rows.push(Row::Header(Section::Agents(g)));
            rows.extend(items);
        }
    }
    let folders: Vec<Row> = matches.iter().filter(|&&i| status(i).is_none()).map(|&i| Row::Item(i)).collect();
    if !folders.is_empty() {
        rows.push(Row::Header(Section::Folders));
        rows.extend(folders);
    }
    rows
}

/// Reads the data again.
pub type Reload<'a> = Box<dyn FnMut() -> Option<Data> + 'a>;
/// Switch to a folder. The bool asks to start the last AI tool. Err is a short message.
pub type Go<'a> = Box<dyn FnMut(&Entry, bool) -> Result<(), String> + 'a>;

pub struct Sidebar<'a> {
    data: Data,
    best: Best,
    query: String,
    rows: Vec<Row>,
    selected: usize,
    offset: usize,
    /// The list height of the last draw. A new height scrolls back to the top.
    last_h: usize,
    /// The folder of this workspace.
    current: String,
    home: Option<String>,
    theme: Theme,
    start: Instant,
    flash: Option<(String, Instant)>,
    animating: bool,
    reload: Reload<'a>,
    go: Go<'a>,
}

impl<'a> Sidebar<'a> {
    pub fn new(data: Data, current: &str, theme: Theme, home: Option<String>, reload: Reload<'a>, go: Go<'a>) -> Sidebar<'a> {
        let mut s = Sidebar {
            data: Data::default(),
            best: Best::new(),
            query: String::new(),
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            last_h: 0,
            current: current.to_string(),
            home,
            theme,
            start: Instant::now(),
            flash: None,
            animating: false,
            reload,
            go,
        };
        s.set_data(data);
        // Start on the folder of this workspace.
        let cur = s.current.clone();
        s.refilter(Some(&cur));
        s
    }

    fn set_data(&mut self, data: Data) {
        let keep = self.selected_entry().map(|e| e.path.clone());
        self.best = best_by_path(&data.sessions);
        self.data = data;
        self.refilter(keep.as_deref());
    }

    fn refilter(&mut self, keep: Option<&str>) {
        self.rows = build_rows(&self.data.entries, &self.best, &self.query);
        let entries = &self.data.entries;
        let kept = keep.and_then(|p| self.rows.iter().position(|r| matches!(r, Row::Item(i) if entries[*i].path == p)));
        self.selected = kept.or_else(|| picker_first(&self.rows)).unwrap_or(0);
        if kept.is_none() {
            self.offset = 0;
        }
    }

    fn selected_entry(&self) -> Option<&Entry> {
        match self.rows.get(self.selected) {
            Some(Row::Item(i)) => self.data.entries.get(*i),
            _ => None,
        }
    }

    fn mv(&mut self, delta: isize) {
        let step = delta.signum();
        let mut left = delta.unsigned_abs();
        let mut i = self.selected as isize;
        while left > 0 {
            i += step;
            if i < 0 || i as usize >= self.rows.len() {
                break;
            }
            if matches!(self.rows[i as usize], Row::Item(_)) {
                self.selected = i as usize;
                left -= 1;
            }
        }
    }

    /// Empty the filter, but stay on the selected folder.
    fn clear_query(&mut self) {
        let keep = self.selected_entry().map(|e| e.path.clone());
        self.query.clear();
        self.refilter(keep.as_deref());
    }

    fn flash(&mut self, msg: &str) {
        self.flash = Some((msg.to_string(), Instant::now() + Duration::from_millis(2000)));
    }

    fn choose(&mut self, resume: bool) {
        let Some(e) = self.selected_entry().cloned() else { return };
        if let Err(m) = (self.go)(&e, resume) {
            self.flash(&m);
        } else if !self.query.is_empty() {
            self.query.clear();
            self.refilter(Some(&e.path));
        }
    }
}

fn picker_first(rows: &[Row]) -> Option<usize> {
    rows.iter().position(|r| matches!(r, Row::Item(_)))
}

impl tui::App for Sidebar<'_> {
    fn draw(&mut self, f: &mut Frame) {
        draw(f, self);
    }

    fn key(&mut self, k: KeyEvent) -> Flow {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            // The sidebar never quits: ctrl-g s hides it.
            KeyCode::Esc => self.clear_query(),
            KeyCode::Char('c') | KeyCode::Char('u') if ctrl => self.clear_query(),
            KeyCode::Enter => self.choose(false),
            KeyCode::Char('o') if ctrl => self.choose(true),
            KeyCode::Up => self.mv(-1),
            KeyCode::Down => self.mv(1),
            KeyCode::Char('p') | KeyCode::Char('k') if ctrl => self.mv(-1),
            KeyCode::Char('n') | KeyCode::Char('j') if ctrl => self.mv(1),
            KeyCode::PageUp => self.mv(-10),
            KeyCode::PageDown => self.mv(10),
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter(None);
            }
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                self.query.push(c);
                self.refilter(None);
            }
            _ => {}
        }
        Flow::Continue
    }

    fn refresh(&mut self) {
        if let Some(d) = (self.reload)() {
            self.set_data(d);
        }
    }

    fn every(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn animating(&self) -> bool {
        self.animating
    }
}

fn header_label(s: Section) -> String {
    match s {
        Section::Agents(st) => st.label().to_uppercase(),
        Section::Folders => "FOLDERS".into(),
    }
}

fn draw(f: &mut Frame, sb: &mut Sidebar) {
    let t = sb.theme;
    let area = f.area();
    let w = area.width as usize;
    let accent = t.fg(Style::default(), t.accent());
    let agents = sb.best.values().filter(|s| s.status != Status::Done).count();
    let right = if agents > 0 && w >= 22 {
        Line::from(Span::styled(format!(" {agents} live "), dim())).right_aligned()
    } else {
        Line::default()
    };
    let flash = sb.flash.as_ref().filter(|(_, until)| Instant::now() < *until).map(|(m, _)| m.clone());
    let bottom = match flash {
        Some(m) => Line::from(Span::styled(format!(" {} ", cut(&m, w.saturating_sub(4))), dim())),
        None => hint_line(&HINTS, w.saturating_sub(4)),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(dim())
        .title_top(Line::from(Span::styled(" grace ", accent.add_modifier(Modifier::BOLD))).left_aligned())
        .title_top(right)
        .title_bottom(bottom);
    let inner = block.inner(area);
    f.render_widget(block, area);
    sb.animating = false;
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let iw = inner.width as usize;

    // Filter line.
    let q = cut(&sb.query, iw.saturating_sub(4));
    let mut spans = vec![
        Span::raw(" "),
        Span::styled("❯ ", accent.add_modifier(Modifier::BOLD)),
        Span::raw(q),
        Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
    ];
    if sb.query.is_empty() && iw > 12 {
        spans.push(Span::styled(" filter", dim()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), Rect { height: 1, ..inner });
    let list = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
    if list.height == 0 {
        return;
    }
    if sb.rows.is_empty() {
        let msg = if sb.data.entries.is_empty() { "No folders yet" } else { "No matches" };
        let p = Paragraph::new(Span::styled(cut(msg, iw), dim())).alignment(Alignment::Center);
        f.render_widget(p, Rect { height: 1, ..list });
        return;
    }

    let h = list.height as usize;
    // The pane starts small and grows when tmux resizes it. Start from the top again,
    // so the agents above the selected folder come back into view.
    if h != sb.last_h {
        sb.last_h = h;
        sb.offset = 0;
    }
    sb.offset = picker::scroll(&to_picker_rows(&sb.rows), sb.selected, sb.offset, h);
    let frame = (sb.start.elapsed().as_millis() / 100) as usize;
    let mut lines = Vec::new();
    for (ri, row) in sb.rows.iter().enumerate().skip(sb.offset).take(h) {
        match *row {
            Row::Header(s) => {
                lines.push(Line::from(vec![Span::raw(" "), Span::styled(cut(&header_label(s), iw - 1), dim().add_modifier(Modifier::BOLD))]));
            }
            Row::Item(i) => {
                let e = &sb.data.entries[i];
                let status = sb.best.get(&e.path);
                sb.animating |= status.is_some_and(|s| s.status == Status::Working);
                // An agent row shows its tool first.
                let mut tags: Vec<&str> = Vec::new();
                if let Some(s) = status {
                    tags.push(&s.tool);
                }
                tags.extend(e.tags.iter().map(|t| t.tool.as_str()).filter(|t| Some(*t) != status.map(|s| s.tool.as_str())));
                let name = name_of(&e.path, sb.home.as_deref());
                lines.push(item_line(&t, &name, status.map(|s| s.status), &tags, ri == sb.selected, e.path == sb.current, frame, iw));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), list);
}

fn to_picker_rows(rows: &[Row]) -> Vec<picker::Row> {
    rows.iter()
        .map(|r| match r {
            Row::Header(_) => picker::Row::Header(Status::Done),
            Row::Item(i) => picker::Row::Item(*i),
        })
        .collect()
}

/// The last part of the path, or "~" for home.
fn name_of(path: &str, home: Option<&str>) -> String {
    let shown = picker::display_path(path, home);
    picker::split_path(&shown).1
}

/// One sidebar row: bar, status glyph, name, tags on the right.
#[allow(clippy::too_many_arguments)]
fn item_line(t: &Theme, name: &str, status: Option<Status>, tags: &[&str], sel: bool, current: bool, frame: usize, w: usize) -> Line<'static> {
    let mut base = Style::default();
    if sel {
        if let Some(bg) = t.sel_bg() {
            base = base.bg(bg);
        }
    }
    let mut spans = Vec::new();
    spans.push(Span::styled(if sel { "▌" } else { " " }, t.fg(base, t.accent())));
    match status {
        Some(s) => {
            let (g, _, style) = status_look(s, frame);
            spans.push(Span::styled(g.to_string(), base.patch(t.apply(style))));
        }
        None => spans.push(Span::styled(" ", base)),
    }
    spans.push(Span::styled(" ", base));
    // bar, glyph, space, and one space at the end.
    let avail = w.saturating_sub(4);
    let name_w = sw(name);
    // The name keeps at least 12 columns (or its width) before tags get space.
    let tag_room = avail.saturating_sub(name_w.min(12) + 1);
    let mut tag_spans = Vec::new();
    let mut tags_used = 0;
    for tag in tags {
        let add = sw(tag) + if tags_used > 0 { 1 } else { 0 };
        if tags_used + add > tag_room {
            break;
        }
        if tags_used > 0 {
            tag_spans.push(Span::styled(" ", base));
        }
        tag_spans.push(Span::styled(tag.to_string(), t.fg(base, tag_color(tag))));
        tags_used += add;
    }
    let name_room = if tags_used > 0 { avail - tags_used - 1 } else { avail };
    let shown = cut(name, name_room);
    let name_style = match (sel, current) {
        (true, _) => t.fg(base, t.accent()).add_modifier(Modifier::BOLD),
        (false, true) => t.fg(base, t.accent()),
        _ => base.add_modifier(Modifier::BOLD),
    };
    let pad = avail.saturating_sub(sw(&shown) + tags_used);
    spans.push(Span::styled(shown, name_style));
    spans.push(Span::styled(" ".repeat(pad), base));
    spans.extend(tag_spans);
    spans.push(Span::styled(" ", base));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Session, Tag};
    use crate::tui::App;

    fn entry(path: &str, tools: &[&str]) -> Entry {
        Entry {
            path: path.into(),
            visits: 1,
            last_visit: 0,
            pinned: false,
            tags: tools.iter().map(|t| Tag { tool: t.to_string(), count: 1, last_used: 0 }).collect(),
        }
    }

    fn session(path: &str, tool: &str, status: Status) -> Session {
        Session { id: 1, path: path.into(), tool: tool.into(), status, shell_pid: None, started_at: 0, updated_at: 1 }
    }

    fn data() -> Data {
        Data {
            entries: vec![
                entry("/h/code/knn", &["claude", "vscode"]),
                entry("/h/code/svm", &["codex"]),
                entry("/h/work/api-server-with-a-long-name", &["claude"]),
                entry("/h/notes", &[]),
            ],
            sessions: vec![session("/h/code/svm", "codex", Status::Waiting), session("/h/code/knn", "claude", Status::Working)],
            editors: vec![],
        }
    }

    fn sidebar<'a>(go: Go<'a>) -> Sidebar<'a> {
        Sidebar::new(data(), "/h/notes", Theme { color: true, truecolor: true }, Some("/h".into()), Box::new(|| None), go)
    }

    #[test]
    fn rows_group_and_filter() {
        let d = data();
        let best = best_by_path(&d.sessions);
        let rows = build_rows(&d.entries, &best, "");
        assert_eq!(
            rows,
            vec![
                Row::Header(Section::Agents(Status::Waiting)),
                Row::Item(1),
                Row::Header(Section::Agents(Status::Working)),
                Row::Item(0),
                Row::Header(Section::Folders),
                Row::Item(2),
                Row::Item(3),
            ]
        );
        // Filter by name, by tag and by status word.
        assert_eq!(build_rows(&d.entries, &best, "notes"), vec![Row::Header(Section::Folders), Row::Item(3)]);
        assert_eq!(build_rows(&d.entries, &best, "needs"), vec![Row::Header(Section::Agents(Status::Waiting)), Row::Item(1)]);
        assert!(build_rows(&d.entries, &best, "zzzq").is_empty());
    }

    #[test]
    fn keys_filter_and_go() {
        let picked = std::cell::RefCell::new(Vec::new());
        let mut sb = sidebar(Box::new(|e, resume| {
            picked.borrow_mut().push((e.path.clone(), resume));
            Ok(())
        }));
        // Starts on the folder of this workspace.
        assert_eq!(sb.selected_entry().unwrap().path, "/h/notes");
        sb.mv(-10);
        assert_eq!(sb.selected_entry().unwrap().path, "/h/code/svm");
        sb.key(KeyEvent::from(KeyCode::Char('k')));
        sb.key(KeyEvent::from(KeyCode::Char('n')));
        sb.key(KeyEvent::from(KeyCode::Char('n')));
        assert_eq!(sb.selected_entry().unwrap().path, "/h/code/knn");
        sb.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        sb.key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(sb.rows.len(), 7);
        sb.key(KeyEvent::from(KeyCode::Enter));
        drop(sb);
        assert_eq!(picked.into_inner(), vec![("/h/code/knn".to_string(), true), ("/h/code/knn".to_string(), false)]);
    }

    #[test]
    fn render_narrow() {
        let mut sb = sidebar(Box::new(|_, _| Err("tmux: no server".into())));
        let out = tui::render_text(30, 12, |f| draw(f, &mut sb));
        println!("{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("╭ grace ") && lines[0].contains(" 2 live "), "{}", lines[0]);
        assert!(lines[1].contains("❯") && lines[1].contains("filter"), "{}", lines[1]);
        assert_eq!(lines[2].trim_matches('│').trim(), "NEEDS YOU");
        assert!(lines[3].starts_with("│ ● svm") && lines[3].ends_with("codex │"), "{}", lines[3]);
        assert!(lines[5].contains("⠋ knn") && lines[5].contains("claude"), "{}", lines[5]);
        assert!(lines[7].contains("api-server-with-a-…") || lines[7].contains("api-server"), "{}", lines[7]);
        assert!(lines[7].contains("…"), "{}", lines[7]);
        assert!(lines[8].starts_with("│▌  notes"), "{}", lines[8]);
        assert!(lines[11].starts_with("╰ enter go · ctrl-o resume"), "{}", lines[11]);
        for l in &lines {
            assert_eq!(l.chars().count(), 30, "{l}");
        }
        // An error shows in the bottom border.
        sb.choose(false);
        let out = tui::render_text(30, 12, |f| draw(f, &mut sb));
        assert!(out.lines().last().unwrap().contains("tmux: no server"), "{out}");
        // Very narrow still draws.
        let out = tui::render_text(14, 6, |f| draw(f, &mut sb));
        assert!(out.lines().all(|l| l.chars().count() == 14), "{out}");
    }

    #[test]
    fn agents_show_after_the_pane_grows() {
        let mut sb = sidebar(Box::new(|_, _| Ok(())));
        // A tiny pane scrolls down to the selected folder, so the agents go out of view.
        let small = tui::render_text(30, 5, |f| draw(f, &mut sb));
        assert!(!small.contains("NEEDS YOU"), "{small}");
        // When tmux makes the pane taller, the agents come back.
        let tall = tui::render_text(30, 14, |f| draw(f, &mut sb));
        assert!(tall.contains("NEEDS YOU") && tall.contains("notes"), "{tall}");
    }
}
