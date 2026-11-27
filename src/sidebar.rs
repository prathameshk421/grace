//! The hub sidebar: live agents by status, then folders by frecency. Type to filter.
use crate::editors_live::OpenEditor;
use crate::launcher::{InstalledTarget, LaunchChoice, LaunchMenu, LaunchRow, TargetKind};
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
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const HINTS: [(&str, &str); 3] = [("⏎", "go"), ("^o", "run"), ("→", "open")];
const LAUNCH_HINTS: [(&str, &str); 3] = [("1-9", "go"), ("←", "back"), ("⏎", "open")];
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

#[cfg(test)]
pub fn build_rows(entries: &[Entry], best: &Best, query: &str) -> Vec<Row> {
    build_rows_top(entries, best, query).0
}

/// Rows grouped by status, plus the entry index of the best match.
/// Groups put agents first, so the first row is not always the best match.
pub fn build_rows_top(entries: &[Entry], best: &Best, query: &str) -> (Vec<Row>, Option<usize>) {
    let matches = picker::filter(entries, best, query);
    let top = matches.first().copied();
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
    (rows, top)
}

pub struct ReloadTick {
    pub visible: bool,
    pub data: Option<(Data, Vec<String>)>,
}

pub type Reload<'a> = Box<dyn FnMut(bool) -> ReloadTick + 'a>;
pub type Go<'a> = Box<dyn FnMut(&Entry, bool) -> Result<(), String> + 'a>;
pub type DiscoverFn<'a> = Box<dyn FnMut() -> (Vec<InstalledTarget>, Vec<OpenEditor>) + 'a>;
pub type LaunchFn<'a> =
    Box<dyn FnMut(&Entry, &LaunchChoice, &[OpenEditor]) -> Result<String, String> + 'a>;

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
    visible: bool,
    force_refresh: bool,
    meta_cache: String,
    start: Instant,
    toast: Option<Toast>,
    animating: bool,
    list_area: Rect,
    launch_header_area: Rect,
    launch_list_area: Rect,
    last_click: DoubleClick,
    reload: Reload<'a>,
    go: Go<'a>,
    installed: Option<Vec<InstalledTarget>>,
    open_editors: Vec<OpenEditor>,
    global_counts: HashMap<String, i64>,
    launch_menu: Option<(Entry, LaunchMenu)>,
    bg_rx: Option<Receiver<(Vec<InstalledTarget>, Vec<OpenEditor>)>>,
    discover: Option<DiscoverFn<'a>>,
    launch: Option<LaunchFn<'a>>,
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
            visible: true,
            force_refresh: false,
            meta_cache: String::new(),
            start: Instant::now(),
            toast: None,
            animating: false,
            list_area: Rect::default(),
            launch_header_area: Rect::default(),
            launch_list_area: Rect::default(),
            last_click: DoubleClick::default(),
            reload,
            go,
            installed: None,
            open_editors: Vec::new(),
            global_counts: HashMap::new(),
            launch_menu: None,
            bg_rx: None,
            discover: None,
            launch: None,
        };
        s.set_data(data, hub_paths);
        let cur = s.current.clone();
        s.refilter(Some(&cur));
        s
    }

    pub fn with_launcher(
        mut self,
        global_counts: HashMap<String, i64>,
        bg_rx: Option<Receiver<(Vec<InstalledTarget>, Vec<OpenEditor>)>>,
        discover: Option<DiscoverFn<'a>>,
        launch: Option<LaunchFn<'a>>,
    ) -> Self {
        if !global_counts.is_empty() {
            self.global_counts = global_counts;
        }
        self.bg_rx = bg_rx;
        self.discover = discover;
        self.launch = launch;
        self
    }

    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    fn set_data(&mut self, data: Data, hub_paths: Vec<String>) {
        let keep = self.selected_entry().map(|e| e.path.clone());
        self.best = best_by_path(&data.sessions);
        self.global_counts.clear();
        for e in &data.entries {
            for t in &e.tags {
                let c = self.global_counts.entry(t.tool.clone()).or_insert(0);
                *c = c.saturating_add(t.count.max(0));
            }
        }
        self.data = data;
        self.hub_paths = hub_paths;
        self.refilter(keep.as_deref());
    }

    fn refilter(&mut self, keep: Option<&str>) {
        self.pattern = (!self.query.trim().is_empty())
            .then(|| Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart));
        let (rows, top) = build_rows_top(&self.data.entries, &self.best, &self.query);
        self.rows = rows;
        let entries = &self.data.entries;
        let kept = keep.and_then(|p| {
            self.rows
                .iter()
                .position(|r| matches!(r, Row::Item(i) if entries[*i].path == p))
        });
        // With a query, select the best match, not the first row of the agent groups.
        let top_row = top.filter(|_| !self.query.trim().is_empty()).and_then(|t| {
            self.rows
                .iter()
                .position(|r| matches!(r, Row::Item(i) if *i == t))
        });
        self.selected = kept
            .or(top_row)
            .or_else(|| picker_first(&self.rows))
            .unwrap_or(0);
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

    fn ensure_installed(&mut self) {
        if self.installed.is_none() {
            if let Some(rx) = &self.bg_rx {
                if let Ok((inst, open)) = rx.try_recv() {
                    self.installed = Some(inst);
                    self.open_editors = open;
                    self.bg_rx = None;
                }
            }
        }
        if self.installed.is_none() {
            if let Some(cb) = self.discover.as_mut() {
                let (inst, open) = cb();
                self.installed = Some(inst);
                self.open_editors = open;
                self.bg_rx = None;
            }
        }
    }

    fn open_launch_drawer(&mut self) {
        let Some(e) = self.selected_entry().cloned() else {
            return;
        };
        self.ensure_installed();
        let installed = self.installed.as_deref().unwrap_or(&[]);
        let open_here: Vec<OpenEditor> = self
            .open_editors
            .iter()
            .filter(|o| platform::same_path(&o.path, &e.path))
            .cloned()
            .collect();
        let active = self.best.get(&e.path);
        let menu = LaunchMenu::new(
            &e,
            installed,
            &open_here,
            active,
            &self.global_counts,
            true,
            true,
            true,
        );
        self.launch_menu = Some((e, menu));
    }

    fn execute_launch(&mut self, target: InstalledTarget) {
        let Some((entry, _)) = self.launch_menu.take() else {
            return;
        };
        let Some(cb) = self.launch.as_mut() else {
            return;
        };
        let choice = LaunchChoice {
            target,
            force_shell: false,
        };
        match cb(&entry, &choice, &self.open_editors) {
            Ok(msg) => self.flash(&msg, ToastKind::Success),
            Err(err) => self.flash(&err, ToastKind::Error),
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

    fn poll_bg(&mut self) -> bool {
        if let Some(rx) = &self.bg_rx {
            if let Ok((inst, open)) = rx.try_recv() {
                self.installed = Some(inst);
                self.open_editors = open;
                self.bg_rx = None;
                return self.launch_menu.is_some();
            }
        }
        false
    }

    fn loading(&self) -> bool {
        self.bg_rx.is_some() && self.launch_menu.is_some()
    }

    fn mouse(&mut self, m: MouseEvent) -> Flow {
        if let Some((_, ref mut menu)) = self.launch_menu {
            match m.kind {
                MouseEventKind::ScrollUp => menu.mv(-1),
                MouseEventKind::ScrollDown => menu.mv(1),
                MouseEventKind::Down(MouseButton::Left) => {
                    let pos = ratatui::layout::Position {
                        x: m.column,
                        y: m.row,
                    };
                    if self.launch_header_area.contains(pos) {
                        self.launch_menu = None;
                        return Flow::Continue;
                    }
                    if self.launch_list_area.contains(pos) {
                        let idx = menu.offset + (m.row - self.launch_list_area.y) as usize;
                        if matches!(menu.rows.get(idx), Some(LaunchRow::Item(_))) {
                            let dbl = self.last_click.click(idx);
                            menu.selected = idx;
                            if dbl {
                                if let Some(item) = menu.selected_item().cloned() {
                                    self.execute_launch(item.target);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            return Flow::Continue;
        }
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
        if let Some((_, ref mut menu)) = self.launch_menu {
            match k.code {
                KeyCode::Esc | KeyCode::Left => {
                    self.launch_menu = None;
                }
                KeyCode::Char('c') if ctrl => {
                    self.launch_menu = None;
                }
                KeyCode::Tab | KeyCode::BackTab => menu.toggle_section(),
                KeyCode::Up => menu.mv(-1),
                KeyCode::Down => menu.mv(1),
                KeyCode::Char('p') | KeyCode::Char('k') if ctrl => menu.mv(-1),
                KeyCode::Char('n') | KeyCode::Char('j') if ctrl => menu.mv(1),
                KeyCode::PageUp => menu.mv(-10),
                KeyCode::PageDown => menu.mv(10),
                KeyCode::Enter => {
                    if let Some(item) = menu.selected_item().cloned() {
                        self.execute_launch(item.target);
                    }
                }
                KeyCode::Char('w') if ctrl => menu.delete_word(),
                KeyCode::Char('u') if ctrl => menu.clear_query(),
                KeyCode::Backspace => {
                    menu.query.pop();
                    menu.refilter();
                }
                KeyCode::Char(c) if is_plain_char(&k) => {
                    if menu.query.is_empty() {
                        if let Some(item) = menu.item_for_hotkey(c).cloned() {
                            self.execute_launch(item.target);
                            return Flow::Continue;
                        }
                    }
                    menu.query.push(c);
                    menu.refilter();
                }
                _ => {}
            }
            return Flow::Continue;
        }
        match k.code {
            KeyCode::Esc => self.clear_query(),
            KeyCode::Char('c') | KeyCode::Char('u') if ctrl => self.clear_query(),
            KeyCode::Enter => self.choose(false),
            KeyCode::Char('o') if ctrl => self.choose(true),
            KeyCode::Right => self.open_launch_drawer(),
            KeyCode::Char('l') if ctrl => self.open_launch_drawer(),
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
        let force = std::mem::take(&mut self.force_refresh);
        let was_visible = self.visible;
        let tick = (self.reload)(force);
        self.visible = tick.visible;
        if !self.visible {
            return expired;
        }
        if let Some((d, hp)) = tick.data {
            let changed = d.entries != self.data.entries
                || d.sessions != self.data.sessions
                || hp != self.hub_paths;
            if changed {
                self.set_data(d, hp);
                return true;
            }
        }
        expired || !was_visible
    }

    fn every(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn animating(&self) -> bool {
        self.animating && self.visible
    }

    fn wake(&mut self) {
        if !self.visible {
            self.force_refresh = true;
        }
    }

    fn wants_refresh_now(&self) -> bool {
        self.force_refresh
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
            None => {
                let pairs = if sb.launch_menu.is_some() {
                    &LAUNCH_HINTS[..]
                } else {
                    &HINTS[..]
                };
                hint_line(pairs, fw, t)
            }
        };
        f.render_widget(Paragraph::new(line), foot);
    }

    sb.animating = false;
    sb.list_area = Rect::default();
    sb.launch_header_area = Rect::default();
    sb.launch_list_area = Rect::default();
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if sb.launch_menu.is_some() {
        draw_launch_drawer(f, inner, sb);
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

fn draw_launch_drawer(f: &mut Frame, inner: Rect, sb: &mut Sidebar) {
    let t = sb.theme;
    let focused = sb.focused;
    let Some((_, ref mut menu)) = sb.launch_menu else {
        return;
    };
    let iw = inner.width as usize;
    let arrow = if t.ascii { "<" } else { "←" };
    let hdr_area = Rect { height: 1, ..inner };
    sb.launch_header_area = hdr_area;
    let hdr_txt = cut(&format!(" {arrow} {}", menu.folder_name), iw);
    f.render_widget(
        Paragraph::new(Span::styled(hdr_txt, t.title_accent())),
        hdr_area,
    );
    if inner.height < 2 {
        return;
    }

    let q = cut_tail(&menu.query, iw.saturating_sub(5));
    let mut q_spans = vec![
        Span::raw(" "),
        Span::styled("❯ ", t.title_accent()),
        Span::raw(q),
    ];
    if focused {
        q_spans.push(Span::styled(
            " ",
            Style::default().add_modifier(Modifier::REVERSED),
        ));
    }
    if menu.query.is_empty() && iw > 12 {
        q_spans.push(Span::styled(" filter", t.muted()));
    }
    f.render_widget(
        Paragraph::new(Line::from(q_spans)),
        Rect {
            y: inner.y + 1,
            height: 1,
            ..inner
        },
    );

    let body = Rect {
        y: inner.y + 2,
        height: inner.height.saturating_sub(2),
        ..inner
    };
    if body.height == 0 {
        return;
    }
    if menu.rows.is_empty() {
        center_msg(f, body, "No matches", t);
        return;
    }
    let h = body.height as usize;
    menu.offset = picker::scroll_by(
        menu.rows.len(),
        |idx| matches!(menu.rows[idx], LaunchRow::Header(_)),
        menu.selected,
        menu.offset,
        h,
    );
    let (list, sb_area) = slice_scrollbar(body, menu.rows.len(), h);
    sb.launch_list_area = list;
    let lw = list.width as usize;
    let mut lines = Vec::new();
    for ri in menu.offset..menu.rows.len().min(menu.offset + h) {
        let row = menu.rows[ri];
        match row {
            LaunchRow::Header(hdr) => {
                lines.push(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        cut(hdr, lw.saturating_sub(1)),
                        t.muted().add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            LaunchRow::Item(idx) => {
                let item = &menu.items[idx];
                let sel = ri == menu.selected;
                let mut base = Style::default();
                if sel {
                    let bg = if focused { t.sel_bg() } else { t.sel_bg_dim() };
                    if let Some(bg) = bg {
                        base = base.bg(bg);
                    }
                }
                let bar_style = if focused {
                    t.fg(base, t.accent())
                } else {
                    t.fg(base, t.border_color())
                };
                let mut spans = vec![Span::styled(
                    if sel { t.bar_glyph() } else { " " },
                    bar_style,
                )];
                if let Some(hk) = menu.hotkey_for_row(ri) {
                    spans.push(Span::styled(
                        format!("{hk} "),
                        base.patch(t.keycap_style()).patch(t.muted()),
                    ));
                } else {
                    spans.push(Span::styled("  ", base));
                }
                let badge_txt: Cow<'_, str> = if item.is_open {
                    if lw < 36 {
                        Cow::Borrowed(t.editor_glyph())
                    } else {
                        Cow::Borrowed("open")
                    }
                } else if item.badge == "live" {
                    if lw < 36 {
                        Cow::Borrowed(if t.ascii { "!" } else { "●" })
                    } else {
                        Cow::Borrowed("live")
                    }
                } else if lw < 36 && matches!(item.badge.as_str(), "last used" | "last AI") {
                    Cow::Borrowed(t.star())
                } else if lw < 32 {
                    Cow::Borrowed("")
                } else {
                    Cow::Borrowed(item.badge.as_str())
                };
                let badge_w = if badge_txt.is_empty() {
                    0
                } else {
                    sw(&badge_txt) + 1
                };
                let avail = lw.saturating_sub(4 + badge_w);
                let label_clean = platform::sanitize_display(&item.target.label);
                let fit = fit_end(&label_clean, avail);
                let label_style = match item.target.kind {
                    TargetKind::HubShell | TargetKind::Travel => {
                        if sel {
                            t.fg(base, t.accent()).add_modifier(Modifier::BOLD)
                        } else {
                            base.patch(t.muted())
                        }
                    }
                    _ => {
                        let s = t.fg(base, t.tag_color(&item.target.tag));
                        if sel {
                            s.add_modifier(Modifier::BOLD)
                        } else {
                            s
                        }
                    }
                };
                let mut used = 0usize;
                push_fit_coalesced(&mut spans, &mut used, &fit, &[], label_style, label_style);
                let pad = lw.saturating_sub(3 + used + badge_w + 1);
                spans.push(Span::styled(spaces(pad), base));
                if !badge_txt.is_empty() {
                    let b_style = if item.is_open {
                        t.fg(base, t.open_blue())
                    } else if item.badge == "live" || item.badge.starts_with("last") {
                        t.fg(base, t.accent())
                    } else {
                        base.patch(t.muted())
                    };
                    spans.push(Span::styled(" ", base));
                    spans.push(Span::styled(badge_txt.into_owned(), b_style));
                }
                spans.push(Span::styled(" ", base));
                lines.push(Line::from(spans));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), list);
    if let Some(sb_area) = sb_area {
        render_scrollbar(f, sb_area, menu.rows.len(), menu.offset, h, t);
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
            shell_start: None,
            agent_pid: None,
            agent_start: None,
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
            installed: None,
        }
    }

    fn sidebar<'a>(go: Go<'a>) -> Sidebar<'a> {
        Sidebar::new(
            data(),
            Vec::new(),
            "/h/notes",
            Theme::dark(),
            Some("/h".into()),
            Box::new(|_| ReloadTick {
                visible: true,
                data: None,
            }),
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
    fn filter_selects_best_match_not_first_agent_row() {
        // The agent row matches "other" only through scattered letters; the
        // folder matches it exactly. Enter and the launcher must act on the folder.
        let d = Data {
            entries: vec![entry("/h/oxtxhxexr", &["claude"]), entry("/h/other", &[])],
            sessions: vec![session("/h/oxtxhxexr", "claude", Status::Done)],
            editors: vec![],
            branches: HashMap::new(),
            installed: None,
        };
        let mut sb = Sidebar::new(
            d,
            Vec::new(),
            "/h",
            Theme::dark(),
            Some("/h".into()),
            Box::new(|_| ReloadTick {
                visible: true,
                data: None,
            }),
            Box::new(|_, _| Ok(())),
        );
        for c in "other".chars() {
            sb.key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert!(matches!(sb.rows[0], Row::Header(Section::Agents(_))));
        assert_eq!(sb.selected_entry().unwrap().path, "/h/other");
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

    fn mock_installed() -> Vec<InstalledTarget> {
        use crate::launcher::LaunchMethod;
        vec![
            InstalledTarget {
                id: "antigravity".into(),
                label: "Antigravity".into(),
                tag: "antigravity".into(),
                kind: TargetKind::GuiApp,
                method: LaunchMethod::Gui {
                    prog: "open".into(),
                    args_before: vec!["-a".into(), "Antigravity".into()],
                    pass_path: false,
                    focus_only: true,
                },
            },
            InstalledTarget {
                id: "cli:claude".into(),
                label: "claude".into(),
                tag: "claude".into(),
                kind: TargetKind::CliAgent,
                method: LaunchMethod::CliAgent {
                    tool_tag: "claude".into(),
                    launch_cmd: "claude".into(),
                },
            },
            InstalledTarget {
                id: "cli:opencode".into(),
                label: "opencode".into(),
                tag: "opencode".into(),
                kind: TargetKind::CliAgent,
                method: LaunchMethod::CliAgent {
                    tool_tag: "opencode".into(),
                    launch_cmd: "opencode".into(),
                },
            },
        ]
    }

    #[test]
    fn launch_drawer_opens_filters_and_fires_hotkeys() {
        let launched = std::cell::RefCell::new(Vec::new());
        let mut sb = sidebar(Box::new(|_, _| Ok(()))).with_launcher(
            HashMap::new(),
            None,
            Some(Box::new(|| (mock_installed(), Vec::new()))),
            Some(Box::new(|e, c, _| {
                launched
                    .borrow_mut()
                    .push((e.path.clone(), c.target.id.clone()));
                Ok(format!("started {}", c.target.label))
            })),
        );
        // Move to knn (/h/code/knn) and open drawer with Right.
        sb.mv(-10);
        sb.mv(1);
        assert_eq!(sb.selected_entry().unwrap().path, "/h/code/knn");
        sb.key(KeyEvent::from(KeyCode::Right));
        assert!(sb.launch_menu.is_some());

        // Narrow 25-col rendering fits width cleanly and keeps "← back" visible.
        let narrow = tui::render_text(25, 14, |f| draw(f, &mut sb));
        let narrow_lines: Vec<&str> = narrow.lines().collect();
        assert!(narrow_lines.last().unwrap().contains("← back"), "{narrow}");
        for l in &narrow_lines {
            assert!(l.chars().count() <= 25, "{l}");
        }

        // Narrow 30-col rendering fits width cleanly.
        let out = tui::render_text(30, 14, |f| draw(f, &mut sb));
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].contains("← knn"), "{}", lines[0]);
        assert!(out.contains("APPS") && out.contains("CLI → HUB"), "{out}");
        for l in &lines {
            assert!(l.chars().count() <= 30, "{l}");
        }

        // Tab jumps selection between Apps and CLI sections.
        assert_eq!(
            sb.launch_menu
                .as_ref()
                .unwrap()
                .1
                .selected_item()
                .unwrap()
                .target
                .kind,
            TargetKind::GuiApp
        );
        sb.key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(
            sb.launch_menu
                .as_ref()
                .unwrap()
                .1
                .selected_item()
                .unwrap()
                .target
                .kind,
            TargetKind::CliAgent
        );
        sb.key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(
            sb.launch_menu
                .as_ref()
                .unwrap()
                .1
                .selected_item()
                .unwrap()
                .target
                .kind,
            TargetKind::GuiApp
        );

        // Esc / Left dismisses drawer.
        sb.key(KeyEvent::from(KeyCode::Left));
        assert!(sb.launch_menu.is_none());

        // Ctrl-L opens drawer, typing "opencode" filters and Enter launches.
        sb.key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert!(sb.launch_menu.is_some());
        for c in "opencode".chars() {
            sb.key(KeyEvent::from(KeyCode::Char(c)));
        }
        sb.key(KeyEvent::from(KeyCode::Enter));
        assert!(sb.launch_menu.is_none());

        // Reopen and use instant digit hotkey '1'.
        sb.key(KeyEvent::from(KeyCode::Right));
        sb.key(KeyEvent::from(KeyCode::Char('1')));
        assert!(sb.launch_menu.is_none());

        drop(sb);
        assert_eq!(
            launched.into_inner(),
            vec![
                ("/h/code/knn".into(), "cli:opencode".into()),
                ("/h/code/knn".into(), "antigravity".into()),
            ]
        );
    }

    #[test]
    fn hidden_sidebar_stops_animation_and_wakes_on_input() {
        let vis_flag = std::rc::Rc::new(std::cell::Cell::new(true));
        let vf = vis_flag.clone();
        let mut sb = Sidebar::new(
            data(),
            Vec::new(),
            "/h/notes",
            Theme::dark(),
            Some("/h".into()),
            Box::new(move |_| ReloadTick {
                visible: vf.get(),
                data: None,
            }),
            Box::new(|_, _| Ok(())),
        );
        let _ = tui::render_text(30, 14, |f| draw(f, &mut sb));
        assert!(sb.animating());

        vis_flag.set(false);
        assert!(!sb.refresh());
        assert!(!sb.animating());
        assert!(!sb.wants_refresh_now());

        sb.wake();
        assert!(sb.wants_refresh_now());

        vis_flag.set(true);
        assert!(sb.refresh());
        assert!(sb.animating());
        assert!(!sb.wants_refresh_now());
    }
}
