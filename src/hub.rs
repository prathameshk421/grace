//! `grace hub`: one terminal window built on tmux.
//!
//! Each folder gets its own tmux window (a workspace). A window has a main pane with the
//! user's shell, and stateless panes around it: the sidebar (left), the diff (right top)
//! and the file tree (right bottom). The stateless panes carry the pane option
//! `@grace_role`, so `grace hub toggle` can find them, kill them and split them back.
//!
//! grace runs tmux on its own socket (`tmux -L grace`) with its own config file, so the
//! user's tmux server and `~/.tmux.conf` are never touched.
use crate::db::Db;
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

/// The tmux session that holds all workspaces.
pub const SESSION: &str = "grace";
/// Field separator in tmux `-F` output. tmux 3.6 and newer print a tab as "_",
/// so grace uses printable text that no pane id, role or number contains.
const SEP: &str = "|g|";
const LAYOUT_KEY: &str = "hub.layout";
const SIZES_KEY: &str = "hub.sizes";

/// The tmux socket name: `GRACE_TMUX_SOCKET`, else "grace".
pub fn socket() -> String {
    std::env::var("GRACE_TMUX_SOCKET").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| SESSION.into())
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
        [Role::Main, Role::Sidebar, Role::Diff, Role::Tree].into_iter().find(|r| r.as_str() == s)
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
        Layout { sidebar: true, diff: true, tree: true }
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
    /// Give the sidebar and the right column their saved widths again. A tmux hook
    /// runs it when the window changes size, so the main pane gets the new space.
    Fit,
    /// Close the window when its main pane is gone. A tmux hook runs it.
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
            "fit" => Toggle::Fit,
            "reap" => Toggle::Reap,
            _ => return None,
        })
    }
}

impl Layout {
    /// Stored as the visible pane names, for example "sidebar tree", or "none".
    pub fn to_meta(self) -> String {
        let mut v = Vec::new();
        for (on, name) in [(self.sidebar, "sidebar"), (self.diff, "diff"), (self.tree, "tree")] {
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
        Layout { sidebar: has("sidebar"), diff: has("diff"), tree: has("tree") }
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
            Toggle::Zoom | Toggle::Fit | Toggle::Reap => {}
        }
        l
    }

    /// What the panes of a window show now.
    pub fn of(panes: &[Pane]) -> Layout {
        let has = |r: Role| panes.iter().any(|p| p.role == Some(r));
        Layout { sidebar: has(Role::Sidebar), diff: has(Role::Diff), tree: has(Role::Tree) }
    }
}

/// Pane sizes, as tmux `-l` values. A pane that comes back gets its old size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sizes {
    /// Sidebar width.
    pub sidebar: String,
    /// Width of the right column (diff and tree).
    pub right: String,
    /// Diff height in percent of the right column.
    pub diff: u16,
}

impl Default for Sizes {
    fn default() -> Sizes {
        Sizes { sidebar: "30".into(), right: "40%".into(), diff: 60 }
    }
}

/// A tmux size: "30" or "40%".
fn valid_size(s: &str) -> bool {
    let n = s.strip_suffix('%').unwrap_or(s);
    !n.is_empty() && n.len() <= 4 && n.bytes().all(|b| b.is_ascii_digit()) && n != "0"
}

impl Sizes {
    pub fn to_meta(&self) -> String {
        format!("sidebar={} right={} diff={}", self.sidebar, self.right, self.diff)
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

    /// Sizes after the panes not in `want` go away: remember their current size.
    /// A width is kept only when the user changed it. A pane that a small window
    /// shrank keeps its saved size, so it comes back full size in a big window.
    /// `total` is the window width, or 0 when it is not known.
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
                // Rounding moves the split by a row: that is not a change.
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
    let layout = db.get_meta(LAYOUT_KEY).ok().flatten().map(|s| Layout::from_meta(&s)).unwrap_or_default();
    let sizes = db.get_meta(SIZES_KEY).ok().flatten().map(|s| Sizes::from_meta(&s)).unwrap_or_default();
    (layout, sizes)
}

pub fn save_layout(db: &Db, layout: Layout, sizes: &Sizes) -> Result<()> {
    db.set_meta(LAYOUT_KEY, &layout.to_meta())?;
    db.set_meta(SIZES_KEY, &sizes.to_meta())
}

// ---------------------------------------------------------------------------
// The planner: from the panes of a window to tmux operations

/// One pane of a window, from `list-panes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub id: String,
    pub role: Option<Role>,
    pub width: u16,
    pub height: u16,
}

/// The pane to split: one that exists, or one an earlier step makes.
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
        /// Side by side (-h), else one above the other (-v).
        horizontal: bool,
        /// Put the new pane left of or above the target (-b).
        before: bool,
        /// Span the full window height or width (-f).
        full: bool,
        size: String,
    },
    /// Set the width of a pane. Later full-height splits shrink earlier panes, so
    /// the planner sets the widths again at the end.
    Width { target: Target, size: String },
    /// Set the height of a pane. tmux rounds splits, so the diff height is set at the end.
    Height { target: Target, size: String },
}

/// The main pane: the one with role main, else the first pane without a role.
pub fn main_pane(panes: &[Pane]) -> Option<&Pane> {
    panes.iter().find(|p| p.role == Some(Role::Main)).or_else(|| panes.iter().find(|p| p.role.is_none()))
}

