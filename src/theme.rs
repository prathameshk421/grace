//! Unified "Site of Grace" visual language: adaptive dark/light/256/NO_COLOR tokens,
//! status glyphs, brand tool colors, and CLI ANSI helpers.
use crate::config::Config;
use crate::model::Status;
use ratatui::style::{Color, Modifier, Style};
use std::io::IsTerminal;

pub const GOLD: Color = Color::Rgb(232, 185, 74);
pub const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
pub const SPINNER_ASCII: [char; 4] = ['|', '/', '-', '\\'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Error,
}

/// Shared CLI ANSI palette and glyph tokens for stdout/stderr commands (`doctor`, `setup`, `stats`, etc.).
#[derive(Debug, Clone, Copy)]
pub struct Ansi {
    pub gold: &'static str,
    pub green: &'static str,
    pub yellow: &'static str,
    pub red: &'static str,
    pub cyan: &'static str,
    pub dim: &'static str,
    pub bold: &'static str,
    pub reset: &'static str,
    pub star: &'static str,
    pub check: &'static str,
    pub cross: &'static str,
}

impl Ansi {
    pub fn stdout(cfg: &Config) -> Self {
        Self::new(std::io::stdout().is_terminal(), cfg)
    }

    pub fn stderr(cfg: &Config) -> Self {
        Self::new(std::io::stderr().is_terminal(), cfg)
    }

