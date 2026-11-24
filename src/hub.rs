//! `grace hub`: one terminal window built on tmux.
//!
//! Each folder gets its own tmux window (a workspace). A window has a main pane with the
//! user's shell, and frameless panes around it: the sidebar (left), the diff (right top)
//! and the file tree (right bottom). Panes carry `@grace_role` and `@grace_meta`.
//!
//! grace runs tmux on its own socket (`tmux -L grace`) with its own config file, so the
//! user's tmux server and `~/.tmux.conf` are never touched.
use crate::config::Config;
use crate::db::Db;
use crate::platform;
use crate::theme::{Ansi, Theme};
use anyhow::{bail, Context, Result};
use crossterm::event::{self, Event, KeyEventKind};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The tmux session that holds all workspaces.
pub const SESSION: &str = "grace";
/// Field separator in tmux `-F` output.
const SEP: &str = "|g|";
const LAYOUT_KEY: &str = "hub.layout";
const SIZES_KEY: &str = "hub.sizes";

/// The tmux socket name: `GRACE_TMUX_SOCKET` (validated `[A-Za-z0-9._-]+`), else "grace".
pub fn socket() -> String {
    std::env::var("GRACE_TMUX_SOCKET")
        .ok()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
        .unwrap_or_else(|| SESSION.into())
}

// ---------------------------------------------------------------------------
// Roles, layout and sizes

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Main,
    Sidebar,
    Diff,
    Tree,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Main => "main",
            Role::Sidebar => "sidebar",
            Role::Diff => "diff",
            Role::Tree => "tree",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        [Role::Main, Role::Sidebar, Role::Diff, Role::Tree]
            .into_iter()
            .find(|r| r.as_str() == s)
    }
    /// The pane title in the border.
    fn title(self) -> &'static str {
        match self {
            Role::Sidebar => "grace",
            r => r.as_str(),
        }
    }
}

/// Which stateless panes show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub sidebar: bool,
    pub diff: bool,
    pub tree: bool,
}

impl Default for Layout {
    fn default() -> Layout {
        Layout {
            sidebar: true,
            diff: true,
            tree: true,
        }
    }
}

/// What a key of the hub asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    Sidebar,
    Diff,
    Tree,
    /// Diff and tree together.
    Right,
    Zoom,
    /// Go to the sidebar (bring it back if hidden).
    Focus,
    /// Cycle layout presets (`full` -> `code+diff` -> `sidebar+main` -> `zen`).
    Preset,
    /// Give the sidebar and the right column their saved widths again.
    Fit,
    /// Close the window when its main pane is gone.
    Reap,
}

impl Toggle {
    pub fn parse(s: &str) -> Option<Toggle> {
        Some(match s {
            "sidebar" => Toggle::Sidebar,
            "diff" => Toggle::Diff,
            "tree" => Toggle::Tree,
            "right" => Toggle::Right,
            "zoom" => Toggle::Zoom,
            "focus" => Toggle::Focus,
            "preset" => Toggle::Preset,
            "fit" => Toggle::Fit,
            "reap" => Toggle::Reap,
            _ => return None,
        })
    }
}

impl Layout {
    pub fn to_meta(self) -> String {
        let mut v = Vec::new();
        for (on, name) in [
            (self.sidebar, "sidebar"),
            (self.diff, "diff"),
            (self.tree, "tree"),
        ] {
            if on {
                v.push(name);
            }
        }
        if v.is_empty() {
            "none".into()
        } else {
            v.join(" ")
        }
    }

    pub fn from_meta(s: &str) -> Layout {
        let has = |n: &str| s.split_whitespace().any(|w| w == n);
        Layout {
            sidebar: has("sidebar"),
            diff: has("diff"),
            tree: has("tree"),
        }
    }

    pub fn from_preset(name: &str) -> Option<Layout> {
        match name.trim().to_ascii_lowercase().as_str() {
            "full" => Some(Layout {
                sidebar: true,
                diff: true,
                tree: true,
            }),
            "code+diff" => Some(Layout {
                sidebar: true,
                diff: true,
                tree: false,
            }),
            "sidebar+main" => Some(Layout {
                sidebar: true,
                diff: false,
                tree: false,
            }),
            "zen" => Some(Layout {
                sidebar: false,
                diff: false,
                tree: false,
            }),
            _ => None,
        }
    }

    pub fn next_preset(self) -> Layout {
        match (self.sidebar, self.diff, self.tree) {
            (true, true, true) => Layout {
                sidebar: true,
                diff: true,
                tree: false,
            },
            (true, true, false) => Layout {
                sidebar: true,
                diff: false,
                tree: false,
            },
            (true, false, false) => Layout {
                sidebar: false,
                diff: false,
                tree: false,
            },
            _ => Layout {
                sidebar: true,
                diff: true,
                tree: true,
            },
        }
    }

    pub fn toggled(self, t: Toggle) -> Layout {
        let mut l = self;
        match t {
            Toggle::Sidebar => l.sidebar = !l.sidebar,
            Toggle::Diff => l.diff = !l.diff,
            Toggle::Tree => l.tree = !l.tree,
            Toggle::Right => {
                let on = !(l.diff || l.tree);
                l.diff = on;
                l.tree = on;
            }
            Toggle::Focus => l.sidebar = true,
            Toggle::Preset => l = self.next_preset(),
            Toggle::Zoom | Toggle::Fit | Toggle::Reap => {}
        }
        l
    }

    /// What the panes of a window show now.
    pub fn of(panes: &[Pane]) -> Layout {
        let has = |r: Role| panes.iter().any(|p| p.role == Some(r));
        Layout {
            sidebar: has(Role::Sidebar),
            diff: has(Role::Diff),
            tree: has(Role::Tree),
        }
    }
}

/// Pane sizes, as tmux `-l` values. A pane that comes back gets its old size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sizes {
    pub sidebar: String,
    pub right: String,
    pub diff: u16,
}

impl Default for Sizes {
    fn default() -> Sizes {
        Sizes {
            sidebar: "30".into(),
            right: "40%".into(),
            diff: 60,
        }
    }
}

/// A tmux size: "30" or "40%".
fn valid_size(s: &str) -> bool {
    let n = s.strip_suffix('%').unwrap_or(s);
    !n.is_empty() && n.len() <= 4 && n.bytes().all(|b| b.is_ascii_digit()) && n != "0"
}

impl Sizes {
    pub fn to_meta(&self) -> String {
        format!(
            "sidebar={} right={} diff={}",
            self.sidebar, self.right, self.diff
        )
    }

