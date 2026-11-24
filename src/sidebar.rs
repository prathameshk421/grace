//! The hub sidebar: live agents by status, then folders by frecency. Type to filter.
use crate::model::{Entry, Status};
use crate::picker::{self, best_by_path, split_path, Best, Data};
use crate::platform;
use crate::theme::{Theme, ToastKind};
use crate::tui::{self, Flow};
use crate::widgets::{
    center_msg, cut, cut_tail, fit_end, hint_line, push_fit_coalesced, render_scrollbar, sw,
    toast_line, Toast,
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
use std::time::{Duration, Instant};

const HINTS: [(&str, &str); 2] = [("⏎", "go"), ("^o", "resume")];
const GROUPS: [Status; 4] = [
    Status::Waiting,
    Status::Ready,
    Status::Working,
    Status::Done,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Agents(Status),
    Folders,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Header(Section),
    Item(usize),
}

pub fn build_rows(entries: &[Entry], best: &Best, query: &str) -> Vec<Row> {
    let matches = picker::filter(entries, best, query);
    let status = |i: usize| best.get(&entries[i].path).map(|s| s.status);
    let mut rows = Vec::new();
    for g in GROUPS {
        let items: Vec<Row> = matches
            .iter()
            .filter(|&&i| status(i) == Some(g))
            .map(|&i| Row::Item(i))
            .collect();
        if !items.is_empty() {
            rows.push(Row::Header(Section::Agents(g)));
            rows.extend(items);
        }
    }
    let folders: Vec<Row> = matches
        .iter()
        .filter(|&&i| status(i).is_none())
        .map(|&i| Row::Item(i))
        .collect();
    if !folders.is_empty() {
        rows.push(Row::Header(Section::Folders));
        rows.extend(folders);
    }
    rows
}

pub type Reload<'a> = Box<dyn FnMut() -> Option<Data> + 'a>;
pub type Go<'a> = Box<dyn FnMut(&Entry, bool) -> Result<(), String> + 'a>;

pub struct Sidebar<'a> {
    data: Data,
    best: Best,
    query: String,
    pattern: Option<Pattern>,
    matcher: Matcher,
    rows: Vec<Row>,
    selected: usize,
    offset: usize,
    last_h: usize,
    current: String,
    home: Option<String>,
    theme: Theme,
    in_hub: bool,
    focused: bool,
    meta_cache: String,
    start: Instant,
    toast: Option<Toast>,
    animating: bool,
    list_area: Rect,
    last_click: Option<(usize, Instant)>,
    reload: Reload<'a>,
    go: Go<'a>,
}

impl<'a> Sidebar<'a> {
    pub fn new(
        data: Data,
        current: &str,
        theme: Theme,
        home: Option<String>,
        reload: Reload<'a>,
        go: Go<'a>,
    ) -> Sidebar<'a> {
        let mut s = Sidebar {
            data: Data::default(),
            best: Best::new(),
            query: String::new(),
            pattern: None,
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            last_h: 0,
            current: current.to_string(),
            home,
            theme,
            in_hub: true,
            focused: true,
            meta_cache: String::new(),
            start: Instant::now(),
            toast: None,
            animating: false,
            list_area: Rect::default(),
            last_click: None,
            reload,
            go,
        };
        s.set_data(data);
        let cur = s.current.clone();
        s.refilter(Some(&cur));
        s
    }

    #[cfg(test)]
    pub fn in_hub(mut self, yes: bool) -> Sidebar<'a> {
        self.in_hub = yes;
        self
    }

    fn set_data(&mut self, data: Data) {
        let keep = self.selected_entry().map(|e| e.path.clone());
        self.best = best_by_path(&data.sessions);
        self.data = data;
        self.refilter(keep.as_deref());
    }

    fn refilter(&mut self, keep: Option<&str>) {
        self.pattern = (!self.query.trim().is_empty())
            .then(|| Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart));
        self.rows = build_rows(&self.data.entries, &self.best, &self.query);
        let entries = &self.data.entries;
        let kept = keep.and_then(|p| {
            self.rows
                .iter()
                .position(|r| matches!(r, Row::Item(i) if entries[*i].path == p))
        });
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

    fn clear_query(&mut self) {
        let keep = self.selected_entry().map(|e| e.path.clone());
        self.query.clear();
        self.refilter(keep.as_deref());
    }

    fn flash(&mut self, msg: &str, kind: ToastKind) {
        self.toast = Some(Toast::new(msg, kind, 2200));
    }

    fn choose(&mut self, resume: bool) {
        let Some(e) = self.selected_entry().cloned() else {
            return;
        };
        if let Err(m) = (self.go)(&e, resume) {
            self.flash(&m, ToastKind::Error);
        } else if !self.query.is_empty() {
            self.query.clear();
            self.refilter(Some(&e.path));
        }
    }
}