    fn new(is_tty: bool, cfg: &Config) -> Self {
        let th = Theme::from_env_and_config(cfg);
        let (star, check, cross) = if th.ascii {
            ("*", "+", "x")
        } else {
            ("✦", "✓", "✗")
        };
        if !is_tty || !th.color {
            return Self {
                gold: "",
                green: "",
                yellow: "",
                red: "",
                cyan: "",
                dim: "",
                bold: "",
                reset: "",
                star,
                check,
                cross,
            };
        }
        let gold = if th.light {
            if th.truecolor {
                "\x1b[1;38;2;176;121;8m"
            } else {
                "\x1b[1;33m"
            }
        } else if th.truecolor {
            "\x1b[1;38;2;232;185;74m"
        } else {
            "\x1b[1;33m"
        };
        Self {
            gold,
            green: "\x1b[32m",
            yellow: "\x1b[33m",
            red: "\x1b[31m",
            cyan: "\x1b[36m",
            dim: "\x1b[2m",
            bold: "\x1b[1m",
            reset: "\x1b[0m",
            star,
            check,
            cross,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// False with NO_COLOR / CLICOLOR=0 / TERM=dumb: modifiers stay, colors go.
    pub color: bool,
    /// Terminal supports 24-bit RGB color.
    pub truecolor: bool,
    /// Light background terminal detected or requested.
    pub light: bool,
    /// ASCII-only emblem fallback.
    pub ascii: bool,
}

impl Theme {
    pub fn from_env_and_config(cfg: &Config) -> Theme {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let clicolor_off = std::env::var("CLICOLOR").is_ok_and(|v| v == "0");
        let term_dumb = std::env::var("TERM").is_ok_and(|v| v == "dumb");
        let force = std::env::var("CLICOLOR_FORCE").is_ok_and(|v| !v.is_empty() && v != "0");
        let color = if no_color {
            false
        } else if force {
            true
        } else {
            !clicolor_off && !term_dumb
        };

        let term_prog = std::env::var("TERM_PROGRAM").unwrap_or_default();
        let truecolor = if term_prog == "Apple_Terminal" {
            false
        } else {
            let ct = std::env::var("COLORTERM")
                .map(|v| matches!(v.to_ascii_lowercase().as_str(), "truecolor" | "24bit"))
                .unwrap_or(false);
            let wt = std::env::var_os("WT_SESSION").is_some_and(|v| !v.is_empty());
            let known_tc_prog = matches!(
                term_prog.as_str(),
                "iTerm.app" | "WezTerm" | "ghostty" | "vscode" | "Tabby" | "Hyper" | "rio"
            );
            let direct_term = std::env::var("TERM")
                .map(|t| t.contains("direct") || t.contains("truecolor"))
                .unwrap_or(false);
            ct || wt || known_tc_prog || direct_term
        };

        let theme_choice = crate::platform::env_non_empty("GRACE_THEME")
            .or_else(|| cfg.theme.clone())
            .unwrap_or_else(|| "auto".into());
        let light = match theme_choice.to_ascii_lowercase().as_str() {
            "light" => true,
            "dark" => false,
            _ => detect_light_env(),
        };

        let ascii_env = crate::platform::env_non_empty("GRACE_ASCII").and_then(|v| {
            match v.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" => Some(true),
                "0" | "false" | "no" => Some(false),
                _ => None,
            }
        });
        let ascii = ascii_env.or(cfg.ascii).unwrap_or(false);

        Theme {
            color,
            truecolor,
            light,
            ascii,
        }
    }

    #[cfg(test)]
    pub fn dark() -> Theme {
        Theme {
            color: true,
            truecolor: true,
            light: false,
            ascii: false,
        }
    }

    /// Map a color to what the terminal can show. None means no color.
    pub fn map(self, c: Color) -> Option<Color> {
        if !self.color {
            return None;
        }
        match c {
            Color::Rgb(r, g, b) if !self.truecolor => Some(Color::Indexed(rgb_to_256(r, g, b))),
            c => Some(c),
        }
    }

    pub fn fg(self, s: Style, c: Color) -> Style {
        match self.map(c) {
            Some(c) => s.fg(c),
            None => s,
        }
    }

    /// Apply the theme rules (NO_COLOR, 256 colors) to a style preserving both fg and bg.
    pub fn apply(self, s: Style) -> Style {
        let mut out = Style::default()
            .add_modifier(s.add_modifier)
            .remove_modifier(s.sub_modifier);
        if let Some(c) = s.fg.and_then(|c| self.map(c)) {
            out = out.fg(c);
        }
        if let Some(c) = s.bg.and_then(|c| self.map(c)) {
            out = out.bg(c);
        }
        out
    }

    fn pick(self, dark_rgb: Color, light_rgb: Color, dark_256: Color, light_256: Color) -> Color {
        match (self.truecolor, self.light) {
            (true, false) => dark_rgb,
            (true, true) => light_rgb,
            (false, false) => dark_256,
            (false, true) => light_256,
        }
    }

    fn pick_opt(
        self,
        dark_rgb: Color,
        light_rgb: Color,
        dark_256: Color,
        light_256: Color,
    ) -> Option<Color> {
        self.color
            .then(|| self.pick(dark_rgb, light_rgb, dark_256, light_256))
    }

    pub fn sym(self, uni: &'static str, asc: &'static str) -> &'static str {
        if self.ascii {
            asc
        } else {
            uni
        }
    }

    pub fn accent(self) -> Color {
        self.pick(
            GOLD,
            Color::Rgb(176, 121, 8),
            Color::Yellow,
            Color::Indexed(130),
        )
    }

    pub fn accent_hi(self) -> Color {
        self.pick(
            Color::Rgb(255, 233, 163),
            Color::Rgb(122, 79, 0),
            Color::LightYellow,
            Color::Indexed(94),
        )
    }

    pub fn fg_muted(self) -> Color {
        self.pick(
            Color::Rgb(139, 145, 160),
            Color::Rgb(92, 99, 112),
            Color::Gray,
            Color::Indexed(242),
        )
    }

    pub fn border_color(self) -> Color {
        self.pick(
            Color::Rgb(63, 68, 82),
            Color::Rgb(200, 205, 216),
            Color::Indexed(240),
            Color::Indexed(246),
        )
    }

    pub fn sel_bg(self) -> Option<Color> {
        self.pick_opt(
            Color::Rgb(40, 44, 56),
            Color::Rgb(236, 231, 218),
            Color::Indexed(237),
            Color::Indexed(254),
        )
    }

