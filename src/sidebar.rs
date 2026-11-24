//! The hub sidebar: live agents by status, then folders by frecency. Type to filter.
use crate::model::{Entry, Status};
use crate::picker::{self, best_by_path, split_path, Best, Data};
use crate::platform;
use crate::theme::{Theme, ToastKind};
use crate::tui::{self, Flow};
use crate::widgets::{
    center_msg, cut, cut_tail, fit_end, hint_line, is_plain_char, push_fit_coalesced,
    render_scrollbar, slice_scrollbar, spaces, sw, toast_line, DoubleClick, Toast,
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
    widgets::Paragraph,
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

pub type Reload<'a> = Box<dyn FnMut() -> Option<(Data, Vec<String>)> + 'a>;
pub type Go<'a> = Box<dyn FnMut(&Entry, bool) -> Result<(), String> + 'a>;

pub struct Sidebar<'a> {
    data: Data,
    hub_paths: Vec<String>,
    best: Best,
    query: String,
    pattern: Option<Pattern>,
    matcher: Matcher,
    utf32_buf: Vec<char>,
    idx_buf: Vec<u32>,
    hl_buf: Vec<usize>,
    tags_buf: Vec<String>,
    rows: Vec<Row>,
    selected: usize,
    offset: usize,
    last_h: usize,
    current: String,
    home: Option<String>,
    theme: Theme,
    focused: bool,
    meta_cache: String,
    start: Instant,
    toast: Option<Toast>,
    animating: bool,
    list_area: Rect,
    last_click: DoubleClick,
    reload: Reload<'a>,
    go: Go<'a>,
}