/// Operations that make the window show `want`. Panes keep their place:
/// the sidebar on the left, the diff on the right top, the tree below it.
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
    let diff = find(Role::Diff).filter(|_| want.diff).map(|p| Target::Pane(p.id.clone()));
    let tree = find(Role::Tree).filter(|_| want.tree).map(|p| Target::Pane(p.id.clone()));
    let column = |role| Op::Split {
        target: main.clone(),
        role,
        horizontal: true,
        before: false,
        full: true,
        size: sizes.right.clone(),
    };
    let height = |t: &Target| match t {
        Target::Pane(id) => panes.iter().find(|p| p.id == *id).map_or(column_h, |p| p.height),
        Target::New(_) => column_h,
    };
    let mut diff_now = diff.clone();
    if want.diff && diff.is_none() {
        ops.push(match &tree {
            // Above the tree, in the same column.
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
                    size: h.saturating_sub(share(h, sizes.diff) + 1).max(1).to_string(),
                }
            }
            None => {
                new_column = true;
                column(Role::Tree)
            }
        });
    }
    let made_roles: Vec<Role> = ops.iter().filter_map(|o| if let Op::Split { role, .. } = o { Some(*role) } else { None }).collect();
    let made = |r: Role| made_roles.contains(&r);
    // The diff gets its share of the column, the tree the rest.
    if want.diff && want.tree && (made(Role::Diff) || made(Role::Tree)) {
        let total = find(Role::Tree).or(find(Role::Diff)).filter(|_| !new_column).map_or(column_h, |p| p.height);
        let target = diff.clone().unwrap_or(Target::New(Role::Diff));
        ops.push(Op::Height { target, size: share(total, sizes.diff).to_string() });
    }
    // A full-height split shrinks the other panes: set the old widths again.
    if made(Role::Sidebar) && !new_column {
        if let Some(c) = diff.as_ref().or(tree.as_ref()) {
            let w = panes.iter().find(|p| Target::Pane(p.id.clone()) == *c).map_or(0, |p| p.width);
            if w > 0 {
                ops.push(Op::Width { target: c.clone(), size: w.to_string() });
            }
        }
    }
    if new_column {
        let col = if made(Role::Diff) { Role::Diff } else { Role::Tree };
        if made(Role::Sidebar) {
            ops.push(Op::Width { target: Target::New(Role::Sidebar), size: sizes.sidebar.clone() });
            ops.push(Op::Width { target: Target::New(col), size: sizes.right.clone() });
        } else if let Some(sb) = find(Role::Sidebar).filter(|_| want.sidebar) {
            ops.push(Op::Width { target: Target::Pane(sb.id.clone()), size: sb.width.to_string() });
        }
    }
    ops
}

/// Rows of the diff in a column of `total` rows: `pct` percent, at least 1.
fn share(total: u16, pct: u16) -> u16 {
    ((total as u32 * pct as u32 + 50) / 100).max(1) as u16
}

/// The command a stateless pane runs.
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

/// tmux arguments for a split. `target` is the pane id to split.
pub fn split_args(op: &Op, target: &str, exe: &str, path: &str) -> Vec<String> {
    let Op::Split { role, horizontal, before, full, size, .. } = op else {
        return Vec::new();
    };
    let mut a: Vec<String> = ["split-window", "-d", "-P", "-F", "#{pane_id}"].iter().map(|s| s.to_string()).collect();
    a.push(if *horizontal { "-h" } else { "-v" }.into());
    if *before {
        a.push("-b".into());
    }
    if *full {
        a.push("-f".into());
    }
    a.extend(["-l".into(), size.clone(), "-t".into(), target.into(), "-c".into(), path.into(), "--".into()]);
    a.extend(pane_command(exe, *role, path));
    a
}

/// tmux arguments that tag a new pane with its role and title.
pub fn tag_args(pane: &str, role: Role) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut pairs = vec![("@grace_role", role.as_str())];
    // The main pane shows its folder, not a title.
    if role != Role::Main {
        pairs.push(("@grace_title", role.title()));
    }
    for (k, v) in pairs {
        if !a.is_empty() {
            a.push(";".into());
        }
        a.extend(["set-option", "-p", "-t", pane, k, v].iter().map(|s| s.to_string()));
    }
    a
}

/// Parse `list-panes -F "#{pane_id}|g|#{@grace_role}|g|#{pane_width}|g|#{pane_height}"`.
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
            Some(Pane { id, role, width, height })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// tmux.conf

/// The keys of the hub, after ctrl-g.
pub const KEYS: [(&str, &str, &str); 10] = [
    ("s", "sidebar", "hub toggle sidebar"),
    ("d", "diff", "hub toggle diff"),
    ("t", "tree", "hub toggle tree"),
    ("r", "right side", "hub toggle right"),
    ("z", "zoom main", "hub toggle zoom"),
    ("g", "go to sidebar", "hub toggle focus"),
    ("q", "detach (keeps running)", ""),
    ("Q", "quit the hub", ""),
    ("?", "this help", ""),
    ("C-g", "send ctrl-g", ""),
];