    pub fn sel_bg_dim(self) -> Option<Color> {
        self.pick_opt(
            Color::Rgb(31, 34, 43),
            Color::Rgb(243, 241, 235),
            Color::Indexed(235),
            Color::Indexed(255),
        )
    }

    pub fn pill_bg(self) -> Option<Color> {
        self.pick_opt(
            Color::Rgb(35, 39, 50),
            Color::Rgb(229, 232, 240),
            Color::Indexed(235),
            Color::Indexed(253),
        )
    }

    pub fn modal_bg(self) -> Option<Color> {
        self.pick_opt(
            Color::Rgb(26, 29, 36),
            Color::Rgb(248, 247, 243),
            Color::Indexed(234),
            Color::Indexed(255),
        )
    }

    pub fn open_blue(self) -> Color {
        self.pick(
            Color::Rgb(97, 175, 239),
            Color::Rgb(9, 105, 218),
            Color::Blue,
            Color::Indexed(25),
        )
    }

    pub fn diff_add_fg(self) -> Color {
        self.pick(
            Color::Rgb(133, 211, 155),
            Color::Rgb(17, 99, 41),
            Color::Green,
            Color::Indexed(28),
        )
    }

    pub fn diff_add_bg(self) -> Option<Color> {
        if !self.color || !self.truecolor {
            None
        } else if self.light {
            Some(Color::Rgb(230, 247, 235))
        } else {
            Some(Color::Rgb(24, 41, 30))
        }
    }

    pub fn diff_del_fg(self) -> Color {
        self.pick(
            Color::Rgb(240, 113, 120),
            Color::Rgb(180, 35, 24),
            Color::Red,
            Color::Indexed(160),
        )
    }

    pub fn diff_del_bg(self) -> Option<Color> {
        if !self.color || !self.truecolor {
            None
        } else if self.light {
            Some(Color::Rgb(253, 235, 236))
        } else {
            Some(Color::Rgb(46, 25, 30))
        }
    }

    pub fn diff_hunk_fg(self) -> Color {
        self.pick(
            Color::Rgb(103, 183, 209),
            Color::Rgb(5, 80, 174),
            Color::Cyan,
            Color::Indexed(25),
        )
    }

    pub fn diff_conflict_fg(self) -> Color {
        self.pick(
            Color::Rgb(198, 120, 221),
            Color::Rgb(130, 80, 223),
            Color::Magenta,
            Color::Indexed(127),
        )
    }

    pub fn diff_hunk_bg(self) -> Option<Color> {
        if !self.color || !self.truecolor {
            None
        } else if self.light {
            Some(Color::Rgb(234, 242, 251))
        } else {
            Some(Color::Rgb(27, 36, 48))
        }
    }

    pub fn toast_style(self, kind: ToastKind) -> Style {
        let s = Style::default().remove_modifier(Modifier::DIM);
        match kind {
            ToastKind::Info => self.fg(s, self.accent()),
            ToastKind::Success => self.fg(s, self.diff_add_fg()),
            ToastKind::Warn => self.fg(s, self.accent()).add_modifier(Modifier::BOLD),
            ToastKind::Error => self.fg(s, self.diff_del_fg()).add_modifier(Modifier::BOLD),
        }
    }

    /// Border style without `Modifier::DIM` in color mode so block titles never fade.
    pub fn border_style(self, focused: bool) -> Style {
        if !self.color {
            if focused {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                dim()
            }
        } else if focused {
            self.fg(Style::default(), self.accent())
        } else {
            self.fg(Style::default(), self.border_color())
        }
    }

    pub fn title_accent(self) -> Style {
        self.fg(Style::default(), self.accent())
            .add_modifier(Modifier::BOLD)
            .remove_modifier(Modifier::DIM)
    }

    pub fn title_muted(self) -> Style {
        self.muted().remove_modifier(Modifier::DIM)
    }

    pub fn keycap_style(self) -> Style {
        Style::default()
            .add_modifier(Modifier::BOLD)
            .remove_modifier(Modifier::DIM)
    }

