//! Shared TUI widgets, grapheme-safe truncation, scrollbars, modals, and toasts.
use crate::theme::{Theme, ToastKind};
use ratatui::{
    layout::{Alignment, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    Frame,
};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Display width of a string in terminal columns.
pub fn sw(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Shown grapheme clusters with their inclusive-exclusive char range `(start, end)`.
/// `None` marks the synthetic ellipsis `'…'`.
pub type Fit = Vec<(String, Option<(usize, usize)>)>;

#[derive(Clone)]
struct Cluster<'a> {
    g: &'a str,
    w: usize,
    char_start: usize,
    char_end: usize,
}

fn clusters(s: &str) -> Vec<Cluster<'_>> {
    let mut out = Vec::new();
    let mut char_pos = 0usize;
    for g in UnicodeSegmentation::graphemes(s, true) {
        let chars = g.chars().count();
        let w = sw(g);
        out.push(Cluster {
            g,
            w,
            char_start: char_pos,
            char_end: char_pos + chars,
        });
        char_pos += chars;
    }
    out
}

fn fit_all_clusters(cs: &[Cluster<'_>]) -> Fit {
    cs.iter()
        .map(|c| (c.g.to_string(), Some((c.char_start, c.char_end))))
        .collect()
}

/// Fit `s` into `w` columns. Cut the end and add "…" if too long.
pub fn fit_end(s: &str, w: usize) -> Fit {
    let cs = clusters(s);
    if cs.iter().map(|c| c.w).sum::<usize>() <= w {
        return fit_all_clusters(&cs);
    }
    if w == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut used = 0;
    for c in &cs {
        if used + c.w > w - 1 {
            break;
        }
        used += c.w;
        out.push((c.g.to_string(), Some((c.char_start, c.char_end))));
    }
    out.push(("…".into(), None));
    out
}

/// Fit `s` into `w` columns. Cut the start and add "…" if too long (great for typing queries).
pub fn fit_tail(s: &str, w: usize) -> Fit {
    let cs = clusters(s);
    if cs.iter().map(|c| c.w).sum::<usize>() <= w {
        return fit_all_clusters(&cs);
    }
    if w == 0 {
        return Vec::new();
    }
    let mut tail = Vec::new();
    let mut used = 0;
    for c in cs.iter().rev() {
        if used + c.w > w - 1 {
            break;
        }
        used += c.w;
        tail.push((c.g.to_string(), Some((c.char_start, c.char_end))));
    }
    tail.reverse();
    let mut out = vec![("…".into(), None)];
    out.extend(tail);
    out
}

/// Fit `s` into `w` columns. Cut the middle and put "…" there if too long.
/// The tail gets the extra column, because the end of a path says more.
pub fn fit_middle(s: &str, w: usize) -> Fit {
    let cs = clusters(s);
    if cs.iter().map(|c| c.w).sum::<usize>() <= w {
        return fit_all_clusters(&cs);
    }
    if w == 0 {
        return Vec::new();
    }
    let budget = w - 1;
    let head_w = budget / 2;
    let tail_w = budget - head_w;
    let mut head = Vec::new();
    let mut used = 0;
    let mut head_count = 0usize;
    for c in &cs {
        if used + c.w > head_w {
            break;
        }
        used += c.w;
        head.push((c.g.to_string(), Some((c.char_start, c.char_end))));
        head_count += 1;
    }
    let mut tail = Vec::new();
    used = 0;
    for (idx, c) in cs.iter().enumerate().rev() {
        if idx < head_count || used + c.w > tail_w {
            break;
        }
        used += c.w;
        tail.push((c.g.to_string(), Some((c.char_start, c.char_end))));
    }
    tail.reverse();
    head.push(("…".into(), None));
    head.extend(tail);
    head
}

/// Cut `s` to `w` columns with "…" at the end.
pub fn cut(s: &str, w: usize) -> String {
    let mut out = String::new();
    for (g, _) in fit_end(s, w) {
        out.push_str(&g);
    }
    out
}

/// Cut `s` to `w` columns with "…" at the start (keeps the typing tail visible).
pub fn cut_tail(s: &str, w: usize) -> String {
    let mut out = String::new();
    for (g, _) in fit_tail(s, w) {
        out.push_str(&g);
    }
    out
}

/// Cut `s` to `w` columns with "…" in the middle.
pub fn cut_mid(s: &str, w: usize) -> String {
    let mut out = String::new();
    for (g, _) in fit_middle(s, w) {
        out.push_str(&g);
    }
    out
}

/// Push `fit` onto `spans`, coalescing contiguous runs that share highlight state.
pub fn push_fit_coalesced(
    spans: &mut Vec<Span<'static>>,
    used: &mut usize,
    fit: &Fit,
    hl: &[usize],
    style: Style,
    hl_style: Style,
) {
    let mut run = String::new();
    let mut run_hl = false;
    for (g, range) in fit {
        let is_hl = range.is_some_and(|(a, b)| hl.iter().any(|&p| p >= a && p < b));
        if !run.is_empty() && is_hl != run_hl {
            *used += sw(&run);
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_hl { hl_style } else { style },
            ));
        }
        run_hl = is_hl;
        run.push_str(g);
    }
    if !run.is_empty() {
        *used += sw(&run);
        spans.push(Span::styled(run, if run_hl { hl_style } else { style }));
    }
}