impl<'a> Sidebar<'a> {
    pub fn new(
        data: Data,
        hub_paths: Vec<String>,
        current: &str,
        theme: Theme,
        home: Option<String>,
        reload: Reload<'a>,
        go: Go<'a>,
    ) -> Sidebar<'a> {
        let mut s = Sidebar {
            data: Data::default(),
            hub_paths: Vec::new(),
            best: Best::new(),
            query: String::new(),
            pattern: None,
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            utf32_buf: Vec::new(),
            idx_buf: Vec::new(),
            hl_buf: Vec::new(),
            tags_buf: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            last_h: 0,
            current: current.to_string(),
            home,
            theme,
            focused: true,
            meta_cache: String::new(),
            start: Instant::now(),
            toast: None,
            animating: false,
            list_area: Rect::default(),
            last_click: DoubleClick::default(),
            reload,
            go,
        };
        s.set_data(data, hub_paths);
        let cur = s.current.clone();
        s.refilter(Some(&cur));
        s
    }

    fn set_data(&mut self, data: Data, hub_paths: Vec<String>) {
        let keep = self.selected_entry().map(|e| e.path.clone());
        self.best = best_by_path(&data.sessions);
        self.data = data;
        self.hub_paths = hub_paths;
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
                        let dbl = self.last_click.click(idx);
                        self.selected = idx;
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

    fn refresh(&mut self) -> bool {
        let expired = self
            .toast
            .as_ref()
            .is_some_and(|x| Instant::now() >= x.until);
        if expired {
            self.toast = None;
        }
        if let Some((d, hp)) = (self.reload)() {
            let changed = d.entries != self.data.entries
                || d.sessions != self.data.sessions
                || hp != self.hub_paths;
            if changed {
                self.set_data(d, hp);
                return true;
            }
        }
        expired
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
    crate::hub::set_pane_meta(&mut sb.meta_cache, &meta);

    let active_toast = sb
        .toast
        .as_ref()
        .filter(|x| Instant::now() < x.until)
        .cloned();
    let (inner, footer) = if area.height <= 1 {
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
    };

    if let Some(foot) = footer {
        let fw = foot.width as usize;
        let line = match &active_toast {
            Some(toast) => toast_line(toast, fw, t),
            None => hint_line(&HINTS, fw, t),
        };
        f.render_widget(Paragraph::new(line), foot);
    }

    sb.animating = false;
    sb.list_area = Rect::default();
    if inner.width == 0 || inner.height == 0 {
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
        return;
    }
    if sb.rows.is_empty() {
        let msg = if sb.data.entries.is_empty() {
            "No folders yet"
        } else {
            "No matches"
        };
        center_msg(f, list_full, msg, t);
        return;
    }

    let h = list_full.height as usize;
    if h != sb.last_h {
        sb.last_h = h;
        sb.offset = 0;
    }
    sb.offset = picker::scroll_by(
        sb.rows.len(),
        |idx| matches!(sb.rows[idx], Row::Header(_)),
        sb.selected,
        sb.offset,
        h,
    );
    let (list, sb_area) = slice_scrollbar(list_full, sb.rows.len(), h);
    sb.list_area = list;
    let lw = list.width as usize;
    let frame = (sb.start.elapsed().as_millis() / 100) as usize;
    let mut lines = Vec::new();
    for ri in sb.offset..sb.rows.len().min(sb.offset + h) {
        let row = sb.rows[ri];
        match row {
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
                sb.tags_buf.clear();
                if let Some(s) = status {
                    sb.tags_buf
                        .push(platform::sanitize_display(&s.tool).into_owned());
                }
                for tag in &e.tags {
                    if Some(tag.tool.as_str()) != status.map(|s| s.tool.as_str()) {
                        sb.tags_buf
                            .push(platform::sanitize_display(&tag.tool).into_owned());
                    }
                }
                let raw_name = name_of(&e.path, sb.home.as_deref());
                let name = platform::sanitize_display(&raw_name);
                sb.hl_buf.clear();
                if let Some(p) = &sb.pattern {
                    sb.idx_buf.clear();
                    p.indices(
                        Utf32Str::new(&name, &mut sb.utf32_buf),
                        &mut sb.matcher,
                        &mut sb.idx_buf,
                    );
                    sb.hl_buf.extend(sb.idx_buf.iter().map(|&x| x as usize));
                }
                let current = platform::same_path(&e.path, &sb.current);
                let open_ws =
                    !current && sb.hub_paths.iter().any(|p| platform::same_path(p, &e.path));
                lines.push(item_line(
                    t,
                    &name,
                    &sb.hl_buf,
                    status.map(|s| s.status),
                    e.pinned,
                    current,
                    open_ws,
                    &sb.tags_buf,
                    ri == sb.selected,
                    sb.focused,
                    frame,
                    lw,
                ));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), list);
    if let Some(sb_area) = sb_area {
        render_scrollbar(f, sb_area, sb.rows.len(), sb.offset, h, t);
    }
}

fn name_of(path: &str, home: Option<&str>) -> String {
    let shown = platform::display_path(path, home);
    split_path(&shown).1.to_string()
}

#[allow(clippy::too_many_arguments)]
fn item_line(
    t: Theme,
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
    spans.push(Span::styled(
        if sel { t.bar_glyph() } else { " " },
        bar_style,
    ));
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
    spans.push(Span::styled(spaces(pad), base));
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
        }
    }

    fn sidebar<'a>(go: Go<'a>) -> Sidebar<'a> {
        Sidebar::new(
            data(),
            Vec::new(),
            "/h/notes",
            Theme::dark(),
            Some("/h".into()),
            Box::new(|| None),
            go,
        )
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
    fn render_narrow_frameless_hub() {
        let mut sb = sidebar(Box::new(|_, _| Err("tmux: no server".into())));
        let out = tui::render_text(30, 12, |f| draw(f, &mut sb));
        let lines: Vec<&str> = out.lines().collect();
        assert!(!lines[0].contains('╭'), "{}", lines[0]);
        assert!(
            lines[0].contains("❯") && lines[0].contains("filter"),
            "{}",
            lines[0]
        );
        assert_eq!(lines[1].trim(), "NEEDS YOU");
        assert!(
            lines[2].starts_with(" ● svm") && lines[2].ends_with("codex"),
            "{}",
            lines[2]
        );
        assert!(lines[7].starts_with("▌◆ notes"), "{}", lines[7]);
        assert!(lines[11].starts_with(" ⏎ go"), "{}", lines[11]);
        for l in &lines {
            assert!(l.chars().count() <= 30, "{l}");
        }
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