fn picker_first(rows: &[Row]) -> Option<usize> {
    rows.iter().position(|r| matches!(r, Row::Item(_)))
}

fn is_plain_char(k: &KeyEvent) -> bool {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    (!ctrl && !alt) || (cfg!(windows) && ctrl && alt)
}

impl tui::App for Sidebar<'_> {
    fn draw(&mut self, f: &mut Frame) {
        draw(f, self);
    }

    fn focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn mouse(&mut self, m: MouseEvent) -> Flow {
        match m.kind {
            MouseEventKind::ScrollUp => self.mv(-1),
            MouseEventKind::ScrollDown => self.mv(1),
            MouseEventKind::Down(MouseButton::Left) => {
                let pos = ratatui::layout::Position {
                    x: m.column,
                    y: m.row,
                };
                if self.list_area.contains(pos) {
                    let idx = self.offset + (m.row - self.list_area.y) as usize;
                    if matches!(self.rows.get(idx), Some(Row::Item(_))) {
                        let dbl = self.last_click.is_some_and(|(p, at)| {
                            p == idx && at.elapsed() < Duration::from_millis(400)
                        });
                        self.selected = idx;
                        self.last_click = Some((idx, Instant::now()));
                        if dbl {
                            self.choose(false);
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
        match k.code {
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
            KeyCode::Char(c) if is_plain_char(&k) => {
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
    let agents = sb
        .best
        .values()
        .filter(|s| s.status != Status::Done)
        .count();
    let meta = if agents > 0 {
        format!("{agents} live")
    } else {
        String::new()
    };
    if sb.in_hub {
        crate::hub::set_pane_meta(&mut sb.meta_cache, &meta);
    }

    let active_toast = sb
        .toast
        .as_ref()
        .filter(|x| Instant::now() < x.until)
        .cloned();
    let (inner, footer) = if sb.in_hub {
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
        let right = if agents > 0 && w >= 22 {
            Line::from(Span::styled(format!(" {agents} live "), t.title_muted())).right_aligned()
        } else {
            Line::default()
        };
        let bottom = match &active_toast {
            Some(toast) => toast_line(toast, w.saturating_sub(4), &t),
            None => hint_line(&HINTS, w.saturating_sub(4), &t),
        };
        let block = Block::bordered()
            .border_type(t.border_type())
            .border_style(t.border_style(sb.focused))
            .title_top(
                Line::from(Span::styled(
                    format!(" {} grace ", t.star()),
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
            None => hint_line(&HINTS, fw, &t),
        };
        f.render_widget(Paragraph::new(line), foot);
    }

    sb.animating = false;
    if inner.width == 0 || inner.height == 0 {
        sb.list_area = Rect::default();
        return;
    }
    let iw = inner.width as usize;

    // Filter line with tail-scrolling query and focus-aware cursor.
    let q = cut_tail(&sb.query, iw.saturating_sub(5));
    let mut spans = vec![
        Span::raw(" "),
        Span::styled("❯ ", t.title_accent()),
        Span::raw(q),
    ];
    if sb.focused {
        spans.push(Span::styled(
            " ",
            Style::default().add_modifier(Modifier::REVERSED),
        ));
    }
    if sb.query.is_empty() && iw > 12 {
        spans.push(Span::styled(" filter", t.muted()));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { height: 1, ..inner },
    );

    let list_full = Rect {
        y: inner.y + 1,
        height: inner.height - 1,
        ..inner
    };
    if list_full.height == 0 {
        sb.list_area = Rect::default();
        return;
    }
    if sb.rows.is_empty() {
        sb.list_area = Rect::default();
        let msg = if sb.data.entries.is_empty() {
            "No folders yet"
        } else {
            "No matches"
        };
        center_msg(f, list_full, msg, &t);
        return;
    }

    let h = list_full.height as usize;
    if h != sb.last_h {
        sb.last_h = h;
        sb.offset = 0;
    }
    sb.offset = picker::scroll(&to_picker_rows(&sb.rows), sb.selected, sb.offset, h);
    let need_sb = sb.rows.len() > h && list_full.width > 12;
    let list = if need_sb {
        Rect {
            width: list_full.width - 1,
            ..list_full
        }
    } else {
        list_full
    };
    sb.list_area = list;
    let lw = list.width as usize;
    let frame = (sb.start.elapsed().as_millis() / 100) as usize;
    let mut lines = Vec::new();
    for (ri, row) in sb.rows.iter().enumerate().skip(sb.offset).take(h) {
        match *row {
            Row::Header(s) => {
                lines.push(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        cut(&header_label(s), lw.saturating_sub(1)),
                        t.muted().add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            Row::Item(i) => {
                let e = &sb.data.entries[i];
                let status = sb.best.get(&e.path);
                sb.animating |= status.is_some_and(|s| s.status == Status::Working);
                let mut tags: Vec<String> = Vec::new();
                if let Some(s) = status {
                    tags.push(platform::sanitize_display(&s.tool));
                }
                for tag in &e.tags {
                    if Some(tag.tool.as_str()) != status.map(|s| s.tool.as_str()) {
                        tags.push(platform::sanitize_display(&tag.tool));
                    }
                }
                let name = platform::sanitize_display(&name_of(&e.path, sb.home.as_deref()));
                let hl = sb
                    .pattern
                    .as_ref()
                    .map(|p| {
                        let mut buf = Vec::new();
                        let mut idx = Vec::new();
                        p.indices(Utf32Str::new(&name, &mut buf), &mut sb.matcher, &mut idx);
                        idx.into_iter().map(|x| x as usize).collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let current = platform::same_path(&e.path, &sb.current);
                let open_ws = !current
                    && sb
                        .data
                        .hub_paths
                        .iter()
                        .any(|p| platform::same_path(p, &e.path));
                lines.push(item_line(
                    &t,
                    &name,
                    &hl,
                    status.map(|s| s.status),
                    e.pinned,
                    current,
                    open_ws,
                    &tags,
                    ri == sb.selected,
                    sb.focused,
                    frame,
                    lw,
                ));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), list);
    if need_sb {
        let sb_area = Rect {
            x: list_full.x + list_full.width - 1,
            width: 1,
            ..list_full
        };
        render_scrollbar(f, sb_area, sb.rows.len(), sb.offset, h, &t);
    }
}

fn to_picker_rows(rows: &[Row]) -> Vec<picker::Row> {
    rows.iter()
        .map(|r| match r {
            Row::Header(_) => picker::Row::Header(Status::Done, 0),
            Row::Item(i) => picker::Row::Item(*i),
        })
        .collect()
}

fn name_of(path: &str, home: Option<&str>) -> String {
    let shown = picker::display_path(path, home);
    split_path(&shown).1
}

#[allow(clippy::too_many_arguments)]
fn item_line(
    t: &Theme,
    name: &str,
    hl: &[usize],
    status: Option<Status>,
    pinned: bool,
    current: bool,
    open_ws: bool,
    tags: &[String],
    sel: bool,
    focused: bool,
    frame: usize,
    w: usize,
) -> Line<'static> {
    let mut base = Style::default();
    if sel {
        let bg = if focused { t.sel_bg() } else { t.sel_bg_dim() };
        if let Some(bg) = bg {
            base = base.bg(bg);
        }
    }
    let mut spans = Vec::new();
    let bar_style = if focused {
        t.fg(base, t.accent())
    } else {
        t.fg(base, t.border_color())
    };
    spans.push(Span::styled(if sel { "▌" } else { " " }, bar_style));
    match status {
        Some(s) => {
            let (g, _, style) = t.status_look(s, frame);
            spans.push(Span::styled(g.to_string(), base.patch(t.apply(style))));
        }
        None if pinned => spans.push(Span::styled(
            t.pin_glyph().to_string(),
            t.fg(base, t.accent()),
        )),
        None if current => spans.push(Span::styled(
            t.active_ws_glyph().to_string(),
            t.fg(base, t.accent()),
        )),
        None if open_ws => spans.push(Span::styled(
            t.open_ws_glyph().to_string(),
            base.patch(t.muted()),
        )),
        None => spans.push(Span::styled(" ", base)),
    }
    spans.push(Span::styled(" ", base));

    let avail = w.saturating_sub(4);
    let name_w = sw(name);
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
        tag_spans.push(Span::styled(tag.clone(), t.fg(base, t.tag_color(tag))));
        tags_used += add;
    }
    let name_room = if tags_used > 0 {
        avail.saturating_sub(tags_used + 1)
    } else {
        avail
    };
    let fit = fit_end(name, name_room);
    let name_style = match (sel, current) {
        (true, _) => t.fg(base, t.accent()).add_modifier(Modifier::BOLD),
        (false, true) => t.fg(base, t.accent()),
        _ => base.add_modifier(Modifier::BOLD),
    };
    let hl_style = t
        .fg(base, if sel { t.accent_hi() } else { t.accent() })
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let mut name_used = 0;
    push_fit_coalesced(&mut spans, &mut name_used, &fit, hl, name_style, hl_style);
    let pad = avail.saturating_sub(name_used + tags_used);
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
    use std::collections::HashMap;

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

    fn session(path: &str, tool: &str, status: Status) -> Session {
        Session {
            id: 1,
            path: path.into(),
            tool: tool.into(),
            status,
            shell_pid: None,
            started_at: 0,
            updated_at: 1,
            session_id: None,
            detail: None,
            tmux_pane: None,
        }
    }

    fn data() -> Data {
        Data {
            entries: vec![
                entry("/h/code/knn", &["claude", "vscode"]),
                entry("/h/code/svm", &["codex"]),
                entry("/h/work/api-server-with-a-long-name", &["claude"]),
                entry("/h/notes", &[]),
            ],
            sessions: vec![
                session("/h/code/svm", "codex", Status::Waiting),
                session("/h/code/knn", "claude", Status::Working),
            ],
            editors: vec![],
            branches: HashMap::new(),
            hub_paths: Vec::new(),
        }
    }

    fn sidebar<'a>(go: Go<'a>) -> Sidebar<'a> {
        Sidebar::new(
            data(),
            "/h/notes",
            Theme::dark(),
            Some("/h".into()),
            Box::new(|| None),
            go,
        )
        .in_hub(false)
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
        assert_eq!(
            build_rows(&d.entries, &best, "notes"),
            vec![Row::Header(Section::Folders), Row::Item(3)]
        );
        assert_eq!(
            build_rows(&d.entries, &best, "needs"),
            vec![Row::Header(Section::Agents(Status::Waiting)), Row::Item(1)]
        );
        assert!(build_rows(&d.entries, &best, "zzzq").is_empty());
    }

    #[test]
    fn keys_filter_and_go() {
        let picked = std::cell::RefCell::new(Vec::new());
        let mut sb = sidebar(Box::new(|e, resume| {
            picked.borrow_mut().push((e.path.clone(), resume));
            Ok(())
        }));
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
        assert_eq!(
            picked.into_inner(),
            vec![
                ("/h/code/knn".to_string(), true),
                ("/h/code/knn".to_string(), false)
            ]
        );
    }

    #[test]
    fn render_narrow_and_frameless_hub() {
        let mut sb = sidebar(Box::new(|_, _| Err("tmux: no server".into())));
        let out = tui::render_text(30, 12, |f| draw(f, &mut sb));
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].starts_with("╭ ✦ grace ") && lines[0].contains(" 2 live "),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].contains("❯") && lines[1].contains("filter"),
            "{}",
            lines[1]
        );
        assert_eq!(lines[2].trim_matches('│').trim(), "NEEDS YOU");
        assert!(
            lines[3].starts_with("│ ● svm") && lines[3].ends_with("codex │"),
            "{}",
            lines[3]
        );
        assert!(lines[8].starts_with("│▌◆ notes"), "{}", lines[8]);
        assert!(lines[11].starts_with("╰ ⏎ go · ^o resume"), "{}", lines[11]);
        for l in &lines {
            assert_eq!(l.chars().count(), 30, "{l}");
        }
        // Frameless hub mode omits the outer Box border.
        let mut sb_hub = sb.in_hub(true);
        let hub_out = tui::render_text(28, 10, |f| draw(f, &mut sb_hub));
        assert!(!hub_out.lines().next().unwrap().contains('╭'));
        assert!(hub_out.lines().next().unwrap().contains("❯"));
    }

    #[test]
    fn agents_show_after_the_pane_grows() {
        let mut sb = sidebar(Box::new(|_, _| Ok(())));
        let small = tui::render_text(30, 5, |f| draw(f, &mut sb));
        assert!(!small.contains("NEEDS YOU"), "{small}");
        let tall = tui::render_text(30, 14, |f| draw(f, &mut sb));
        assert!(
            tall.contains("NEEDS YOU") && tall.contains("notes"),
            "{tall}"
        );
    }
}