/// Build keybinding hint spans for a bottom border/bar, never dimmed on the keycaps.
pub fn hint_spans(pairs: &[(&str, &str)], w: usize, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(" ")];
    let mut used = 2;
    let mut first = true;
    let sep_style = theme.title_muted();
    let key_style = theme.keycap_style();
    let lbl_style = theme.title_muted();
    for (k, l) in pairs {
        let add = sw(k) + 1 + sw(l) + if first { 0 } else { 3 };
        if used + add > w {
            continue;
        }
        if !first {
            spans.push(Span::styled(" · ", sep_style));
        }
        spans.push(Span::styled((*k).to_string(), key_style));
        spans.push(Span::styled(format!(" {l}"), lbl_style));
        used += add;
        first = false;
    }
    spans.push(Span::raw(" "));
    spans
}

pub fn hint_line(pairs: &[(&str, &str)], w: usize, theme: &Theme) -> Line<'static> {
    Line::from(hint_spans(pairs, w, theme))
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub msg: String,
    pub kind: ToastKind,
    pub until: Instant,
}

impl Toast {
    pub fn new(msg: impl Into<String>, kind: ToastKind, ms: u64) -> Self {
        Toast {
            msg: crate::platform::sanitize_display(&msg.into()),
            kind,
            until: Instant::now() + Duration::from_millis(ms),
        }
    }
}

pub fn toast_line(toast: &Toast, w: usize, theme: &Theme) -> Line<'static> {
    let icon = match toast.kind {
        ToastKind::Success => {
            if theme.ascii {
                "+"
            } else {
                "✓"
            }
        }
        ToastKind::Warn => "!",
        ToastKind::Error => {
            if theme.ascii {
                "x"
            } else {
                "✗"
            }
        }
        ToastKind::Info => theme.star(),
    };
    let text = cut(&format!("{icon} {}", toast.msg), w.saturating_sub(2));
    Line::from(vec![
        Span::raw(" "),
        Span::styled(text, theme.toast_style(toast.kind)),
        Span::raw(" "),
    ])
}

/// Centered single-line status message inside `area`.
pub fn center_msg(f: &mut Frame, area: Rect, msg: &str, theme: &Theme) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let y = area.y + area.height.saturating_sub(1) / 2;
    let p = Paragraph::new(Span::styled(cut(msg, area.width as usize), theme.muted()))
        .alignment(Alignment::Center);
    f.render_widget(
        p,
        Rect {
            y,
            height: 1,
            ..area
        },
    );
}