    pub fn from_meta(s: &str) -> Sizes {
        let mut out = Sizes::default();
        for kv in s.split_whitespace() {
            match kv.split_once('=') {
                Some(("sidebar", v)) if valid_size(v) => out.sidebar = v.into(),
                Some(("right", v)) if valid_size(v) => out.right = v.into(),
                Some(("diff", v)) => {
                    if let Ok(n) = v.parse::<u16>() {
                        if (10..=90).contains(&n) {
                            out.diff = n;
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    pub fn remember(&self, panes: &[Pane], want: Layout, total: u16) -> Sizes {
        let mut s = self.clone();
        let find = |r: Role| panes.iter().find(|p| p.role == Some(r));
        let (diff, tree) = (find(Role::Diff), find(Role::Tree));
        let right = diff.or(tree);
        let sidebar = find(Role::Sidebar);
        let (fit_sb, fit_rt) = fit_widths(self, total, sidebar.is_some(), right.is_some());
        let moved = |w: u16, fit: u16| total == 0 || w.abs_diff(fit) > 1;
        if let Some(p) = sidebar {
            if !want.sidebar && p.width > 0 && moved(p.width, fit_sb) {
                s.sidebar = p.width.to_string();
            }
        }
        let right_goes = !want.diff && !want.tree;
        if right_goes {
            if let Some(p) = right {
                if p.width > 0 && moved(p.width, fit_rt) {
                    s.right = p.width.to_string();
                }
            }
        } else if let (Some(d), Some(t)) = (diff, tree) {
            if !want.diff || !want.tree {
                let total = d.height as u32 + t.height as u32 + 1;
                let pct = ((d.height as u32 * 100 + total / 2) / total.max(1)) as u16;
                if (10..=90).contains(&pct) && pct.abs_diff(self.diff) > 1 {
                    s.diff = pct;
                }
            }
        }
        s
    }
}

/// Read the stored layout and sizes.
pub fn load_layout(db: &Db) -> (Layout, Sizes) {
    let cfg_default = crate::config::load()
        .hub_layout
        .as_deref()
        .and_then(Layout::from_preset)
        .unwrap_or_default();
    let layout = db
        .get_meta(LAYOUT_KEY)
        .ok()
        .flatten()
        .map(|s| Layout::from_meta(&s))
        .unwrap_or(cfg_default);
    let sizes = db
        .get_meta(SIZES_KEY)
        .ok()
        .flatten()
        .map(|s| Sizes::from_meta(&s))
        .unwrap_or_default();
    (layout, sizes)
}

pub fn save_layout(db: &Db, layout: Layout, sizes: &Sizes) -> Result<()> {
    let layout_s = layout.to_meta();
    let sizes_s = sizes.to_meta();
    db.save_meta_batch(&[(LAYOUT_KEY, &layout_s), (SIZES_KEY, &sizes_s)])
}

// ---------------------------------------------------------------------------
// The planner: from the panes of a window to tmux operations

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub id: String,
    pub role: Option<Role>,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Pane(String),
    New(Role),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Kill(String),
    Split {
        target: Target,
        role: Role,
        horizontal: bool,
        before: bool,
        full: bool,
        size: String,
    },
    Width {
        target: Target,
        size: String,
    },
    Height {
        target: Target,
        size: String,
    },
}

pub fn main_pane(panes: &[Pane]) -> Option<&Pane> {
    panes
        .iter()
        .find(|p| p.role == Some(Role::Main))
        .or_else(|| panes.iter().find(|p| p.role.is_none()))
}

pub fn plan(panes: &[Pane], want: Layout, sizes: &Sizes) -> Vec<Op> {
    let Some(main) = main_pane(panes) else {
        return Vec::new();
    };
    let column_h = main.height;
    let main = Target::Pane(main.id.clone());
    let find = |r: Role| panes.iter().find(|p| p.role == Some(r));
    let mut ops = Vec::new();
    let mut kill = |r: Role, keep: bool| {
        if !keep {
            for p in panes.iter().filter(|p| p.role == Some(r)) {
                ops.push(Op::Kill(p.id.clone()));
            }
        }
    };
    kill(Role::Sidebar, want.sidebar);
    kill(Role::Diff, want.diff);
    kill(Role::Tree, want.tree);

    if want.sidebar && find(Role::Sidebar).is_none() {
        ops.push(Op::Split {
            target: main.clone(),
            role: Role::Sidebar,
            horizontal: true,
            before: true,
            full: true,
            size: sizes.sidebar.clone(),
        });
    }
    let diff = find(Role::Diff)
        .filter(|_| want.diff)
        .map(|p| Target::Pane(p.id.clone()));
    let tree = find(Role::Tree)
        .filter(|_| want.tree)
        .map(|p| Target::Pane(p.id.clone()));
    let column = |role| Op::Split {
        target: main.clone(),
        role,
        horizontal: true,
        before: false,
        full: true,
        size: sizes.right.clone(),
    };
    let height = |t: &Target| match t {
        Target::Pane(id) => panes
            .iter()
            .find(|p| p.id == *id)
            .map_or(column_h, |p| p.height),
        Target::New(_) => column_h,
    };
    let mut diff_now = diff.clone();
    if want.diff && diff.is_none() {
        ops.push(match &tree {
            Some(t) => Op::Split {
                target: t.clone(),
                role: Role::Diff,
                horizontal: false,
                before: true,
                full: false,
                size: share(height(t), sizes.diff).to_string(),
            },
            None => column(Role::Diff),
        });
        diff_now = Some(Target::New(Role::Diff));
    }
    let mut new_column = want.diff && diff.is_none() && tree.is_none();
    if want.tree && tree.is_none() {
        ops.push(match diff_now {
            Some(d) => {
                let h = height(&d);
                Op::Split {
                    target: d,
                    role: Role::Tree,
                    horizontal: false,
                    before: false,
                    full: false,
                    size: h
                        .saturating_sub(share(h, sizes.diff) + 1)
                        .max(1)
                        .to_string(),
                }
            }
            None => {
                new_column = true;
                column(Role::Tree)
            }
        });
    }
    let made_roles: Vec<Role> = ops
        .iter()
        .filter_map(|o| {
            if let Op::Split { role, .. } = o {
                Some(*role)
            } else {
                None
            }
        })
        .collect();
    let made = |r: Role| made_roles.contains(&r);
    if want.diff && want.tree && (made(Role::Diff) || made(Role::Tree)) {
        let total = find(Role::Tree)
            .or(find(Role::Diff))
            .filter(|_| !new_column)
            .map_or(column_h, |p| p.height);
        let target = diff.clone().unwrap_or(Target::New(Role::Diff));
        ops.push(Op::Height {
            target,
            size: share(total, sizes.diff).to_string(),
        });
    }
    if made(Role::Sidebar) && !new_column {
        if let Some(c) = diff.as_ref().or(tree.as_ref()) {
            let w = panes
                .iter()
                .find(|p| Target::Pane(p.id.clone()) == *c)
                .map_or(0, |p| p.width);
            if w > 0 {
                ops.push(Op::Width {
                    target: c.clone(),
                    size: w.to_string(),
                });
            }
        }
    }
    if new_column {
        let col = if made(Role::Diff) {
            Role::Diff
        } else {
            Role::Tree
        };
        if made(Role::Sidebar) {
            ops.push(Op::Width {
                target: Target::New(Role::Sidebar),
                size: sizes.sidebar.clone(),
            });
            ops.push(Op::Width {
                target: Target::New(col),
                size: sizes.right.clone(),
            });
        } else if let Some(sb) = find(Role::Sidebar).filter(|_| want.sidebar) {
            ops.push(Op::Width {
                target: Target::Pane(sb.id.clone()),
                size: sb.width.to_string(),
            });
        }
    }
    ops
}

fn share(total: u16, pct: u16) -> u16 {
    ((total as u32 * pct as u32 + 50) / 100).max(1) as u16
}

pub fn pane_command(exe: &str, role: Role, path: &str) -> Vec<String> {
    let sub: &[&str] = match role {
        Role::Main => &["hub", "shell"],
        Role::Sidebar => &["hub", "sidebar"],
        Role::Diff => &["diff"],
        Role::Tree => &["tree"],
    };
    let mut v = vec![exe.to_string()];
    v.extend(sub.iter().map(|s| s.to_string()));
    if role != Role::Main {
        v.push(path.to_string());
    }
    v
}

pub fn split_args(op: &Op, target: &str, exe: &str, path: &str) -> Vec<String> {
    let Op::Split {
        role,
        horizontal,
        before,
        full,
        size,
        ..
    } = op
    else {
        return Vec::new();
    };
    let mut a: Vec<String> = ["split-window", "-d", "-P", "-F", "#{pane_id}"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    a.push(if *horizontal { "-h" } else { "-v" }.into());
    if *before {
        a.push("-b".into());
    }
    if *full {
        a.push("-f".into());
    }
    a.extend([
        "-l".into(),
        size.clone(),
        "-t".into(),
        target.into(),
        "-c".into(),
        path.into(),
        "--".into(),
    ]);
    a.extend(pane_command(exe, *role, path));
    a
}

pub fn tag_args(pane: &str, role: Role) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut pairs = vec![("@grace_role", role.as_str())];
    if role != Role::Main {
        pairs.push(("@grace_title", role.title()));
    }
    for (k, v) in pairs {
        if !a.is_empty() {
            a.push(";".into());
        }
        a.extend(
            ["set-option", "-p", "-t", pane, k, v]
                .iter()
                .map(|s| s.to_string()),
        );
    }
    a
}

pub fn parse_panes(out: &str) -> Vec<Pane> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.split(SEP);
            let id = f.next()?.to_string();
            if !id.starts_with('%') {
                return None;
            }
            let role = Role::parse(f.next().unwrap_or(""));
            let width = f.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let height = f.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            Some(Pane {
                id,
                role,
                width,
                height,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// tmux.conf

/// The keys of the hub, after ctrl-g.
pub const KEYS: [(&str, &str, &str); 16] = [
    ("s", "sidebar", "hub toggle sidebar"),
    ("d", "diff", "hub toggle diff"),
    ("t", "tree", "hub toggle tree"),
    ("r", "right side", "hub toggle right"),
    ("l", "cycle layout", "hub toggle preset"),
    ("P", "fit widths", "hub toggle fit"),
    ("z", "zoom main", "hub toggle zoom"),
    ("g", "go to sidebar", "hub toggle focus"),
    ("n", "next workspace", ""),
    ("p", "prev workspace", ""),
    ("Tab", "last workspace", ""),
    ("w", "close workspace", ""),
    ("q", "detach (keeps running)", ""),
    ("Q", "quit the hub", ""),
    ("?", "this help", ""),
    ("C-g", "send ctrl-g", ""),
];

pub fn sh_quote(s: &str) -> String {
    platform::quote_posix(s)
}

/// Quote for a tmux config file, in double quotes. Rejects/replaces CR/LF/NUL.
fn tmux_quote(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '\n' | '\r' | '\0' => o.push(' '),
            '\\' | '"' | '$' => {
                o.push('\\');
                o.push(c);
            }
            _ => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Escape regex characters for a tmux `s|...|...|` format.
fn regex_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        if "\\.^$*+?()[]{}|".contains(c) {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

/// Parse `tmux -V`, for example "tmux 3.3a" or "tmux next-3.5". Unknown: None.
pub fn parse_version(s: &str) -> Option<(u32, u32)> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let rest = &s[start..];
    let (maj, rest) = rest.split_once('.')?;
    let min: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    Some((maj.parse().ok()?, min.parse().ok()?))
}

fn default_tmux_terminal() -> &'static str {
    let has_tmux_256 = Command::new("infocmp")
        .arg("tmux-256color")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if has_tmux_256 {
        "tmux-256color"
    } else {
        "screen-256color"
    }
}

/// Input of the config file.
pub struct ConfCtx<'a> {
    pub version: (u32, u32),
    pub exe: &'a str,
    pub socket: &'a str,
    pub home: Option<&'a str>,
    pub data_dir: Option<&'a str>,
    pub config_path: Option<&'a str>,
    pub theme: Option<&'a str>,
    pub ascii_env: Option<&'a str>,
    pub no_color: Option<&'a str>,
    pub truecolor: bool,
    pub light: bool,
    pub color: bool,
    pub ascii: bool,
    pub allow_passthrough: bool,
    pub default_terminal: &'a str,
}

/// The tmux.conf that grace writes at each start. Options that need a newer tmux are left out.
pub fn conf_text(c: &ConfCtx) -> String {
    let at_least = |maj: u32, min: u32| c.version >= (maj, min);
    let mut o = String::new();
    let mut line = |s: &str| {
        o.push_str(s);
        o.push('\n');
    };
    line("# Written by grace at each `grace hub` start. Edits here are lost.");
    line("# grace uses its own tmux server (tmux -L grace), so your ~/.tmux.conf stays as it is.");
    line("");
    line(&format!(
        "set -g default-terminal {}",
        tmux_quote(c.default_terminal)
    ));
    if c.truecolor && c.color {
        if at_least(3, 2) {
            line("set -as terminal-features \",*:RGB\"");
        } else {
            line("set -as terminal-overrides \",*:Tc\"");
        }
        line("set-environment -g COLORTERM truecolor");
    }
    if at_least(3, 2) {
        line("set -s extended-keys on");
    }
    if at_least(3, 3) && c.allow_passthrough {
        line("set -g allow-passthrough on");
    }
    line("set -g base-index 1");
    line("setw -g pane-base-index 1");
    line("set -g mouse on");
    line("set -s escape-time 10");
    line("set -g focus-events on");
    line("set -s set-clipboard on");
    line("set -g history-limit 50000");
    line("set -g renumber-windows on");
    line("set -g status-interval 1");
    line("set -g set-titles on");
    line("set -g set-titles-string \"grace · #W\"");
    line(&format!(
        "set-environment -g GRACE_TMUX_SOCKET {}",
        tmux_quote(c.socket)
    ));
    for (var, val) in [
        ("GRACE_DATA_DIR", c.data_dir),
        ("GRACE_CONFIG", c.config_path),
        ("GRACE_THEME", c.theme),
        ("GRACE_ASCII", c.ascii_env),
        ("NO_COLOR", c.no_color),
    ] {
        if let Some(v) = val {
            line(&format!("set-environment -g {var} {}", tmux_quote(v)));
        }
    }
    line("");
    let (accent, muted, dim_c, border_c, sel_fg, msg_bg) = if !c.color {
        (
            "default", "default", "default", "default", "default", "default",
        )
    } else {
        match (c.truecolor, c.light) {
            (true, true) => (
                "#b07908", "#5c6370", "#9ca3af", "#c8cdd8", "#ffffff", "#ece7da",
            ),
            (true, false) => (
                "#e8b94a", "#8b91a0", "#5c6370", "#3f4452", "#16171b", "#282c38",
            ),
            (false, true) => (
                "colour130",
                "colour242",
                "colour248",
                "colour250",
                "colour255",
                "colour254",
            ),
            (false, false) => (
                "yellow",
                "colour247",
                "colour241",
                "colour240",
                "black",
                "colour237",
            ),
        }
    };
    let star = if c.ascii { "*" } else { "✦" };
    let sep_bar = if c.ascii { "|" } else { "│" };
    let dot = if c.ascii { "|" } else { "·" };
    let lines_style = if c.ascii { "simple" } else { "single" };
    line("# Panes");
    line("set -g pane-border-status top");
    if at_least(3, 2) {
        line(&format!("set -g pane-border-lines {lines_style}"));
    }
    line(&format!("set -g pane-border-style \"fg={border_c}\""));
    line(&format!("set -g pane-active-border-style \"fg={accent}\""));
    let path = match c.home.filter(|h| {
        !h.is_empty()
            && !h
                .chars()
                .any(|ch| matches!(ch, '\'' | '#' | '\n' | '\r' | '\0' | ':' | ',' | '{' | '}'))
    }) {
        Some(h) => {
            let esc = regex_escape(h);
            format!(
                "#{{?#{{==:#{{pane_current_path}},{h}}},~,#{{s|^{esc}/|~/:pane_current_path}}}}"
            )
        }
        None => "#{pane_current_path}".into(),
    };
    let pane_hdr = format!("#{{?#{{@grace_title}},#{{@grace_title}},{path}}}");
    // Inside tmux #{?cond,true,false}, commas in #[...] styles must be escaped as #,
    line(&format!(
        "set -g pane-border-format '#{{?pane_active,#[fg={accent}#,bold] {star} {pane_hdr}#{{?#{{@grace_meta}}, #[nobold]{dot} #{{@grace_meta}},}} ,#[fg={muted}] {pane_hdr}#{{?#{{@grace_meta}}, #[fg={dim_c}]{dot} #{{@grace_meta}},}} }}'"
    ));
    line("");
    line("# Status bar");
    line("set -g status-position bottom");
    line("set -g status-style \"bg=default,fg=default\"");
    line(&format!(
        "set -g window-status-style \"bg=default,fg={muted},none\""
    ));
    line(&format!(
        "set -g window-status-current-style \"bg=default,fg={accent},bold,none\""
    ));
    let badge_style = if c.color {
        format!("bg={accent}#,fg={sel_fg}#,bold")
    } else {
        "reverse#,bold".into()
    };
    line(&format!(
        "set -g status-left '#{{?#{{==:#{{client_key_table}},grace}},#[{badge_style}] {star} CTRL-G #[bg=default] ,#[fg={accent}#,bold] {star} grace #[fg={border_c}]{sep_bar} }}'"
    ));
    line("set -g status-left-length 22");
    line(&format!(
        "set -g status-right '#{{?#{{==:#{{client_key_table}},grace}},#[fg={accent}#,bold]s#[fg={muted}#,nobold] side {dot} #[fg={accent}#,bold]d#[fg={muted}#,nobold] diff {dot} #[fg={accent}#,bold]t#[fg={muted}#,nobold] tree {dot} #[fg={accent}#,bold]l#[fg={muted}#,nobold] layout {dot} #[fg={accent}#,bold]P#[fg={muted}#,nobold] fit {dot} #[fg={accent}#,bold]z#[fg={muted}#,nobold] zoom {dot} #[fg={accent}#,bold]q#[fg={muted}#,nobold] detach {dot} #[fg={accent}#,bold]Q#[fg={muted}#,nobold] quit ,#[fg=default#,bold]^g #[fg={muted}#,nobold]? keys {dot} q detach {dot} Q quit }}'"
    ));
    line("set -g status-right-length 115");
    line("set -g window-status-separator \"\"");
    line("set -g window-status-format \" #I #W#{?#{@grace_glyph}, #{@grace_glyph},} \"");
    line("set -g window-status-current-format \" #I #W#{?#{@grace_glyph}, #{@grace_glyph},} \"");
    let msg_style = if c.color {
        format!("fg={accent},bg={msg_bg},bold")
    } else {
        "reverse,bold".into()
    };
    line(&format!("set -g message-style \"{msg_style}\""));
    line("");
    line("# Keys: ctrl-g, then one key. No prefix needed.");
    line("bind -n C-g switch-client -T grace");
    let exe = sh_quote(c.exe).replace('#', "##");
    let env = format!(
        "GRACE_TMUX_SOCKET={}",
        sh_quote(c.socket).replace('#', "##")
    );
    for (key, _, cmd) in KEYS {
        if cmd.is_empty() {
            continue;
        }
        let sh = format!("{env} {exe} {cmd} '#{{window_id}}'");
        line(&format!(
            "bind -T grace {key} run-shell -b {}",
            tmux_quote(&sh)
        ));
    }
    for idx in 1..=9 {
        line(&format!("bind -T grace {idx} select-window -t :{idx}"));
    }
    line("bind -T grace n next-window");
    line("bind -T grace p previous-window");
    line("bind -T grace Tab last-window");
    line("bind -T grace w confirm-before -p \"Close workspace '#W'? (y/n)\" kill-window");
    let fit = format!("{env} {exe} hub toggle fit '#{{window_id}}'");
    line(&format!(
        "set-hook -g window-resized {}",
        tmux_quote(&format!("run-shell -b {}", tmux_quote(&fit)))
    ));
    line("bind -T grace q detach-client");
    line("bind -T grace Q confirm-before -p \"Quit the hub and close every workspace? (y/n)\" \"kill-session -t =grace\"");
    let reap = format!("{env} {exe} hub toggle reap '#{{hook_window}}'");
    line(&format!(
        "set-hook -g pane-exited {}",
        tmux_quote(&format!("run-shell -b {}", tmux_quote(&reap)))
    ));
    line("bind -T grace C-g send-keys C-g");
    if at_least(3, 2) {
        let sh = format!("{env} {exe} hub keys --wait");
        let border = if c.ascii { "" } else { "-b rounded " };
        let style = if at_least(3, 3) {
            format!("{border}-S \"fg={accent}\" -T \" {star} grace keys \" ")
        } else {
            String::new()
        };
        line(&format!(
            "bind -T grace ? display-popup -E -w 46 -h 25 {style}{}",
            tmux_quote(&sh)
        ));
    } else {
        line(&format!(
            "bind -T grace ? display-message {}",
            tmux_quote(&keys_line())
        ));
    }
    o
}

pub fn keys_line() -> String {
    let v: Vec<String> = KEYS.iter().map(|(k, l, _)| format!("{k} {l}")).collect();
    format!("ctrl-g, then: {}", v.join(" · "))
}

// ---------------------------------------------------------------------------
// Running tmux

pub const WINDOWS_MSG: &str = "grace hub needs tmux, and tmux does not run on Windows.\n\
Run grace inside WSL to use the hub. grace diff and grace tree work here.";

pub const INSTALL_MSG: &str = concat!(
    "grace hub needs tmux 3.2 or newer, and tmux is not installed.\n",
    "Install it, then run grace hub again:\n",
    "  macOS          brew install tmux\n",
    "  Debian/Ubuntu  sudo apt install tmux\n",
    "  Fedora         sudo dnf install tmux\n",
    "  Arch           sudo pacman -S tmux\n",
    "  FreeBSD        pkg install tmux\n",
    "  Alpine         apk add tmux\n",
    "On Windows, use grace inside WSL."
);

pub fn inside_hub(tmux_env: Option<&str>, socket: &str) -> bool {
    let Some(v) = tmux_env.filter(|v| !v.is_empty()) else {
        return false;
    };
    let path = v.split(',').next().unwrap_or("");
    Path::new(path).file_name().is_some_and(|n| n == socket)
}

pub struct Tmux {
    pub socket: String,
}

impl Tmux {
    pub fn new() -> Tmux {
        Tmux { socket: socket() }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new("tmux");
        c.arg("-L")
            .arg(&self.socket)
            .env_remove("TMUX")
            .stdin(Stdio::null());
        c
    }

    pub fn run<S: AsRef<std::ffi::OsStr>>(&self, args: &[S]) -> Result<String> {
        let out = self.cmd().args(args).output().context("cannot run tmux")?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            bail!("tmux: {err}");
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    }

    pub fn ok(&self, args: &[&str]) -> bool {
        self.cmd()
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    pub fn panes(&self, window: &str) -> Result<Vec<Pane>> {
        let f = "#{pane_id}|g|#{@grace_role}|g|#{pane_width}|g|#{pane_height}";
        Ok(parse_panes(&self.run(&[
            "list-panes",
            "-t",
            window,
            "-F",
            f,
        ])?))
    }

    /// Workspaces as `(window_id, folder, glyph)`.
    pub fn windows_and_glyphs(&self) -> Result<Vec<(String, String, String)>> {
        let out = self.run(&[
            "list-windows",
            "-t",
            &format!("={SESSION}"),
            "-F",
            "#{window_id}|g|#{@grace_path}|g|#{@grace_glyph}",
        ])?;
        Ok(out
            .lines()
            .filter_map(|l| {
                let mut parts = l.split(SEP);
                let w = parts.next()?.to_string();
                let p = parts.next()?.to_string();
                if p.is_empty() {
                    return None;
                }
                let g = parts.next().unwrap_or("").to_string();
                Some((w, p, g))
            })
            .collect())
    }

    /// Workspaces as (window id, folder).
    pub fn windows(&self) -> Result<Vec<(String, String)>> {
        Ok(self
            .windows_and_glyphs()?
            .into_iter()
            .map(|(w, p, _)| (w, p))
            .collect())
    }

    pub fn current_window(&self) -> Result<String> {
        let pane = std::env::var("TMUX_PANE").context("not inside the grace hub")?;
        self.run(&["display-message", "-p", "-t", &pane, "#{window_id}"])
    }

    pub fn message(&self, msg: &str) {
        let safe = platform::sanitize_display(msg).replace('#', "##");
        let _ = self.run(&["display-message", "--", &safe]);
    }
}

pub fn socket_exists(socket: &str) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `geteuid()` has no preconditions and always succeeds.
        let uid = unsafe { libc::geteuid() };
        let base = platform::env_non_empty("TMUX_TMPDIR").unwrap_or_else(|| "/tmp".to_string());
        Path::new(&base)
            .join(format!("tmux-{uid}"))
            .join(socket)
            .exists()
    }
    #[cfg(not(unix))]
    {
        let _ = socket;
        false
    }
}

/// Update `@grace_meta` on the current hub pane only when it changed.
pub fn set_pane_meta(cache: &mut String, meta: &str) {
    let clean = platform::sanitize_display(meta).into_owned();
    if *cache == clean {
        return;
    }
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return;
    };
    if !inside_hub(std::env::var("TMUX").ok().as_deref(), &socket()) {
        return;
    }
    let t = Tmux::new();
    let res = if clean.is_empty() {
        t.run(&["set-option", "-p", "-u", "-t", &pane, "@grace_meta"])
    } else {
        t.run(&["set-option", "-p", "-t", &pane, "@grace_meta", &clean])
    };
    if res.is_ok() {
        *cache = clean;
    }
}

/// Returns `(window_active, session_attached)` for the current hub pane, or `(true, true)` outside hub.
pub fn pane_activity() -> (bool, bool) {
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return (true, true);
    };
    if !inside_hub(std::env::var("TMUX").ok().as_deref(), &socket()) {
        return (true, true);
    }
    let Ok(out) = Tmux::new().run(&[
        "display-message",
        "-p",
        "-t",
        &pane,
        "#{window_active}|g|#{session_attached}",
    ]) else {
        return (true, true);
    };
    let mut parts = out.trim().split(SEP);
    let win_active = parts.next().is_some_and(|v| v == "1");
    let attached = parts.next().is_some_and(|v| v != "0");
    (win_active, attached)
}

/// Send literal `text` to the main pane of the current hub window (used by TreeView `i` key).
pub fn paste_to_main(text: &str) -> bool {
    if text.is_empty() || text.chars().any(platform::is_unsafe_char) {
        return false;
    }
    let t = Tmux::new();
    let Ok(win) = t.current_window() else {
        return false;
    };
    let Ok(panes) = t.panes(&win) else {
        return false;
    };
    let Some(main) = main_pane(&panes) else {
        return false;
    };
    t.run(&["send-keys", "-t", &main.id, "-l", "--", text])
        .is_ok()
}

/// tmux version, or a friendly error when tmux is missing.
pub fn tmux_version() -> Result<(u32, u32)> {
    if cfg!(windows) {
        bail!(WINDOWS_MSG);
    }
    let out = match Command::new("tmux").arg("-V").output() {
        Ok(o) => o,
        Err(_) => bail!(INSTALL_MSG),
    };
    if !out.status.success() {
        bail!(INSTALL_MSG);
    }
    let v = String::from_utf8_lossy(&out.stdout);
    let ver = parse_version(&v).unwrap_or((99, 0));
    if ver < (3, 2) {
        bail!("grace hub needs tmux 3.2 or newer. You have {}.", v.trim());
    }
    Ok(ver)
}

fn term_size() -> (u16, u16) {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        if let Ok(s) = crossterm::terminal::size() {
            return s;
        }
    }
    let env = |k: &str, d: u16| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or(d)
    };
    (env("COLUMNS", 200), env("LINES", 50))
}

fn exe() -> Result<String> {
    let p = platform::stable_exe().context("cannot find the grace program")?;
    Ok(p.to_string_lossy().into_owned())
}

pub fn apply(
    t: &Tmux,
    window: &str,
    path: &str,
    exe: &str,
    want: Layout,
    sizes: &Sizes,
) -> Result<()> {
    let panes = t.panes(window)?;
    let ops = plan(&panes, want, sizes);
    let mut made: Vec<(Role, String)> = Vec::new();
    let mut kill_batch: Vec<String> = Vec::new();
    for op in &ops {
        if let Op::Kill(id) = op {
            if !kill_batch.is_empty() {
                kill_batch.push(";".into());
            }
            kill_batch.extend(["kill-pane".into(), "-t".into(), id.clone()]);
        }
    }
    if !kill_batch.is_empty() {
        let _ = t.run(&kill_batch);
    }
    let mut resize_batch: Vec<String> = Vec::new();
    for op in &ops {
        match op {
            Op::Kill(_) => {}
            Op::Width { target, size } | Op::Height { target, size } => {
                let flag = if matches!(op, Op::Width { .. }) {
                    "-x"
                } else {
                    "-y"
                };
                let id = match target {
                    Target::Pane(id) => Some(id.clone()),
                    Target::New(r) => made.iter().find(|(m, _)| m == r).map(|(_, id)| id.clone()),
                };
                if let Some(id) = id {
                    if !resize_batch.is_empty() {
                        resize_batch.push(";".into());
                    }
                    resize_batch.extend([
                        "resize-pane".into(),
                        "-t".into(),
                        id,
                        flag.into(),
                        size.clone(),
                    ]);
                }
            }
            Op::Split { target, role, .. } => {
                let target = match target {
                    Target::Pane(id) => id.clone(),
                    Target::New(r) => match made.iter().find(|(m, _)| m == r) {
                        Some((_, id)) => id.clone(),
                        None => continue,
                    },
                };
                let Ok(id) = t.run(&split_args(op, &target, exe, path)) else {
                    continue;
                };
                let _ = t.run(&tag_args(&id, *role));
                made.push((*role, id));
            }
        }
    }
    if !resize_batch.is_empty() {
        let _ = t.run(&resize_batch);
    }
    if let Ok(panes) = t.panes(window) {
        fit(t, window, &panes, sizes);
    }
    Ok(())
}

fn window_name(path: &str) -> String {
    let raw = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    platform::sanitize_display(&raw).into_owned()
}

fn setup_window(
    t: &Tmux,
    ids: &str,
    path: &str,
    exe: &str,
    layout: Layout,
    sizes: &Sizes,
) -> Result<(String, String)> {
    let (win, pane) = ids.split_once(SEP).context("unexpected tmux output")?;
    let mut opt_args: Vec<String> = ["set-option", "-w", "-t", win, "@grace_path", path]
        .iter()
        .map(|s| s.to_string())
        .collect();
    opt_args.push(";".into());
    opt_args.extend(tag_args(pane, Role::Main));
    t.run(&opt_args)?;
    apply(t, win, path, exe, layout, sizes)?;
    Ok((win.to_string(), pane.to_string()))
}

pub fn switch(t: &Tmux, db: &Db, path: &str, resume: Option<&str>) -> Result<String> {
    if !Path::new(path).is_dir() {
        bail!("folder does not exist: {path}");
    }
    let existing = t
        .windows()?
        .into_iter()
        .find(|(_, p)| platform::same_path(p, path))
        .map(|(w, _)| w);
    let fresh = existing.is_none();
    let (win, known_main) = match existing {
        Some(w) => (w, None),
        None => {
            let (layout, sizes) = load_layout(db);
            let exe = exe()?;
            let mut args: Vec<String> = [
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{window_id}|g|#{pane_id}",
                "-t",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            args.extend([
                format!("={SESSION}:"),
                "-n".into(),
                window_name(path),
                "-c".into(),
                path.into(),
                "--".into(),
            ]);
            let mut cmd_vec = pane_command(&exe, Role::Main, path);
            if let Some(run_cmd) = resume {
                cmd_vec.push(format!("--run={run_cmd}"));
            }
            args.extend(cmd_vec);
            let ids = t.run(&args)?;
            let (w, m) = setup_window(t, &ids, path, &exe, layout, &sizes)?;
            (w, Some(m))
        }
    };
    t.run(&["select-window", "-t", &win])?;
    let main = match known_main {
        Some(m) => Some(m),
        None => main_pane(&t.panes(&win)?).map(|p| p.id.clone()),
    };
    if let Some(main) = &main {
        t.run(&["select-pane", "-t", main])?;
        if let Some(cmd) = resume.filter(|_| !fresh) {
            let cur = t
                .run(&[
                    "display-message",
                    "-p",
                    "-t",
                    main,
                    "#{pane_current_command}",
                ])
                .unwrap_or_default();
            if is_shell(&cur) {
                t.run(&["send-keys", "-t", main, "-l", "--", cmd])?;
                t.run(&["send-keys", "-t", main, "Enter"])?;
            } else {
                t.message("a program already runs in this workspace");
            }
        }
    }
    Ok(win)
}

/// True for a shell name, as tmux shows in pane_current_command (excludes `grace` itself!).
pub fn is_shell(cmd: &str) -> bool {
    let c = cmd.trim().trim_start_matches('-');
    let me = std::env::var("SHELL").ok().and_then(|s| {
        Path::new(&s)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    });
    matches!(
        c,
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "nu" | "pwsh"
    ) || me.as_deref() == Some(c)
}

/// `grace hub [path]`: open the hub, or switch the workspace when already inside.
pub fn start(db: &Db, path: &str, attach: bool) -> Result<()> {
    let version = tmux_version()?;
    if !Path::new(path).is_dir() {
        bail!("folder does not exist: {path}");
    }
    let t = Tmux::new();
    if inside_hub(std::env::var("TMUX").ok().as_deref(), &t.socket) {
        switch(&t, db, path, None)?;
        return Ok(());
    }
    if std::env::var_os("TMUX").is_some_and(|v| !v.is_empty()) && attach {
        eprintln!("grace: you are inside tmux. The hub opens nested, on its own tmux server.");
    }
    let dir = crate::db::data_dir()?;
    platform::create_dir_private(&dir)?;
    let conf = dir.join("tmux.conf");
    let home = platform::home_dir().map(|h| platform::clean_path_str(&h.to_string_lossy()));
    let data_dir = platform::env_non_empty("GRACE_DATA_DIR");
    let config_path = platform::env_non_empty("GRACE_CONFIG");
    let theme_env = platform::env_non_empty("GRACE_THEME");
    let ascii_env = platform::env_non_empty("GRACE_ASCII");
    let no_color = platform::env_non_empty("NO_COLOR");
    let exe = exe()?;
    let cfg = crate::config::load();
    let theme = Theme::from_env_and_config(&cfg);
    let allow_pt = std::env::var("GRACE_TMUX_PASSTHROUGH").is_ok_and(|v| v == "1");
    let text = conf_text(&ConfCtx {
        version,
        exe: &exe,
        socket: &t.socket,
        home: home.as_deref(),
        data_dir: data_dir.as_deref(),
        config_path: config_path.as_deref(),
        theme: theme_env.as_deref(),
        ascii_env: ascii_env.as_deref(),
        no_color: no_color.as_deref(),
        truecolor: theme.truecolor,
        light: theme.light,
        color: theme.color,
        ascii: theme.ascii,
        allow_passthrough: allow_pt,
        default_terminal: default_tmux_terminal(),
    });
    write_conf_atomic(&conf, &text)?;
    let conf_s = conf.to_string_lossy().into_owned();

    if t.ok(&["has-session", "-t", &format!("={SESSION}")]) {
        t.run(&["source-file", &conf_s])?;
        switch(&t, db, path, None)?;
    } else {
        let (w, h) = term_size();
        let (layout, sizes) = load_layout(db);
        let mut args: Vec<String> = [
            "-f",
            &conf_s,
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-P",
            "-F",
            "#{window_id}|g|#{pane_id}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        args.extend(["-x".into(), w.to_string(), "-y".into(), h.to_string()]);
        args.extend([
            "-n".into(),
            window_name(path),
            "-c".into(),
            path.into(),
            "--".into(),
        ]);
        args.extend(pane_command(&exe, Role::Main, path));
        let ids = t.run(&args)?;
        let (_win, main_pane_id) = setup_window(&t, &ids, path, &exe, layout, &sizes)?;
        let _ = t.run(&["select-pane", "-t", &main_pane_id]);
    }
    if !attach {
        return Ok(());
    }
    let status = t
        .cmd()
        .stdin(Stdio::inherit())
        .args(["attach-session", "-t", &format!("={SESSION}")])
        .status()?;
    if !t.ok(&["has-session", "-t", &format!("={SESSION}")]) {
        eprintln!("grace: the hub stopped.");
        return Ok(());
    }
    if !status.success() {
        bail!("tmux attach failed");
    }
    eprintln!("grace: detached. The hub and its shells keep running.");
    eprintln!("  grace hub        go back");
    eprintln!("  grace hub quit   stop the hub");
    Ok(())
}

fn write_conf_atomic(path: &Path, content: &str) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().context("bad tmux.conf path")?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!("tmux.conf.tmp-{}-{nanos}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(content.as_bytes())?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    platform::restrict_file_0600(path);
    Ok(())
}

/// `grace hub quit`: stop the hub server. This closes every workspace.
pub fn quit() -> Result<()> {
    if cfg!(windows) {
        bail!(WINDOWS_MSG);
    }
    let t = Tmux::new();
    if !t.ok(&["has-session", "-t", &format!("={SESSION}")]) {
        let _ = t.run(&["kill-server"]);
        println!("grace: the hub is not running.");
        return Ok(());
    }
    t.run(&["kill-session", "-t", &format!("={SESSION}")])?;
    let _ = t.run(&["kill-server"]);
    println!("grace: the hub stopped.");
    Ok(())
}

/// True when this process is the hub's own `role` pane ("diff" or "tree").
pub fn pane_in_hub(role: &str) -> bool {
    if !matches!(role, "diff" | "tree" | "sidebar") {
        return false;
    }
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return false;
    };
    if !inside_hub(std::env::var("TMUX").ok().as_deref(), &socket()) {
        return false;
    }
    Tmux::new()
        .run(&["display-message", "-p", "-t", &pane, "#{@grace_role}"])
        .is_ok_and(|r| r.trim() == role)
}

/// After `grace diff` or `grace tree` ends in the hub, hide the pane cleanly.
pub fn pane_closed(in_hub: bool, role: Role) -> Result<()> {
    if !in_hub {
        return Ok(());
    }
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return Ok(());
    };
    let t = Tmux::new();
    let Ok(win) = t.run(&["display-message", "-p", "-t", &pane, "#{window_id}"]) else {
        return Ok(());
    };
    let exe = exe()?;
    let sh = format!(
        "GRACE_TMUX_SOCKET={} {} hub toggle {} {}",
        sh_quote(&t.socket),
        sh_quote(&exe),
        role.as_str(),
        sh_quote(win.trim())
    );
    if t.run(&["run-shell", "-b", &sh.replace('#', "##")]).is_ok() {
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    Ok(())
}

/// `grace hub toggle <what>`: run by the ctrl-g keys. Errors show as a tmux message.
pub fn toggle(db: &Db, what: &str, window: Option<&str>) -> Result<()> {
    if cfg!(windows) {
        bail!(WINDOWS_MSG);
    }
    let t = Tmux::new();
    let _lock = toggle_lock();
    if let Err(e) = toggle_inner(&t, db, what, window) {
        t.message(&format!("grace: {e}"));
    }
    Ok(())
}

fn toggle_inner(t: &Tmux, db: &Db, what: &str, window: Option<&str>) -> Result<()> {
    let Some(what) = Toggle::parse(what) else {
        bail!("unknown toggle {what}: use sidebar, diff, tree, right, preset, zoom or focus");
    };
    let win = match window.filter(|w| !w.is_empty()) {
        Some(w) => w.to_string(),
        None => t.current_window()?,
    };
    if what == Toggle::Reap {
        if let Ok(panes) = t.panes(&win) {
            if !panes.is_empty() && main_pane(&panes).is_none() {
                let _ = t.run(&["kill-window", "-t", &win]);
            }
        }
        return Ok(());
    }
    let panes = t.panes(&win)?;
    let path = t.run(&["display-message", "-p", "-t", &win, "#{@grace_path}"])?;
    match what {
        Toggle::Reap => {}
        Toggle::Zoom => {
            let main = main_pane(&panes).context("no main pane here")?;
            t.run(&["resize-pane", "-Z", "-t", &main.id])?;
            let (is_zoomed, total) = window_metrics(t, &win);
            if !is_zoomed && total > 0 {
                let (_, sizes) = load_layout(db);
                fit_with_total(t, &t.panes(&win)?, &sizes, total);
            }
        }
        Toggle::Fit => {
            let (_, sizes) = load_layout(db);
            fit(t, &win, &panes, &sizes);
        }
        Toggle::Focus => {
            if Layout::of(&panes).sidebar {
                let sb = panes
                    .iter()
                    .find(|p| p.role == Some(Role::Sidebar))
                    .unwrap();
                t.run(&["select-pane", "-t", &sb.id])?;
            } else {
                let exe = exe()?;
                let (_, sizes) = load_layout(db);
                let want = Layout::of(&panes).toggled(Toggle::Focus);
                apply(t, &win, &path, &exe, want, &sizes)?;
                save_layout(db, want, &sizes)?;
                for (w, p) in t.windows()? {
                    if w != win {
                        let _ = apply(t, &w, &p, &exe, want, &sizes);
                    }
                }
                if let Some(sb) = t
                    .panes(&win)?
                    .iter()
                    .find(|p| p.role == Some(Role::Sidebar))
                {
                    t.run(&["select-pane", "-t", &sb.id])?;
                }
            }
        }
        _ => {
            let exe = exe()?;
            let (_, sizes) = load_layout(db);
            let want = Layout::of(&panes).toggled(what);
            let (_, total) = window_metrics(t, &win);
            let sizes = sizes.remember(&panes, want, total);
            save_layout(db, want, &sizes)?;
            apply(t, &win, &path, &exe, want, &sizes)?;
            for (w, p) in t.windows()? {
                if w != win {
                    let _ = apply(t, &w, &p, &exe, want, &sizes);
                }
            }
        }
    }
    Ok(())
}

fn cols(size: &str, total: u16) -> u16 {
    match size.strip_suffix('%') {
        Some(p) => (p.parse::<u32>().unwrap_or(0) * total as u32 / 100) as u16,
        None => size.parse().unwrap_or(0),
    }
}

pub fn fit_widths(sizes: &Sizes, total: u16, sidebar: bool, right: bool) -> (u16, u16) {
    let mut sb = if sidebar {
        cols(&sizes.sidebar, total).max(1)
    } else {
        0
    };
    let mut rt = if right {
        cols(&sizes.right, total).max(1)
    } else {
        0
    };
    let room = total.saturating_sub(total * 2 / 5 + 2);
    if sb + rt > room && sb + rt > 0 {
        let k = room as u32;
        let sum = (sb + rt) as u32;
        sb = (sb as u32 * k / sum) as u16;
        rt = (rt as u32 * k / sum) as u16;
    }
    (sb, rt)
}

fn window_metrics(t: &Tmux, win: &str) -> (bool, u16) {
    let Ok(out) = t.run(&[
        "display-message",
        "-p",
        "-t",
        win,
        "#{window_zoomed_flag}|g|#{window_width}",
    ]) else {
        return (false, 0);
    };
    let mut parts = out.trim().split(SEP);
    let zoomed = parts.next().is_some_and(|v| v == "1");
    let width = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    (zoomed, width)
}

fn toggle_lock() -> Option<std::fs::File> {
    let dir = crate::db::data_dir().ok()?;
    platform::create_dir_private(&dir).ok()?;
    let lock_path = dir.join("hub.lock");
    if std::fs::symlink_metadata(&lock_path).is_ok_and(|m| m.file_type().is_symlink()) {
        return None;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts.open(&lock_path).ok()?;
    platform::restrict_file_0600(&lock_path);
    f.lock().ok()?;
    Some(f)
}

fn fit_with_total(t: &Tmux, panes: &[Pane], sizes: &Sizes, total: u16) {
    let sb = panes.iter().find(|p| p.role == Some(Role::Sidebar));
    let rt = panes
        .iter()
        .find(|p| matches!(p.role, Some(Role::Diff) | Some(Role::Tree)));
    let (w_sb, w_rt) = fit_widths(sizes, total, sb.is_some(), rt.is_some());
    let mut args: Vec<String> = Vec::new();
    if let Some(p) = sb {
        args.extend([
            "resize-pane".into(),
            "-t".into(),
            p.id.clone(),
            "-x".into(),
            w_sb.max(1).to_string(),
        ]);
    }
    if let Some(p) = rt {
        if !args.is_empty() {
            args.push(";".into());
        }
        args.extend([
            "resize-pane".into(),
            "-t".into(),
            p.id.clone(),
            "-x".into(),
            w_rt.max(1).to_string(),
        ]);
    }
    if !args.is_empty() {
        let _ = t.run(&args);
    }
}

fn fit(t: &Tmux, win: &str, panes: &[Pane], sizes: &Sizes) {
    let (zoomed, total) = window_metrics(t, win);
    if zoomed || total == 0 {
        return;
    }
    fit_with_total(t, panes, sizes, total);
}

/// `grace hub shell [--run CMD]`: the main pane. Runs an optional initial command inside the
/// interactive shell, then closes the workspace when the shell exits.
pub fn shell(run_cmd: Option<&str>) -> Result<()> {
    if cfg!(windows) {
        bail!(WINDOWS_MSG);
    }
    let raw_sh = platform::env_non_empty("SHELL")
        .filter(|s| Path::new(s).exists())
        .unwrap_or_else(|| "/bin/sh".into());
    let sh_name = Path::new(&raw_sh)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sh".into());
    if let Some(cmd) = run_cmd.filter(|c| !c.trim().is_empty()) {
        let flag = match sh_name.as_str() {
            "zsh" | "bash" | "fish" => "-lic",
            _ => "-lc",
        };
        let _ = Command::new(&raw_sh).args([flag, cmd]).status();
    }
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut c = Command::new(&raw_sh);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.arg0(format!("-{sh_name}"));
    }
    let _ = c.status();
    if let Ok(pane) = std::env::var("TMUX_PANE") {
        let _ = Tmux::new().run(&["kill-window", "-t", &pane]);
    }
    Ok(())
}

/// The status glyph for the status bar, with tmux styles.
pub fn glyph(s: crate::model::Status, frame: usize, ascii: bool, color: bool) -> String {
    use crate::model::Status;
    let wrap = |fg: &str, ch: &str| -> String {
        if color {
            format!("#[fg={fg}]{ch}#[default]")
        } else {
            ch.into()
        }
    };
    match s {
        Status::Waiting => wrap("yellow,bold", if ascii { "!" } else { "●" }),
        Status::Ready => (if ascii { "o" } else { "○" }).into(),
        Status::Working => {
            let f = if ascii {
                crate::theme::SPINNER_ASCII[frame % crate::theme::SPINNER_ASCII.len()]
            } else {
                crate::theme::SPINNER[frame % crate::theme::SPINNER.len()]
            };
            let mut s_buf = [0u8; 4];
            wrap("cyan", f.encode_utf8(&mut s_buf))
        }
        Status::Done => wrap("green", if ascii { "+" } else { "✓" }),
    }
}

/// Sync `@grace_glyph` on hub windows if a hub tmux session exists.
pub fn sync_glyphs_if_running(db: &Db, cfg: Option<&Config>) {
    if cfg!(windows) {
        return;
    }
    let t = Tmux::new();
    if cfg!(unix) && !socket_exists(&t.socket) {
        return;
    }
    let Ok(rows) = t.windows_and_glyphs() else {
        return;
    };
    let now = crate::model::now();
    let sessions = db.sessions(now).unwrap_or_default();
    let loaded;
    let cfg = match cfg {
        Some(c) => c,
        None => {
            loaded = crate::config::load();
            &loaded
        }
    };
    let theme = Theme::from_env_and_config(cfg);
    sync_glyphs_rows(
        &t,
        &rows,
        &crate::picker::best_by_path(&sessions),
        0,
        theme.ascii,
        theme.color,
    );
}

fn sync_glyphs_rows(
    t: &Tmux,
    rows: &[(String, String, String)],
    best: &crate::picker::Best,
    frame: usize,
    ascii: bool,
    color: bool,
) {
    let mut args: Vec<String> = Vec::new();
    for (w, p, have) in rows {
        let want = best
            .get(p)
            .map(|s| glyph(s.status, frame, ascii, color))
            .unwrap_or_default();
        if p.is_empty() || want == *have {
            continue;
        }
        if !args.is_empty() {
            args.push(";".into());
        }
        if want.is_empty() {
            args.extend(
                ["set-option", "-w", "-u", "-t", w, "@grace_glyph"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        } else {
            args.extend(
                ["set-option", "-w", "-t", w, "@grace_glyph"]
                    .iter()
                    .map(|s| s.to_string()),
            );
            args.push(want);
        }
    }
    if !args.is_empty() {
        let _ = t.run(&args);
    }
}

/// `grace hub sidebar <path>`: the sidebar pane of the workspace for `path`.
pub fn run_sidebar(
    db: &Db,
    reg: &crate::tools::Registry,
    ignore: &platform::IgnoreMatcher,
    path: &str,
) -> Result<()> {
    use crate::picker::{best_by_path, Best, Data};
    let t = Tmux::new();
    let cfg = crate::config::load();
    let theme = Theme::from_env_and_config(&cfg);
    let ascii = theme.ascii;
    let color = theme.color;

    let initial_rows = t.windows_and_glyphs().unwrap_or_default();
    let initial_hub_paths: Vec<String> = initial_rows.iter().map(|(_, p, _)| p.clone()).collect();
    let now = crate::model::now();
    let initial_entries = crate::load(db, reg, ignore, None).unwrap_or_default();
    let initial_sessions = db.sessions(now).unwrap_or_default();
    let initial_data = Data {
        entries: initial_entries,
        sessions: initial_sessions,
        editors: Vec::new(),
        branches: HashMap::new(),
    };

    let mut cached_data = initial_data.clone();
    let mut last_ver = db.data_version();
    let mut last_hub_paths = initial_hub_paths.clone();
    let mut last_best: Best = best_by_path(&initial_data.sessions);
    let mut last_idle_bg = Instant::now();
    let mut frame = 0usize;

    let reload = Box::new(move || -> Option<(Data, Vec<String>)> {
        let (win_active, attached) = pane_activity();
        if (!win_active || !attached) && last_idle_bg.elapsed() < Duration::from_secs(15) {
            return None;
        }
        last_idle_bg = Instant::now();

        let ver = db.data_version();
        let rows = t.windows_and_glyphs().unwrap_or_default();
        let hub_paths: Vec<String> = rows.iter().map(|(_, p, _)| p.clone()).collect();
        let db_changed = ver != last_ver;
        let hubs_changed = hub_paths != last_hub_paths;

        frame = frame.wrapping_add(1);
        let has_working = last_best
            .values()
            .any(|s| s.status == crate::model::Status::Working);
        if !db_changed && !hubs_changed {
            if win_active && attached && has_working {
                sync_glyphs_rows(&t, &rows, &last_best, frame, ascii, color);
            }
            return None;
        }

        if db_changed {
            let now = crate::model::now();
            let entries = crate::load(db, reg, ignore, None).ok()?;
            let sessions = db.sessions(now).unwrap_or_default();
            last_ver = ver;
            last_best = best_by_path(&sessions);
            cached_data = Data {
                entries,
                sessions,
                editors: Vec::new(),
                branches: HashMap::new(),
            };
        }
        last_hub_paths = hub_paths.clone();
        if win_active && attached {
            sync_glyphs_rows(&t, &rows, &last_best, frame, ascii, color);
        }
        Some((cached_data.clone(), hub_paths))
    });
    let t_go = Tmux::new();
    let go = Box::new(
        move |e: &crate::model::Entry, resume: bool| -> std::result::Result<(), String> {
            let cmd = if resume {
                let fresh = crate::tools::Registry::from_db(db).ok();
                let reg = fresh.as_ref().unwrap_or(reg);
                let cmd = e
                    .tags
                    .iter()
                    .filter(|t| reg.is_ai_tag(&t.tool))
                    .find_map(|t| reg.launch_command(&t.tool));
                Some(cmd.ok_or("no AI tool used here")?)
            } else {
                None
            };
            switch(&t_go, db, &e.path, cmd.as_deref())
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
    );
    let home = platform::home_dir().map(|h| platform::clean_path_str(&h.to_string_lossy()));
    let mut sb = crate::sidebar::Sidebar::new(
        initial_data,
        initial_hub_paths,
        path,
        theme,
        home,
        reload,
        go,
    );
    crate::tui::run(&mut sb)
}

struct RawGuard;

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// `grace hub keys`: the help popup.
pub fn keys(wait: bool) -> Result<()> {
    if cfg!(windows) {
        bail!(WINDOWS_MSG);
    }
    let cfg = crate::config::load();
    let theme = Theme::from_env_and_config(&cfg);
    let a = Ansi::stdout(&cfg);
    println!();
    println!("  {}{} grace hub keys{}", a.gold, theme.star(), a.reset);
    println!("  {}ctrl-g, then one key{}", a.dim, a.reset);
    println!();
    for (k, l, _) in KEYS {
        println!("  {}{k:<5}{} {l}", a.gold, a.reset);
    }
    if !wait {
        return Ok(());
    }
    println!();
    print!("  {}press any key{}", a.dim, a.reset);
    use std::io::Write;
    std::io::stdout().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let _guard = RawGuard;
    loop {
        match event::read() {
            Ok(Event::Key(k)) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, role: Option<Role>, w: u16, h: u16) -> Pane {
        Pane {
            id: id.into(),
            role,
            width: w,
            height: h,
        }
    }

    fn full() -> Vec<Pane> {
        vec![
            pane("%1", Some(Role::Main), 100, 50),
            pane("%2", Some(Role::Sidebar), 30, 50),
            pane("%3", Some(Role::Diff), 70, 30),
            pane("%4", Some(Role::Tree), 70, 19),
        ]
    }

    #[test]
    fn plan_new_window_adds_all_panes() {
        let ops = plan(
            &[pane("%1", Some(Role::Main), 200, 50)],
            Layout::default(),
            &Sizes::default(),
        );
        assert_eq!(ops.len(), 6);
        assert_eq!(
            ops[3],
            Op::Height {
                target: Target::New(Role::Diff),
                size: "30".into()
            }
        );
        assert_eq!(
            ops[4],
            Op::Width {
                target: Target::New(Role::Sidebar),
                size: "30".into()
            }
        );
        assert_eq!(
            ops[5],
            Op::Width {
                target: Target::New(Role::Diff),
                size: "40%".into()
            }
        );
        assert_eq!(
            ops[0],
            Op::Split {
                target: Target::Pane("%1".into()),
                role: Role::Sidebar,
                horizontal: true,
                before: true,
                full: true,
                size: "30".into()
            }
        );
    }

    #[test]
    fn plan_toggles() {
        let s = Sizes::default();
        let l = Layout::of(&full());
        assert_eq!(l, Layout::default());
        assert_eq!(
            plan(&full(), l.toggled(Toggle::Sidebar), &s),
            vec![Op::Kill("%2".into())]
        );
        let r = l.toggled(Toggle::Right);
        assert_eq!((r.diff, r.tree), (false, false));
        assert_eq!(
            plan(&full(), r, &s),
            vec![Op::Kill("%3".into()), Op::Kill("%4".into())]
        );
        assert_eq!(r.toggled(Toggle::Right), l);
        assert!(plan(&[pane("%2", Some(Role::Sidebar), 30, 50)], l, &s).is_empty());
    }

    #[test]
    fn split_and_tag_args() {
        let op = Op::Split {
            target: Target::Pane("%1".into()),
            role: Role::Diff,
            horizontal: true,
            before: false,
            full: true,
            size: "40%".into(),
        };
        let a = split_args(&op, "%1", "/bin/grace", "/p q");
        assert_eq!(
            a.join(" "),
            "split-window -d -P -F #{pane_id} -h -f -l 40% -t %1 -c /p q -- /bin/grace diff /p q"
        );
        let t = tag_args("%5", Role::Sidebar);
        assert_eq!(
            t.join(" "),
            "set-option -p -t %5 @grace_role sidebar ; set-option -p -t %5 @grace_title grace"
        );
    }

    #[test]
    fn sizes_are_remembered() {
        let s = Sizes::default();
        let l = Layout::default();
        let mut panes = full();
        panes[1].width = 26;
        assert_eq!(
            s.remember(&panes, l.toggled(Toggle::Sidebar), 0).sidebar,
            "26"
        );
        assert_eq!(s.remember(&panes, l.toggled(Toggle::Right), 0).right, "70");
        assert_eq!(s.remember(&panes, l.toggled(Toggle::Diff), 0).diff, 60);
    }

    #[test]
    fn shrunk_sizes_are_not_remembered() {
        let s = Sizes::default();
        let l = Layout::default();
        let (sb, rt) = fit_widths(&s, 80, true, true);
        let mut panes = full();
        panes[1].width = sb;
        panes[2].width = rt;
        panes[3].width = rt;
        assert_eq!(
            s.remember(&panes, l.toggled(Toggle::Sidebar), 80).sidebar,
            "30"
        );
        assert_eq!(
            s.remember(&panes, l.toggled(Toggle::Right), 80).right,
            "40%"
        );
    }

    #[test]
    fn layout_round_trip() {
        for l in [
            Layout::default(),
            Layout {
                sidebar: false,
                diff: true,
                tree: false,
            },
            Layout {
                sidebar: false,
                diff: false,
                tree: false,
            },
        ] {
            assert_eq!(Layout::from_meta(&l.to_meta()), l);
        }
        let s = Sizes {
            sidebar: "24".into(),
            right: "35%".into(),
            diff: 55,
        };
        assert_eq!(Sizes::from_meta(&s.to_meta()), s);
        assert_eq!(
            Sizes::from_meta("sidebar=x right=0 diff=99; rm"),
            Sizes::default()
        );

        let d = tempfile::tempdir().unwrap();
        let db = Db::open(&d.path().join("g.db")).unwrap();
        assert_eq!(load_layout(&db), (Layout::default(), Sizes::default()));
        let l = Layout {
            sidebar: true,
            diff: false,
            tree: true,
        };
        save_layout(&db, l, &s).unwrap();
        assert_eq!(load_layout(&db), (l, s));
    }

    #[test]
    fn panes_parse() {
        let p =
            parse_panes("%1|g|main|g|100|g|50\n%2|g||g|80|g|20\nbad line\n%3|g|sidebar|g|30|g|50");
        assert_eq!(p.len(), 3);
        assert_eq!(p[1].role, None);
        assert_eq!(p[2], pane("%3", Some(Role::Sidebar), 30, 50));
        assert_eq!(main_pane(&p[1..]).unwrap().id, "%2");
    }

    fn conf(v: (u32, u32), truecolor: bool, allow_passthrough: bool) -> String {
        conf_text(&ConfCtx {
            version: v,
            exe: "/opt/my tools/grace",
            socket: "grace",
            home: Some("/Users/me"),
            data_dir: None,
            config_path: None,
            theme: None,
            ascii_env: None,
            no_color: None,
            truecolor,
            light: false,
            color: true,
            ascii: false,
            allow_passthrough,
            default_terminal: "tmux-256color",
        })
    }

    #[test]
    fn conf_has_keys_and_style() {
        let c = conf((3, 8), true, false);
        assert!(c.contains("bind -n C-g switch-client -T grace"));
        for k in ["s", "d", "t", "r", "l", "P", "z", "g"] {
            assert!(
                c.contains(&format!("bind -T grace {k} run-shell -b ")),
                "{k}"
            );
        }
        assert!(c.contains("bind -T grace q detach-client"));
        assert!(c.contains("bind -T grace Q confirm-before"));
        assert!(c.contains("set -g base-index 1"));
        assert!(c.contains("window-status-current-style"));
        assert!(c.contains("client_key_table"));
        assert!(c.contains("set-hook -g pane-exited"));
        assert!(c.contains("bind -T grace ? display-popup -E"));
        assert!(c.contains(
            "GRACE_TMUX_SOCKET=grace '/opt/my tools/grace' hub toggle sidebar '#{window_id}'"
        ));
        assert!(c.contains("pane-active-border-style \"fg=#e8b94a\""));
        assert!(c.contains("set -as terminal-features \",*:RGB\""));
        assert!(!c.contains("allow-passthrough"));
        assert!(c.contains("#{s|^/Users/me/|~/:pane_current_path}"));
        // Inside tmux #{?cond,true,false}, commas inside #[...] style specs must be escaped (#,)
        assert!(c.contains("#[fg=#e8b94a#,bold]"));
        assert!(c.contains("#[bg=#e8b94a#,fg=#16171b#,bold]"));
        assert!(!c.contains("#[fg=#e8b94a,bold]"));
        assert!(c.contains("display-popup -E -w 46 -h 25"));
    }

    #[test]
    fn conf_degrades_on_old_tmux_and_non_truecolor() {
        let c = conf((3, 0), false, false);
        assert!(!c.contains("pane-border-lines"));
        assert!(!c.contains("terminal-features"));
        assert!(!c.contains("COLORTERM truecolor"));
        assert!(!c.contains("display-popup"));
        assert!(c.contains("bind -T grace ? display-message"));
        let c = conf((3, 3), true, true);
        assert!(c.contains("allow-passthrough on"));

        let ascii_no_color = conf_text(&ConfCtx {
            version: (3, 4),
            exe: "/bin/grace",
            socket: "grace",
            home: None,
            data_dir: None,
            config_path: Some("/tmp/cfg.toml"),
            theme: Some("light"),
            ascii_env: Some("1"),
            no_color: None,
            truecolor: true,
            light: false,
            color: false,
            ascii: true,
            allow_passthrough: false,
            default_terminal: "tmux-256color",
        });
        assert!(ascii_no_color.contains("pane-border-lines simple"));
        assert!(!ascii_no_color.contains("pane-border-lines ascii"));
        assert!(!ascii_no_color.contains("fg=#") && !ascii_no_color.contains("colour"));
        assert!(ascii_no_color.contains("set-environment -g GRACE_CONFIG"));
        assert!(ascii_no_color.contains("set-environment -g GRACE_THEME"));
        assert!(ascii_no_color.contains("set-environment -g GRACE_ASCII"));
        assert_eq!(glyph(crate::model::Status::Done, 0, true, false), "+");
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(tmux_quote(r#"a"b$c\d"#), r#""a\"b\$c\\d""#);
        assert_eq!(tmux_quote("a\nb"), "\"a b\"");
        assert_eq!(regex_escape("/a.b"), r"/a\.b");
    }

    #[test]
    fn versions_and_socket() {
        assert_eq!(parse_version("tmux 3.8"), Some((3, 8)));
        assert_eq!(parse_version("tmux 3.3a"), Some((3, 3)));
        assert_eq!(parse_version("tmux next-3.6"), Some((3, 6)));
        assert_eq!(parse_version("tmux master"), None);
        assert!(inside_hub(
            Some("/private/tmp/tmux-501/grace,123,0"),
            "grace"
        ));
        assert!(!inside_hub(
            Some("/private/tmp/tmux-501/default,123,0"),
            "grace"
        ));
        assert!(!inside_hub(Some(""), "grace"));
        assert!(!inside_hub(None, "grace"));
    }

    #[test]
    fn toggles_parse() {
        assert_eq!(Toggle::parse("right"), Some(Toggle::Right));
        assert_eq!(Toggle::parse("preset"), Some(Toggle::Preset));
        assert_eq!(Toggle::parse("nope"), None);
        assert!(is_shell("zsh") && is_shell("-bash") && !is_shell("claude") && !is_shell("grace"));
        assert!(keys_line().contains("s sidebar"));
    }

    #[test]
    fn fit_keeps_room_for_main() {
        let d = Sizes::default();
        assert_eq!(fit_widths(&d, 200, true, true), (30, 80));
        let (sb, rt) = fit_widths(&d, 80, true, true);
        assert!(80 - sb - rt >= 32, "{sb} {rt}");
        assert_eq!(fit_widths(&d, 200, false, true), (0, 80));
    }
}