    pub fn muted(self) -> Style {
        if self.color {
            self.fg(Style::default(), self.fg_muted())
        } else {
            dim()
        }
    }

    pub fn star(self) -> &'static str {
        self.sym("✦", "*")
    }

    pub fn pin_glyph(self) -> char {
        if self.ascii {
            '*'
        } else {
            '★'
        }
    }

    pub fn editor_glyph(self) -> &'static str {
        self.sym("▣", "#")
    }

    pub fn active_ws_glyph(self) -> &'static str {
        self.sym("◆", ">")
    }

    pub fn open_ws_glyph(self) -> &'static str {
        self.sym("◇", "-")
    }

    pub fn branch_glyph(self) -> &'static str {
        self.sym("⎇", "@")
    }

    pub fn scroll_thumb(self) -> &'static str {
        self.sym("█", "#")
    }

    pub fn scroll_track(self) -> &'static str {
        self.sym("│", "|")
    }

    pub fn h_rule(self) -> &'static str {
        self.sym("─", "-")
    }

    pub fn v_rule(self) -> &'static str {
        self.sym("│", "|")
    }

    pub fn dot_sep(self) -> &'static str {
        self.sym(" · ", " | ")
    }

    pub fn border_type(self) -> ratatui::widgets::BorderType {
        if self.ascii {
            ratatui::widgets::BorderType::Plain
        } else {
            ratatui::widgets::BorderType::Rounded
        }
    }

    pub fn bar_glyph(self) -> &'static str {
        self.sym("▌", ">")
    }

    pub fn status_look(self, s: Status, frame: usize) -> (char, &'static str, Style) {
        let spin = if self.ascii {
            SPINNER_ASCII[frame % SPINNER_ASCII.len()]
        } else {
            SPINNER[frame % SPINNER.len()]
        };
        let wait_c = self.pick(
            Color::Rgb(240, 184, 73),
            Color::Rgb(184, 110, 0),
            Color::Yellow,
            Color::Indexed(136),
        );
        let work_c = self.pick(
            Color::Rgb(86, 182, 194),
            Color::Rgb(14, 124, 134),
            Color::Cyan,
            Color::Indexed(31),
        );
        let done_c = self.diff_add_fg();
        match s {
            Status::Waiting => (
                if self.ascii { '!' } else { '●' },
                "needs you",
                self.fg(Style::default(), wait_c)
                    .add_modifier(Modifier::BOLD),
            ),
            Status::Ready => (
                if self.ascii { 'o' } else { '○' },
                "your turn",
                Style::default(),
            ),
            Status::Working => (spin, "working", self.fg(Style::default(), work_c)),
            Status::Done => (
                if self.ascii { '+' } else { '✓' },
                "done",
                self.fg(Style::default(), done_c),
            ),
        }
    }

    pub fn tag_color(self, tool: &str) -> Color {
        if self.light {
            return match tool {
                "claude" | "claude-desktop" => Color::Rgb(184, 82, 50),
                "codex" | "codex-app" => Color::Rgb(91, 76, 212),
                "antigravity" => Color::Rgb(16, 98, 214),
                "gemini" => Color::Rgb(124, 58, 237),
                "aider" => Color::Rgb(21, 128, 61),
                "amp" => Color::Rgb(122, 108, 47),
                "crush" => Color::Rgb(91, 61, 219),
                "qwen" => Color::Rgb(29, 78, 216),
                "t3code" => Color::Rgb(75, 85, 99),
                "cursor" | "opencode" | "copilot" | "goose" | "windsurf" | "zed" => {
                    Color::Rgb(71, 80, 94)
                }
                "vscode" => Color::Rgb(0, 102, 184),
                "vscodium" => Color::Rgb(2, 132, 199),
                "neovim" => Color::Rgb(54, 125, 38),
                "vim" => Color::Rgb(1, 122, 40),
                "sublime" => Color::Rgb(194, 106, 0),
                "jetbrains" => Color::Rgb(196, 24, 106),
                _ => Color::DarkGray,
            };
        }
        const MONO: Color = Color::Rgb(207, 212, 222);
        match tool {
            "claude" | "claude-desktop" => Color::Rgb(217, 119, 87),
            "codex" | "codex-app" => Color::Rgb(177, 167, 255),
            "antigravity" => Color::Rgb(49, 134, 255),
            "gemini" => Color::Rgb(179, 129, 221),
            "aider" => Color::Rgb(60, 230, 130),
            "amp" => Color::Rgb(230, 228, 205),
            "crush" => Color::Rgb(139, 116, 255),
            "qwen" => Color::Rgb(109, 139, 255),
            "t3code" => Color::Rgb(196, 202, 212),
            "cursor" | "opencode" | "copilot" | "goose" => MONO,
            "vscode" => Color::Rgb(31, 156, 240),
            "vscodium" => Color::Rgb(70, 190, 230),
            "neovim" => Color::Rgb(87, 161, 67),
            "vim" => Color::Rgb(46, 189, 89),
            "sublime" => Color::Rgb(255, 152, 0),
            "jetbrains" => Color::Rgb(255, 49, 140),
            "windsurf" | "zed" => MONO,
            _ => Color::Gray,
        }
    }
}