/// Draw a slim vertical scrollbar in the 1-col `area` when `total > height`.
pub fn render_scrollbar(
    f: &mut Frame,
    area: Rect,
    total: usize,
    offset: usize,
    height: usize,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 || height == 0 || total <= height {
        return;
    }
    let track_h = area.height as usize;
    let thumb_h = ((height * track_h) / total).clamp(1, track_h);
    let max_off = total.saturating_sub(height);
    let top = (offset.min(max_off) * (track_h - thumb_h) + max_off / 2)
        .checked_div(max_off)
        .unwrap_or(0);
    let thumb_style = theme.fg(Style::default(), theme.accent());
    let track_style = theme.fg(Style::default(), theme.border_color());
    let lines: Vec<Line> = (0..track_h)
        .map(|row| {
            if row >= top && row < top + thumb_h {
                Line::from(Span::styled(theme.scroll_thumb(), thumb_style))
            } else {
                Line::from(Span::styled(theme.scroll_track(), track_style))
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), Rect { width: 1, ..area });
}

/// Render a centered modal overlay with Clear + rounded gold border + footer hints.
pub fn render_modal(
    f: &mut Frame,
    area: Rect,
    title: &str,
    body: Vec<Line<'static>>,
    hints: &[(&str, &str)],
    theme: &Theme,
) {
    if area.width < 16 || area.height < 6 {
        return;
    }
    let max_line = body
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(20)
        .max(sw(title) + 6);
    let w = ((max_line + 6) as u16).clamp(32.min(area.width), area.width.saturating_sub(2).max(16));
    let h =
        ((body.len() + 4) as u16).clamp(6.min(area.height), area.height.saturating_sub(2).max(6));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    f.render_widget(Clear, rect);
    let mut block_style = Style::default();
    if let Some(bg) = theme.modal_bg() {
        block_style = block_style.bg(bg);
    }
    let inner_h = (h as usize).saturating_sub(2);
    let shown_body: Vec<Line<'static>> = if body.len() > inner_h && inner_h >= 2 {
        let mut cut_lines: Vec<Line<'static>> = body.into_iter().take(inner_h - 1).collect();
        cut_lines.push(Line::from(Span::styled(
            "  ... (resize taller for all)",
            theme.muted(),
        )));
        cut_lines
    } else {
        body
    };
    let block = Block::bordered()
        .border_type(theme.border_type())
        .border_style(theme.border_style(true))
        .style(block_style)
        .title_top(Line::from(Span::styled(
            format!(" {} {title} ", theme.star()),
            theme.title_accent(),
        )))
        .title_bottom(hint_line(hints, (w as usize).saturating_sub(4), theme));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    if inner.width > 2 && inner.height > 0 {
        let top_pad = if inner.height > shown_body.len() as u16 + 1 {
            1
        } else {
            0
        };
        let padded = Rect {
            x: inner.x + 1,
            y: inner.y + top_pad,
            width: inner.width - 2,
            height: inner.height.saturating_sub(top_pad),
        };
        f.render_widget(Paragraph::new(shown_body), padded);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fit_text(f: &Fit) -> String {
        f.iter().map(|(s, _)| s.as_str()).collect()
    }

    #[test]
    fn grapheme_truncation_handles_nfd_and_emoji() {
        assert_eq!(fit_text(&fit_end("knn", 5)), "knn");
        assert_eq!(fit_text(&fit_end("kmeans-clustering", 8)), "kmeans-…");
        assert_eq!(fit_text(&fit_end("abc", 0)), "");
        // NFD decomposed e + combining acute stays intact as 1 grapheme.
        assert_eq!(fit_text(&fit_end("cafe\u{0301}-app", 5)), "cafe\u{0301}…");
        assert_eq!(fit_text(&fit_middle("~/code/aiml", 20)), "~/code/aiml");
        let m = fit_middle("~/code/very/deep/nested/aiml", 12);
        assert_eq!(fit_text(&m), "~/cod…d/aiml");
        assert_eq!(m[5], ("…".into(), None));
        assert_eq!(m[6].1, Some((22, 23)));
        assert_eq!(cut_tail("long/search/query/tail", 12), "…/query/tail");
    }

    #[test]
    fn hint_line_never_dims_keys() {
        use ratatui::style::Modifier;
        let t = Theme::dark();
        let l = hint_line(&[("j/k", "move"), ("q", "quit")], 40, &t);
        let key_span = &l.spans[1];
        assert_eq!(key_span.content, "j/k");
        assert!(key_span.style.add_modifier.contains(Modifier::BOLD));
        assert!(!key_span.style.add_modifier.contains(Modifier::DIM));
    }
}