/// Quote for /bin/sh.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Quote for a tmux config file, in double quotes. `#` doubles, because
/// run-shell expands formats.
fn tmux_quote(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
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

/// Input of the config file.
pub struct ConfCtx<'a> {
    pub version: (u32, u32),
    pub exe: &'a str,
    pub socket: &'a str,
    pub home: Option<&'a str>,
    pub data_dir: Option<&'a str>,
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
    line("set -g default-terminal \"tmux-256color\"");
    if at_least(3, 2) {
        line("set -as terminal-features \",*:RGB\"");
        line("set -s extended-keys on");
    } else {
        line("set -as terminal-overrides \",*:Tc\"");
    }
    if at_least(3, 3) {
        line("set -g allow-passthrough on");
    }
    line("set -g mouse on");
    line("set -s escape-time 10");
    line("set -g focus-events on");
    line("set -s set-clipboard on");
    line("set -g history-limit 50000");
    line("set -g renumber-windows on");
    line("set -g status-interval 1");
    line("set -g set-titles on");
    line("set -g set-titles-string \"grace · #W\"");
    line("set-environment -g COLORTERM truecolor");
    line(&format!("set-environment -g GRACE_TMUX_SOCKET {}", tmux_quote(c.socket)));
    if let Some(d) = c.data_dir {
        line(&format!("set-environment -g GRACE_DATA_DIR {}", tmux_quote(d)));
    }
    line("");
    line("# Panes");
    line("set -g pane-border-status top");
    if at_least(3, 2) {
        line("set -g pane-border-lines single");
    }
    line("set -g pane-border-style \"fg=colour240\"");
    line("set -g pane-active-border-style \"fg=#e8b94a\"");
    // The main pane shows its folder, with ~ for home. The other panes show their title.
    let path = match c.home.filter(|h| !h.is_empty() && !h.contains('\'') && !h.contains('#')) {
        Some(h) => format!("#{{s|^{}|~|:pane_current_path}}", regex_escape(h)),
        None => "#{pane_current_path}".into(),
    };
    line(&format!(
        "set -g pane-border-format ' #{{?#{{@grace_title}},#[bold]#{{@grace_title}}#[nobold],{path}}} '"
    ));
    line("");
    line("# Status bar");
    line("set -g status-position bottom");
    line("set -g status-style \"bg=default,fg=colour250\"");
    line("set -g status-left \"#[fg=#e8b94a,bold] grace #[default]\"");
    line("set -g status-left-length 12");
    line("set -g status-right \"#[fg=colour244]ctrl-g: ? keys · q detach · Q quit \"");
    line("set -g status-right-length 60");
    line("set -g window-status-separator \"\"");
    line("set -g window-status-format \" #[fg=colour244]#I#[default] #W#{?#{@grace_glyph}, #{@grace_glyph},} \"");
    line("set -g window-status-current-format \" #[fg=#e8b94a,bold]#I #W#[default]#{?#{@grace_glyph}, #{@grace_glyph},} \"");
    line("set -g message-style \"fg=#e8b94a,bg=default\"");
    line("");
    line("# Keys: ctrl-g, then one key. No prefix needed.");
    line("bind -n C-g switch-client -T grace");
    let exe = sh_quote(c.exe).replace('#', "##");
    let env = format!("GRACE_TMUX_SOCKET={}", sh_quote(c.socket).replace('#', "##"));
    for (key, _, cmd) in KEYS {
        if cmd.is_empty() {
            continue;
        }
        let sh = format!("{env} {exe} {cmd} '#{{window_id}}'");
        line(&format!("bind -T grace {key} run-shell -b {}", tmux_quote(&sh)));
    }
    // tmux scales every pane when the window changes size. Keep the side panes at
    // their saved widths, so the main pane gets the new space.
    let fit = format!("{env} {exe} hub toggle fit '#{{window_id}}'");
    line(&format!("set-hook -g window-resized {}", tmux_quote(&format!("run-shell -b {}", tmux_quote(&fit)))));
    line("bind -T grace q detach-client");
    line("bind -T grace Q confirm-before -p \"Quit the hub and close every workspace? (y/n)\" \"kill-session -t =grace\"");
    // A main pane can die without its shell wrapper (kill -9, a crash). Close the
    // window then, so no side panes stay behind with nothing in the middle.
    let reap = format!("{env} {exe} hub toggle reap '#{{hook_window}}'");
    line(&format!("set-hook -g pane-exited {}", tmux_quote(&format!("run-shell -b {}", tmux_quote(&reap)))));
    line("bind -T grace C-g send-keys C-g");
    if at_least(3, 2) {
        let sh = format!("{env} {exe} hub keys --wait");
        let title = if at_least(3, 3) { "-T \" grace keys \" " } else { "" };
        line(&format!("bind -T grace ? display-popup -E -w 38 -h 16 {title}{}", tmux_quote(&sh)));
    } else {
        line(&format!("bind -T grace ? display-message {}", tmux_quote(&keys_line())));
    }
    o
}

/// All keys on one line, for old tmux and for messages.
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
    "On Windows, use grace inside WSL."
);

/// True if `$TMUX` points at the grace socket. `$TMUX` looks like "/tmp/tmux-501/grace,123,0".
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

    /// A tmux command on the grace socket. TMUX is unset, so a grace hub opens
    /// inside another tmux too. Every command names its target.
    fn cmd(&self) -> Command {
        let mut c = Command::new("tmux");
        c.arg("-L").arg(&self.socket).env_remove("TMUX").stdin(Stdio::null());
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
        self.cmd().args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    }

    pub fn panes(&self, window: &str) -> Result<Vec<Pane>> {
        let f = "#{pane_id}|g|#{@grace_role}|g|#{pane_width}|g|#{pane_height}";
        Ok(parse_panes(&self.run(&["list-panes", "-t", window, "-F", f])?))
    }

    /// Workspaces as (window id, folder).
    pub fn windows(&self) -> Result<Vec<(String, String)>> {
        let out = self.run(&["list-windows", "-t", &format!("={SESSION}"), "-F", "#{window_id}|g|#{@grace_path}"])?;
        Ok(out
            .lines()
            .filter_map(|l| l.split_once(SEP))
            .filter(|(_, p)| !p.is_empty())
            .map(|(w, p)| (w.to_string(), p.to_string()))
            .collect())
    }

    /// The window of this process's pane.
    pub fn current_window(&self) -> Result<String> {
        let pane = std::env::var("TMUX_PANE").context("not inside the grace hub")?;
        self.run(&["display-message", "-p", "-t", &pane, "#{window_id}"])
    }

    pub fn message(&self, msg: &str) {
        let _ = self.run(&["display-message", msg]);
    }
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
    let v = String::from_utf8_lossy(&out.stdout);
    // Builds from git say "tmux master" or "tmux next-3.6": treat as new.
    let ver = parse_version(&v).unwrap_or((99, 0));
    if ver < (3, 0) {
        bail!("grace hub needs tmux 3.2 or newer. You have {}.", v.trim());
    }
    Ok(ver)
}

/// Size for a new session: the terminal, else COLUMNS and LINES, else 200x50.
fn term_size() -> (u16, u16) {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        if let Ok(s) = crossterm::terminal::size() {
            return s;
        }
    }
    let env = |k: &str, d: u16| std::env::var(k).ok().and_then(|v| v.parse().ok()).filter(|&n| n > 0).unwrap_or(d);
    (env("COLUMNS", 200), env("LINES", 50))
}

fn exe() -> Result<String> {
    let p = std::env::current_exe().context("cannot find the grace program")?;
    Ok(p.to_string_lossy().into_owned())
}