pub fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn detect_light_env() -> bool {
    let Ok(v) = std::env::var("COLORFGBG") else {
        return false;
    };
    let Some(bg_s) = v.rsplit(';').next() else {
        return false;
    };
    let Ok(bg) = bg_s.trim().parse::<u8>() else {
        return false;
    };
    bg == 7 || bg >= 15
}

/// Nearest xterm 256-color index for an RGB color.
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_follow_the_terminal() {
        let t = Theme {
            color: true,
            truecolor: false,
            light: false,
            ascii: false,
        };
        assert_eq!(t.accent(), Color::Yellow);
        assert_eq!(t.sel_bg(), Some(Color::Indexed(237)));
        assert_ne!(t.sel_bg(), Some(t.border_color()));
        assert_eq!(t.map(Color::Rgb(0, 122, 204)), Some(Color::Indexed(32)));
        let t = Theme {
            color: false,
            truecolor: true,
            light: false,
            ascii: false,
        };
        assert_eq!(t.map(Color::Red), None);
        assert_eq!(t.sel_bg(), None);
        assert_ne!(t.tag_color("vscode"), t.tag_color("antigravity"));
    }

    #[test]
    fn apply_preserves_fg_and_bg() {
        let t = Theme {
            color: true,
            truecolor: false,
            light: false,
            ascii: false,
        };
        let s = Style::default()
            .fg(Color::Rgb(0, 122, 204))
            .bg(Color::Rgb(40, 44, 52))
            .add_modifier(Modifier::BOLD);
        let mapped = t.apply(s);
        assert_eq!(mapped.fg, Some(Color::Indexed(32)));
        assert!(mapped.bg.is_some());
        assert!(mapped.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn light_theme_contrast_remaps() {
        let dark = Theme::dark();
        let light = Theme {
            color: true,
            truecolor: true,
            light: true,
            ascii: false,
        };
        assert_ne!(dark.accent(), light.accent());
        assert_ne!(dark.tag_color("amp"), light.tag_color("amp"));
        assert_ne!(dark.sel_bg(), light.sel_bg());
    }

    #[test]
    fn ascii_env_overrides_config() {
        let prev = std::env::var_os("GRACE_ASCII");
        std::env::set_var("GRACE_ASCII", "0");
        let cfg = Config {
            ascii: Some(true),
            ..Config::default()
        };
        assert!(!Theme::from_env_and_config(&cfg).ascii);
        std::env::set_var("GRACE_ASCII", "1");
        let cfg_false = Config {
            ascii: Some(false),
            ..Config::default()
        };
        assert!(Theme::from_env_and_config(&cfg_false).ascii);
        match prev {
            Some(v) => std::env::set_var("GRACE_ASCII", v),
            None => std::env::remove_var("GRACE_ASCII"),
        }
    }
}