/// Make the window show `want`. Errors of single steps are skipped.
pub fn apply(t: &Tmux, window: &str, path: &str, want: Layout, sizes: &Sizes) -> Result<()> {
    let panes = t.panes(window)?;
    let exe = exe()?;
    let mut made: Vec<(Role, String)> = Vec::new();
    for op in plan(&panes, want, sizes) {
        match &op {
            Op::Kill(id) => {
                let _ = t.run(&["kill-pane", "-t", id]);
            }
            Op::Width { target, size } | Op::Height { target, size } => {
                let flag = if matches!(op, Op::Width { .. }) { "-x" } else { "-y" };
                let id = match target {
                    Target::Pane(id) => Some(id.clone()),
                    Target::New(r) => made.iter().find(|(m, _)| m == r).map(|(_, id)| id.clone()),
                };
                if let Some(id) = id {
                    let _ = t.run(&["resize-pane", "-t", &id, flag, size]);
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
                let Ok(id) = t.run(&split_args(&op, &target, &exe, path)) else {
                    continue;
                };
                let _ = t.run(&tag_args(&id, *role));
                made.push((*role, id));
            }
        }
    }
    // The plan uses the saved sizes. In a small window, shrink the side panes so the
    // main pane keeps its 40%.
    if let Ok(panes) = t.panes(window) {
        fit(t, window, &panes, sizes);
    }
    Ok(())
}

fn window_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

/// Tag a new window and its main pane, then add the stateless panes.
fn setup_window(t: &Tmux, ids: &str, path: &str, layout: Layout, sizes: &Sizes) -> Result<String> {
    let (win, pane) = ids.split_once(SEP).context("unexpected tmux output")?;
    t.run(&["set-option", "-w", "-t", win, "@grace_path", path])?;
    t.run(&tag_args(pane, Role::Main))?;
    apply(t, win, path, layout, sizes)?;
    // In a small window the saved sizes may not leave room for the main pane.
    fit(t, win, &t.panes(win)?, sizes);
    Ok(win.to_string())
}

/// Select the workspace for `path`, or make it. With `resume`, type that command
/// in the main pane when its shell is idle. Returns the window id.
pub fn switch(t: &Tmux, db: &Db, path: &str, resume: Option<&str>) -> Result<String> {
    let existing = t.windows()?.into_iter().find(|(_, p)| crate::platform::same_path(p, path)).map(|(w, _)| w);
    let fresh = existing.is_none();
    let win = match existing {
        Some(w) => w,
        None => {
            let (layout, sizes) = load_layout(db);
            let mut args: Vec<String> = ["new-window", "-d", "-P", "-F", "#{window_id}|g|#{pane_id}", "-t"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            args.extend([format!("={SESSION}:"), "-n".into(), window_name(path), "-c".into(), path.into(), "--".into()]);
            args.extend(pane_command(&exe()?, Role::Main, path));
            let ids = t.run(&args)?;
            setup_window(t, &ids, path, layout, &sizes)?
        }
    };
    t.run(&["select-window", "-t", &win])?;
    let main = main_pane(&t.panes(&win)?).map(|p| p.id.clone());
    if let Some(main) = &main {
        t.run(&["select-pane", "-t", main])?;
        if let Some(cmd) = resume {
            let idle = fresh || {
                let cur = t.run(&["display-message", "-p", "-t", main, "#{pane_current_command}"]).unwrap_or_default();
                is_shell(&cur)
            };
            if idle {
                t.run(&["send-keys", "-t", main, "-l", cmd])?;
                t.run(&["send-keys", "-t", main, "Enter"])?;
            } else {
                t.message("a program already runs in this workspace");
            }
        }
    }
    Ok(win)
}

/// True for a shell name, as tmux shows in pane_current_command.
pub fn is_shell(cmd: &str) -> bool {
    let c = cmd.trim().trim_start_matches('-');
    let me = std::env::var("SHELL").ok().and_then(|s| Path::new(&s).file_name().map(|n| n.to_string_lossy().into_owned()));
    matches!(c, "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "nu" | "pwsh" | "grace") || me.as_deref() == Some(c)
}

/// `grace hub [path]`: open the hub, or switch the workspace when already inside.
pub fn start(db: &Db, path: &str, attach: bool) -> Result<()> {
    let version = tmux_version()?;
    let t = Tmux::new();
    if inside_hub(std::env::var("TMUX").ok().as_deref(), &t.socket) {
        switch(&t, db, path, None)?;
        return Ok(());
    }
    if std::env::var_os("TMUX").is_some_and(|v| !v.is_empty()) && attach {
        eprintln!("grace: you are inside tmux. The hub opens nested, on its own tmux server.");
    }
    let dir = crate::db::data_dir()?;
    std::fs::create_dir_all(&dir)?;
    let conf = dir.join("tmux.conf");
    let home = crate::platform::home_dir().map(|h| crate::platform::clean_path_str(&h.to_string_lossy()));
    let data_dir = std::env::var("GRACE_DATA_DIR").ok().filter(|s| !s.is_empty());
    let exe = exe()?;
    let text = conf_text(&ConfCtx {
        version,
        exe: &exe,
        socket: &t.socket,
        home: home.as_deref(),
        data_dir: data_dir.as_deref(),
    });
    std::fs::write(&conf, text)?;
    let conf_s = conf.to_string_lossy().into_owned();

    if t.ok(&["has-session", "-t", &format!("={SESSION}")]) {
        t.run(&["source-file", &conf_s])?;
        switch(&t, db, path, None)?;
    } else {
        let (w, h) = term_size();
        let (layout, sizes) = load_layout(db);
        let mut args: Vec<String> = ["-f", &conf_s, "new-session", "-d", "-s", SESSION, "-P", "-F", "#{window_id}|g|#{pane_id}"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        args.extend(["-x".into(), w.to_string(), "-y".into(), h.to_string()]);
        args.extend(["-n".into(), window_name(path), "-c".into(), path.into(), "--".into()]);
        args.extend(pane_command(&exe, Role::Main, path));
        let ids = t.run(&args)?;
        // The server may have run already (with no session): load the config now.
        t.run(&["source-file", &conf_s])?;
        let win = setup_window(&t, &ids, path, layout, &sizes)?;
        if let Some(main) = main_pane(&t.panes(&win)?) {
            t.run(&["select-pane", "-t", &main.id])?;
        }
    }
    if !attach {
        return Ok(());
    }
    let status = t.cmd().stdin(Stdio::inherit()).args(["attach-session", "-t", &format!("={SESSION}")]).status()?;
    // tmux exits with 1 when the server stops under it (ctrl-g Q, or the last
    // workspace closed). That is a normal quit, not an error.
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

/// `grace hub quit`: stop the hub server. This closes every workspace.
pub fn quit() -> Result<()> {
    let t = Tmux::new();
    if !t.ok(&["has-session", "-t", &format!("={SESSION}")]) {
        // A server with no grace session is still ours: stop it too.
        let _ = t.run(&["kill-server"]);
        println!("grace: the hub is not running.");
        return Ok(());
    }
    // Close the session: the server then stops on its own, and an attached
    // terminal shows "[exited]" like a normal end.
    t.run(&["kill-session", "-t", &format!("={SESSION}")])?;
    let _ = t.run(&["kill-server"]);
    println!("grace: the hub stopped.");
    Ok(())
}

/// True when this process is the hub's own `role` pane ("diff" or "tree").
/// A `grace diff` that the user runs in a workspace shell is not.
pub fn pane_in_hub(role: &str) -> bool {
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return false;
    };
    if !inside_hub(std::env::var("TMUX").ok().as_deref(), &socket()) {
        return false;
    }
    Tmux::new().run(&["display-message", "-p", "-t", &pane, "#{@grace_role}"]).is_ok_and(|r| r.trim() == role)
}

/// After `grace diff` or `grace tree` ends. In the hub, q hides the pane: the toggle
/// closes this pane and saves the layout, so every workspace stays the same.
pub fn pane_closed(in_hub: bool, role: &str) -> Result<()> {
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
    let sh = format!("GRACE_TMUX_SOCKET={} {} hub toggle {role} {}", sh_quote(&t.socket), sh_quote(&exe), sh_quote(win.trim()));
    if t.run(&["run-shell", "-b", &sh.replace('#', "##")]).is_ok() {
        // The toggle closes this pane. Wait for it, so the pane is still here when it looks.
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    Ok(())
}

/// `grace hub toggle <what>`: run by the ctrl-g keys. Errors show as a tmux message.
pub fn toggle(db: &Db, what: &str, window: Option<&str>) -> Result<()> {
    let t = Tmux::new();
    // Each key runs in its own process. Take turns, so fast presses do not each
    // see the old panes and add the same pane twice.
    let _lock = toggle_lock();
    if let Err(e) = toggle_inner(&t, db, what, window) {
        t.message(&format!("grace: {e}"));
    }
    Ok(())
}

fn toggle_inner(t: &Tmux, db: &Db, what: &str, window: Option<&str>) -> Result<()> {
    let Some(what) = Toggle::parse(what) else {
        bail!("unknown toggle {what}: use sidebar, diff, tree, right, zoom or focus");
    };
    let win = match window.filter(|w| !w.is_empty()) {
        Some(w) => w.to_string(),
        None => t.current_window()?,
    };
    if what == Toggle::Reap {
        // The window may be gone already: the shell wrapper closed it.
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
            // The window may have changed size while zoomed: fit the panes now.
            if !zoomed(t, &win) {
                let (_, sizes) = load_layout(db);
                fit(t, &win, &t.panes(&win)?, &sizes);
            }
        }
        Toggle::Fit => {
            let (_, sizes) = load_layout(db);
            fit(t, &win, &panes, &sizes);
        }
        Toggle::Focus => {
            if Layout::of(&panes).sidebar {
                let sb = panes.iter().find(|p| p.role == Some(Role::Sidebar)).unwrap();
                t.run(&["select-pane", "-t", &sb.id])?;
            } else {
                let (_, sizes) = load_layout(db);
                let want = Layout::of(&panes).toggled(Toggle::Focus);
                apply(t, &win, &path, want, &sizes)?;
                save_layout(db, want, &sizes)?;
                // Like the other toggles: every workspace shows the sidebar again.
                for (w, p) in t.windows()? {
                    if w != win {
                        let _ = apply(t, &w, &p, want, &sizes);
                    }
                }
                if let Some(sb) = t.panes(&win)?.iter().find(|p| p.role == Some(Role::Sidebar)) {
                    t.run(&["select-pane", "-t", &sb.id])?;
                }
            }
        }
        _ => {
            // The current window decides. Every workspace follows, so the layout stays the same.
            let (_, sizes) = load_layout(db);
            let want = Layout::of(&panes).toggled(what);
            let total = window_width(t, &win);
            let sizes = sizes.remember(&panes, want, total);
            save_layout(db, want, &sizes)?;
            apply(t, &win, &path, want, &sizes)?;
            for (w, p) in t.windows()? {
                if w != win {
                    let _ = apply(t, &w, &p, want, &sizes);
                }
            }
        }
    }
    Ok(())
}

/// Columns for a tmux size ("30" or "40%") in a window `total` columns wide.
fn cols(size: &str, total: u16) -> u16 {
    match size.strip_suffix('%') {
        Some(p) => (p.parse::<u32>().unwrap_or(0) * total as u32 / 100) as u16,
        None => size.parse().unwrap_or(0),
    }
}

/// Widths for the sidebar and the right column in a window `total` columns wide.
/// The main pane keeps at least 40% of the window, so the side panes shrink in a small window.
pub fn fit_widths(sizes: &Sizes, total: u16, sidebar: bool, right: bool) -> (u16, u16) {
    let mut sb = if sidebar { cols(&sizes.sidebar, total).max(1) } else { 0 };
    let mut rt = if right { cols(&sizes.right, total).max(1) } else { 0 };
    let room = total.saturating_sub(total * 2 / 5 + 2);
    if sb + rt > room && sb + rt > 0 {
        let k = room as u32;
        let sum = (sb + rt) as u32;
        sb = (sb as u32 * k / sum) as u16;
        rt = (rt as u32 * k / sum) as u16;
    }
    (sb, rt)
}

/// The width of a window in columns, or 0.
fn window_width(t: &Tmux, win: &str) -> u16 {
    t.run(&["display-message", "-p", "-t", win, "#{window_width}"]).ok().and_then(|w| w.trim().parse().ok()).unwrap_or(0)
}

/// True when the main pane of the window is zoomed.
fn zoomed(t: &Tmux, win: &str) -> bool {
    t.run(&["display-message", "-p", "-t", win, "#{window_zoomed_flag}"]).is_ok_and(|z| z.trim() == "1")
}

/// A lock file that makes toggles run one at a time. None when it cannot lock.
fn toggle_lock() -> Option<std::fs::File> {
    let dir = crate::db::data_dir().ok()?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("hub.lock")).ok()?;
    f.lock().ok()?;
    Some(f)
}

/// Set the sidebar width and the right column width to the saved sizes.
fn fit(t: &Tmux, win: &str, panes: &[Pane], sizes: &Sizes) {
    // resize-pane ends a zoom. Leave a zoomed window as it is: zoom out fits it.
    if zoomed(t, win) {
        return;
    }
    let total = window_width(t, win);
    if total == 0 {
        return;
    }
    let sb = panes.iter().find(|p| p.role == Some(Role::Sidebar));
    let rt = panes.iter().find(|p| matches!(p.role, Some(Role::Diff) | Some(Role::Tree)));
    let (w_sb, w_rt) = fit_widths(sizes, total, sb.is_some(), rt.is_some());
    if let Some(p) = sb {
        let _ = t.run(&["resize-pane", "-t", &p.id, "-x", &w_sb.max(1).to_string()]);
    }
    if let Some(p) = rt {
        let _ = t.run(&["resize-pane", "-t", &p.id, "-x", &w_rt.max(1).to_string()]);
    }
}

/// `grace hub shell`: the main pane. Runs the user's shell, and closes the
/// workspace when the shell exits, so no stateless panes stay behind.
pub fn shell() -> Result<()> {
    let sh = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "sh".into());
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut c = Command::new(&sh);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A leading dash makes it a login shell, like tmux does.
        let name = Path::new(&sh).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        c.arg0(format!("-{name}"));
    }
    let _ = c.status();
    if let Ok(pane) = std::env::var("TMUX_PANE") {
        let _ = Tmux::new().run(&["kill-window", "-t", &pane]);
    }
    Ok(())
}

/// The status glyph for the status bar, with tmux styles.
pub fn glyph(s: crate::model::Status, frame: usize) -> String {
    use crate::model::Status;
    match s {
        Status::Waiting => "#[fg=yellow,bold]●#[default]".into(),
        Status::Ready => "○".into(),
        Status::Working => {
            let f = crate::picker::SPINNER[frame % crate::picker::SPINNER.len()];
            format!("#[fg=cyan]{f}#[default]")
        }
        Status::Done => "#[fg=green]✓#[default]".into(),
    }
}

/// Show the agent status of each workspace in the status bar. One tmux call when something changed.
fn sync_glyphs(t: &Tmux, best: &crate::picker::Best, frame: usize) {
    let f = "#{window_id}|g|#{@grace_path}|g|#{@grace_glyph}";
    let Ok(out) = t.run(&["list-windows", "-t", &format!("={SESSION}"), "-F", f]) else {
        return;
    };
    let mut args: Vec<String> = Vec::new();
    for l in out.lines() {
        let mut parts = l.split(SEP);
        let (Some(w), Some(p)) = (parts.next(), parts.next()) else { continue };
        let have = parts.next().unwrap_or("");
        let want = best.get(p).map(|s| glyph(s.status, frame)).unwrap_or_default();
        if p.is_empty() || want == have {
            continue;
        }
        if !args.is_empty() {
            args.push(";".into());
        }
        if want.is_empty() {
            args.extend(["set-option", "-w", "-u", "-t", w, "@grace_glyph"].iter().map(|s| s.to_string()));
        } else {
            args.extend(["set-option", "-w", "-t", w, "@grace_glyph"].iter().map(|s| s.to_string()));
            args.push(want);
        }
    }
    if !args.is_empty() {
        let _ = t.run(&args);
    }
}

/// `grace hub sidebar <path>`: the sidebar pane of the workspace for `path`.
pub fn run_sidebar(db: &Db, reg: &crate::tools::Registry, path: &str) -> Result<()> {
    use crate::picker::{best_by_path, Data, Theme};
    let t = Tmux::new();
    let load = || -> Option<Data> {
        let now = crate::model::now();
        let entries = crate::load(db, reg, None).ok()?;
        let sessions = db.sessions(now).unwrap_or_default();
        Some(Data { entries, sessions, editors: Vec::new() })
    };
    let data = load().unwrap_or_default();
    let mut frame = 0usize;
    let reload = Box::new(|| {
        let d = load()?;
        frame += 1;
        sync_glyphs(&t, &best_by_path(&d.sessions), frame);
        Some(d)
    });
    let go = Box::new(|e: &crate::model::Entry, resume: bool| -> std::result::Result<(), String> {
        let cmd = if resume {
            // Read the tools again: the user may have added one since the sidebar started.
            let fresh = crate::tools::Registry::from_db(db).ok();
            let reg = fresh.as_ref().unwrap_or(reg);
            let cmd = e.tags.iter().filter(|t| reg.is_ai_tag(&t.tool)).find_map(|t| reg.launch_command(&t.tool));
            Some(cmd.ok_or("no AI tool used here")?)
        } else {
            None
        };
        switch(&t, db, &e.path, cmd.as_deref()).map(|_| ()).map_err(|e| e.to_string())
    });
    let home = crate::platform::home_dir().map(|h| h.to_string_lossy().into_owned());
    let mut sb = crate::sidebar::Sidebar::new(data, path, Theme::from_env(), home, reload, go);
    crate::tui::run(&mut sb)
}

/// `grace hub keys`: the help popup.
pub fn keys(wait: bool) -> Result<()> {
    let gold = "\x1b[1;38;2;232;185;74m";
    let reset = "\x1b[0m";
    println!();
    println!("  {gold}grace hub keys{reset}");
    println!("  \x1b[2mctrl-g, then one key\x1b[0m");
    println!();
    for (k, l, _) in KEYS {
        println!("  {gold}{k:<5}{reset} {l}");
    }
    if !wait {
        return Ok(());
    }
    println!();
    print!("  \x1b[2mpress any key\x1b[0m");
    use std::io::Write;
    std::io::stdout().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let _ = crossterm::event::read();
    crossterm::terminal::disable_raw_mode()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, role: Option<Role>, w: u16, h: u16) -> Pane {
        Pane { id: id.into(), role, width: w, height: h }
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
        let ops = plan(&[pane("%1", Some(Role::Main), 200, 50)], Layout::default(), &Sizes::default());
        assert_eq!(ops.len(), 6);
        // The sizes are set again at the end.
        assert_eq!(ops[3], Op::Height { target: Target::New(Role::Diff), size: "30".into() });
        assert_eq!(ops[4], Op::Width { target: Target::New(Role::Sidebar), size: "30".into() });
        assert_eq!(ops[5], Op::Width { target: Target::New(Role::Diff), size: "40%".into() });
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
        assert!(matches!(&ops[1], Op::Split { role: Role::Diff, full: true, before: false, size, .. } if size == "40%"));
        // 50 rows: diff 30, border 1, tree 19.
        assert!(matches!(&ops[2], Op::Split { target: Target::New(Role::Diff), role: Role::Tree, horizontal: false, size, .. } if size == "19"));
    }

    #[test]
    fn plan_toggles() {
        let s = Sizes::default();
        let l = Layout::of(&full());
        assert_eq!(l, Layout::default());
        // Hide the sidebar: one kill.
        assert_eq!(plan(&full(), l.toggled(Toggle::Sidebar), &s), vec![Op::Kill("%2".into())]);
        // The sidebar comes back, and the right column keeps its width.
        let no_sb: Vec<Pane> = full().into_iter().filter(|p| p.role != Some(Role::Sidebar)).collect();
        let ops = plan(&no_sb, l, &s);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[1], Op::Width { target: Target::Pane("%3".into()), size: "70".into() });
        // Hide the right side: both go.
        let r = l.toggled(Toggle::Right);
        assert_eq!((r.diff, r.tree), (false, false));
        assert_eq!(plan(&full(), r, &s), vec![Op::Kill("%3".into()), Op::Kill("%4".into())]);
        // The right side comes back, and the sidebar keeps its width.
        let no_right: Vec<Pane> = full().into_iter().take(2).collect();
        let ops = plan(&no_right, l, &s);
        assert_eq!(ops.len(), 4);
        assert_eq!(ops[3], Op::Width { target: Target::Pane("%2".into()), size: "30".into() });
        // Diff comes back above the tree.
        let no_diff: Vec<Pane> = full().into_iter().filter(|p| p.role != Some(Role::Diff)).collect();
        let ops = plan(&no_diff, Layout::of(&no_diff).toggled(Toggle::Diff), &s);
        assert_eq!(
            ops,
            vec![
                Op::Split {
                    target: Target::Pane("%4".into()),
                    role: Role::Diff,
                    horizontal: false,
                    before: true,
                    full: false,
                    size: "11".into()
                },
                Op::Height { target: Target::New(Role::Diff), size: "11".into() },
            ]
        );
        // Tree alone comes back as the full right column.
        let main_only = vec![pane("%1", Some(Role::Main), 100, 50)];
        let ops = plan(&main_only, Layout { sidebar: false, diff: false, tree: true }, &s);
        assert!(matches!(&ops[0], Op::Split { role: Role::Tree, horizontal: true, full: true, .. }));
        assert_eq!(ops.len(), 1);
        // The sidebar alone needs no width step.
        let ops = plan(&main_only, Layout { sidebar: true, diff: false, tree: false }, &s);
        assert_eq!(ops.len(), 1);
        // Right side toggles back on when both are hidden.
        assert_eq!(r.toggled(Toggle::Right), l);
        // No main pane: nothing to do.
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
        assert_eq!(a.last().unwrap(), "/p q");
        let t = tag_args("%5", Role::Sidebar);
        assert_eq!(t.join(" "), "set-option -p -t %5 @grace_role sidebar ; set-option -p -t %5 @grace_title grace");
        assert_eq!(tag_args("%1", Role::Main).join(" "), "set-option -p -t %1 @grace_role main");
    }

    #[test]
    fn sizes_are_remembered() {
        let s = Sizes::default();
        let l = Layout::default();
        let mut panes = full();
        panes[1].width = 26;
        assert_eq!(s.remember(&panes, l.toggled(Toggle::Sidebar), 0).sidebar, "26");
        assert_eq!(s.remember(&panes, l.toggled(Toggle::Right), 0).right, "70");
        assert_eq!(s.remember(&panes, l.toggled(Toggle::Diff), 0).diff, 60);
        panes[2].height = 24;
        panes[3].height = 19;
        assert_eq!(s.remember(&panes, l.toggled(Toggle::Tree), 0).diff, 55);
        assert_eq!(share(44, 60), 26);
        // Nothing hides: sizes stay.
        assert_eq!(s.remember(&panes, l, 0), s);
    }

    #[test]
    fn shrunk_sizes_are_not_remembered() {
        // An 80 column window: fit shrank the sidebar to 22 and the right side to 23.
        let s = Sizes::default();
        let l = Layout::default();
        let (sb, rt) = fit_widths(&s, 80, true, true);
        let mut panes = full();
        panes[1].width = sb;
        panes[2].width = rt;
        panes[3].width = rt;
        assert_eq!(s.remember(&panes, l.toggled(Toggle::Sidebar), 80).sidebar, "30");
        assert_eq!(s.remember(&panes, l.toggled(Toggle::Right), 80).right, "40%");
        // The user dragged the sidebar: keep it.
        panes[1].width = sb + 5;
        assert_eq!(s.remember(&panes, l.toggled(Toggle::Sidebar), 80).sidebar, (sb + 5).to_string());
        // A split one row off is rounding, not a change.
        panes[2].height = 13;
        panes[3].height = 8;
        assert_eq!(s.remember(&panes, l.toggled(Toggle::Tree), 80).diff, 60);
    }

    #[test]
    fn printable_lines() {
        assert_eq!(crate::git::printable("a\x1b[31mR\rC\x07\tx"), "a?[31mR?C?    x");
        assert_eq!(crate::git::printable("日本 ok"), "日本 ok");
    }

    #[test]
    fn layout_round_trip() {
        for l in [
            Layout::default(),
            Layout { sidebar: false, diff: true, tree: false },
            Layout { sidebar: false, diff: false, tree: false },
        ] {
            assert_eq!(Layout::from_meta(&l.to_meta()), l);
        }
        assert_eq!(Layout::default().to_meta(), "sidebar diff tree");
        let s = Sizes { sidebar: "24".into(), right: "35%".into(), diff: 55 };
        assert_eq!(Sizes::from_meta(&s.to_meta()), s);
        // Bad values fall back to the defaults.
        assert_eq!(Sizes::from_meta("sidebar=x right=0 diff=99; rm"), Sizes::default());

        let d = tempfile::tempdir().unwrap();
        let db = Db::open(&d.path().join("g.db")).unwrap();
        assert_eq!(load_layout(&db), (Layout::default(), Sizes::default()));
        let l = Layout { sidebar: true, diff: false, tree: true };
        save_layout(&db, l, &s).unwrap();
        assert_eq!(load_layout(&db), (l, s));
    }

    #[test]
    fn panes_parse() {
        let p = parse_panes("%1|g|main|g|100|g|50\n%2|g||g|80|g|20\nbad line\n%3|g|sidebar|g|30|g|50");
        assert_eq!(p.len(), 3);
        assert_eq!(p[1].role, None);
        assert_eq!(p[2], pane("%3", Some(Role::Sidebar), 30, 50));
        // A pane without a role counts as main when no pane has the role.
        assert_eq!(main_pane(&p[1..]).unwrap().id, "%2");
    }

    fn conf(v: (u32, u32)) -> String {
        conf_text(&ConfCtx {
            version: v,
            exe: "/opt/my tools/grace",
            socket: "grace",
            home: Some("/Users/me"),
            data_dir: None,
        })
    }

    #[test]
    fn conf_has_keys_and_style() {
        let c = conf((3, 8));
        println!("{c}");
        assert!(c.contains("bind -n C-g switch-client -T grace"));
        for k in ["s", "d", "t", "r", "z", "g"] {
            assert!(c.contains(&format!("bind -T grace {k} run-shell -b ")), "{k}");
        }
        assert!(c.contains("bind -T grace q detach-client"));
        assert!(c.contains("bind -T grace Q confirm-before"));
        assert!(c.contains("set-hook -g pane-exited"));
        assert!(c.contains("hub toggle reap '#{hook_window}'"));
        assert!(c.contains("bind -T grace ? display-popup -E"));
        assert!(c.contains("GRACE_TMUX_SOCKET='grace' '/opt/my tools/grace' hub toggle sidebar '#{window_id}'"));
        assert!(c.contains("set-environment -g GRACE_TMUX_SOCKET \"grace\""));
        assert!(c.contains("pane-active-border-style \"fg=#e8b94a\""));
        assert!(c.contains("set -g pane-border-lines single"));
        assert!(c.contains("set -as terminal-features \",*:RGB\""));
        assert!(c.contains("set -g allow-passthrough on"));
        assert!(c.contains("#{s|^/Users/me|~|:pane_current_path}"));
        assert!(c.contains("? keys · q detach · Q quit"));
        assert!(!c.contains("~/.tmux.conf\n") && !c.contains("source-file"));
        for opt in ["mouse on", "escape-time 10", "focus-events on", "set-clipboard on", "extended-keys on", "tmux-256color"] {
            assert!(c.contains(opt), "{opt}");
        }
    }

    #[test]
    fn conf_degrades_on_old_tmux() {
        let c = conf((3, 0));
        assert!(!c.contains("pane-border-lines"));
        assert!(!c.contains("terminal-features"));
        assert!(!c.contains("allow-passthrough"));
        assert!(!c.contains("display-popup"));
        assert!(c.contains("terminal-overrides \",*:Tc\""));
        assert!(c.contains("bind -T grace ? display-message"));
        let c = conf((3, 2));
        assert!(c.contains("display-popup") && !c.contains("-T \" grace keys \""));
        assert!(!c.contains("allow-passthrough"));
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(tmux_quote(r#"a"b$c\d"#), r#""a\"b\$c\\d""#);
        assert_eq!(regex_escape("/a.b"), r"/a\.b");
    }

    #[test]
    fn versions_and_socket() {
        assert_eq!(parse_version("tmux 3.8"), Some((3, 8)));
        assert_eq!(parse_version("tmux 3.3a"), Some((3, 3)));
        assert_eq!(parse_version("tmux next-3.6"), Some((3, 6)));
        assert_eq!(parse_version("tmux master"), None);
        assert!(inside_hub(Some("/private/tmp/tmux-501/grace,123,0"), "grace"));
        assert!(!inside_hub(Some("/private/tmp/tmux-501/default,123,0"), "grace"));
        assert!(!inside_hub(Some(""), "grace"));
        assert!(!inside_hub(None, "grace"));
    }

    #[test]
    fn toggles_parse() {
        assert_eq!(Toggle::parse("right"), Some(Toggle::Right));
        assert_eq!(Toggle::parse("nope"), None);
        assert!(is_shell("zsh") && is_shell("-bash") && !is_shell("claude"));
        assert!(keys_line().contains("s sidebar"));
    }

    #[test]
    fn fit_keeps_room_for_main() {
        let d = Sizes::default();
        // Wide window: the saved sizes as they are.
        assert_eq!(fit_widths(&d, 200, true, true), (30, 80));
        // Narrow window: the side panes shrink, the main pane keeps at least 40%.
        let (sb, rt) = fit_widths(&d, 80, true, true);
        assert!(80 - sb - rt >= 32, "{sb} {rt}");
        // Hidden panes take no space.
        assert_eq!(fit_widths(&d, 200, false, true), (0, 80));
        let big = Sizes { sidebar: "74".into(), right: "65".into(), diff: 60 };
        let (sb, rt) = fit_widths(&big, 120, true, true);
        assert!(120 - sb - rt >= 48, "{sb} {rt}");
    }
}
