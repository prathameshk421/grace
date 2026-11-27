//! Unified app, editor, and CLI agent launcher: discovery, ranking, and modal state.
use crate::config::Config;
use crate::editors_live::OpenEditor;
use crate::model::{Entry, Session, Status};
use crate::picker::EditorChoice;
use crate::platform;
use crate::tools::{self, Kind, Registry};
use anyhow::{bail, Result};
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config as MatcherConfig, Matcher, Utf32Str,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
#[cfg(any(target_os = "macos", windows))]
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    GuiApp,
    TerminalEditor,
    CliAgent,
    HubShell,
    Travel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchMethod {
    Gui {
        prog: String,
        args_before: Vec<String>,
        pass_path: bool,
        focus_only: bool,
    },
    /// Built only on macOS (an app opened through its URL scheme).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    GuiUrl {
        prog: String,
        args_before: Vec<String>,
        url_prefix: String,
    },
    TerminalEditor {
        prog: String,
        args_before: Vec<String>,
    },
    CliAgent {
        tool_tag: String,
        launch_cmd: String,
    },
    HubShell,
    Travel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledTarget {
    pub id: String,
    pub label: String,
    pub tag: String,
    pub kind: TargetKind,
    pub method: LaunchMethod,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchItem {
    pub target: InstalledTarget,
    pub badge: String,
    pub is_open: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchChoice {
    pub target: InstalledTarget,
    pub force_shell: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchRow {
    Header(&'static str),
    Item(usize),
}

struct GuiAppSpec {
    id: &'static str,
    label: &'static str,
    tag: &'static str,
    clis: &'static [&'static str],
    cli_args_before: &'static [&'static str],
    cli_pass_path: bool,
    cli_focus_only: bool,
    mac_bundles: &'static [(&'static str, bool, bool)], // (Bundle name, pass_path, focus_only)
    mac_url_prefix: Option<&'static str>,
    mac_inner_bins: &'static [&'static str],
    win_rel_exes: &'static [(&'static str, bool, bool)], // (rel path, pass_path, focus_only)
}

const GUI_SPECS: &[GuiAppSpec] = &[
    GuiAppSpec {
        id: "antigravity",
        label: "Antigravity",
        tag: "antigravity",
        clis: &["antigravity", "antigravity-ide"],
        cli_args_before: &[],
        cli_pass_path: true,
        cli_focus_only: false,
        mac_bundles: &[
            ("Antigravity IDE", true, false),
            ("Antigravity", false, true),
        ],
        mac_url_prefix: None,
        mac_inner_bins: &["Contents/Resources/app/bin/antigravity"],
        win_rel_exes: &[(r"Programs\Antigravity\Antigravity.exe", true, false)],
    },
    GuiAppSpec {
        id: "cursor",
        label: "Cursor",
        tag: "cursor",
        clis: &["cursor"],
        cli_args_before: &[],
        cli_pass_path: true,
        cli_focus_only: false,
        mac_bundles: &[("Cursor", true, false)],
        mac_url_prefix: None,
        mac_inner_bins: &["Contents/Resources/app/bin/cursor"],
        win_rel_exes: &[(r"Programs\cursor\Cursor.exe", true, false)],
    },
    GuiAppSpec {
        id: "windsurf",
        label: "Windsurf",
        tag: "windsurf",
        clis: &["windsurf"],
        cli_args_before: &[],
        cli_pass_path: true,
        cli_focus_only: false,
        mac_bundles: &[("Windsurf", true, false)],
        mac_url_prefix: None,
        mac_inner_bins: &["Contents/Resources/app/bin/windsurf"],
        win_rel_exes: &[(r"Programs\Windsurf\Windsurf.exe", true, false)],
    },
    GuiAppSpec {
        id: "t3code",
        label: "T3 Code",
        tag: "t3code",
        clis: &["t3"],
        cli_args_before: &["app"],
        cli_pass_path: true,
        cli_focus_only: false,
        mac_bundles: &[("T3 Code", false, true), ("T3 Code (Nightly)", false, true)],
        mac_url_prefix: None,
        mac_inner_bins: &[],
        win_rel_exes: &[
            (r"Programs\T3 Code\T3 Code.exe", false, true),
            (
                r"Programs\T3 Code (Nightly)\T3 Code (Nightly).exe",
                false,
                true,
            ),
        ],
    },
    GuiAppSpec {
        id: "claude-desktop",
        label: "Claude Desktop",
        tag: "claude-desktop",
        clis: &["claude-desktop"],
        cli_args_before: &[],
        cli_pass_path: false,
        cli_focus_only: true,
        mac_bundles: &[("Claude", true, false)],
        mac_url_prefix: Some("claude://code/new?folder="),
        mac_inner_bins: &[],
        win_rel_exes: &[
            (r"AnthropicClaude\claude.exe", false, true),
            (r"Programs\Claude\Claude.exe", false, true),
        ],
    },
    GuiAppSpec {
        id: "vscode",
        label: "VS Code",
        tag: "vscode",
        clis: &["code"],
        cli_args_before: &[],
        cli_pass_path: true,
        cli_focus_only: false,
        mac_bundles: &[("Visual Studio Code", true, false)],
        mac_url_prefix: None,
        mac_inner_bins: &["Contents/Resources/app/bin/code"],
        win_rel_exes: &[(r"Programs\Microsoft VS Code\Code.exe", true, false)],
    },
    GuiAppSpec {
        id: "vscode-insiders",
        label: "VS Code Insiders",
        tag: "vscode",
        clis: &["code-insiders"],
        cli_args_before: &[],
        cli_pass_path: true,
        cli_focus_only: false,
        mac_bundles: &[("Visual Studio Code - Insiders", true, false)],
        mac_url_prefix: None,
        mac_inner_bins: &["Contents/Resources/app/bin/code-insiders"],
        win_rel_exes: &[(
            r"Programs\Microsoft VS Code Insiders\Code - Insiders.exe",
            true,
            false,
        )],
    },
    GuiAppSpec {
        id: "vscodium",
        label: "VSCodium",
        tag: "vscodium",
        clis: &["codium"],
        cli_args_before: &[],
        cli_pass_path: true,
        cli_focus_only: false,
        mac_bundles: &[("VSCodium", true, false)],
        mac_url_prefix: None,
        mac_inner_bins: &["Contents/Resources/app/bin/codium"],
        win_rel_exes: &[(r"Programs\VSCodium\VSCodium.exe", true, false)],
    },
    GuiAppSpec {
        id: "zed",
        label: "Zed",
        tag: "zed",
        clis: &["zed"],
        cli_args_before: &[],
        cli_pass_path: true,
        cli_focus_only: false,
        mac_bundles: &[("Zed", true, false)],
        mac_url_prefix: None,
        mac_inner_bins: &["Contents/MacOS/cli"],
        win_rel_exes: &[(r"Programs\Zed\zed.exe", true, false)],
    },
    GuiAppSpec {
        id: "sublime",
        label: "Sublime Text",
        tag: "sublime",
        clis: &["subl"],
        cli_args_before: &[],
        cli_pass_path: true,
        cli_focus_only: false,
        mac_bundles: &[("Sublime Text", true, false)],
        mac_url_prefix: None,
        mac_inner_bins: &["Contents/SharedSupport/bin/subl"],
        win_rel_exes: &[(r"Sublime Text\subl.exe", true, false)],
    },
    GuiAppSpec {
        id: "idea",
        label: "IntelliJ IDEA",
        tag: "jetbrains",
        clis: &["idea"],
        cli_args_before: &[],
        cli_pass_path: true,
        cli_focus_only: false,
        mac_bundles: &[
            ("IntelliJ IDEA", true, false),
            ("IntelliJ IDEA CE", true, false),
        ],
        mac_url_prefix: None,
        mac_inner_bins: &[],
        win_rel_exes: &[],
    },
    GuiAppSpec {
        id: "pycharm",
        label: "PyCharm",
        tag: "jetbrains",
        clis: &["pycharm"],
        cli_args_before: &[],
        cli_pass_path: true,
        cli_focus_only: false,
        mac_bundles: &[("PyCharm", true, false), ("PyCharm CE", true, false)],
        mac_url_prefix: None,
        mac_inner_bins: &[],
        win_rel_exes: &[],
    },
    GuiAppSpec {
        id: "webstorm",
        label: "WebStorm",
        tag: "jetbrains",
        clis: &["webstorm"],
        cli_args_before: &[],
        cli_pass_path: true,
        cli_focus_only: false,
        mac_bundles: &[("WebStorm", true, false)],
        mac_url_prefix: None,
        mac_inner_bins: &[],
        win_rel_exes: &[],
    },
];

pub fn has_tmux() -> bool {
    !cfg!(windows) && tools::on_path("tmux")
}

#[cfg(target_os = "macos")]
fn find_trusted_mac_bundle(name: &str) -> Option<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
    ];
    if let Some(h) = platform::home_dir() {
        roots.push(h.join("Applications"));
    }
    for root in roots {
        let cand = root.join(format!("{name}.app"));
        let Some(s) = cand.to_str() else {
            continue;
        };
        if crate::editors_live::trusted_bundle(s) {
            return Some(cand);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn safe_bundle_bin(bundle: &Path, inner_rel: &str) -> Option<PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let cand = bundle.join(inner_rel);
    let meta = std::fs::metadata(&cand).ok()?;
    if !meta.is_file() {
        return None;
    }
    let euid = unsafe { libc::geteuid() };
    let mode = meta.permissions().mode();
    if (mode & 0o111) == 0 || (mode & 0o002) != 0 || (meta.uid() != 0 && meta.uid() != euid) {
        return None;
    }
    if let (Ok(b_canon), Ok(c_canon)) =
        (std::fs::canonicalize(bundle), std::fs::canonicalize(&cand))
    {
        if !c_canon.starts_with(&b_canon) {
            return None;
        }
    }
    Some(cand)
}

#[cfg(windows)]
fn find_win_exe(rel: &str) -> Option<PathBuf> {
    for var in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(base) = platform::env_non_empty(var) {
            let cand = PathBuf::from(base).join(rel);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn detect_gui_app(spec: &GuiAppSpec) -> Option<InstalledTarget> {
    for cli in spec.clis {
        if let Some(p) = tools::resolve_on_path(cli) {
            return Some(InstalledTarget {
                id: spec.id.into(),
                label: spec.label.into(),
                tag: spec.tag.into(),
                kind: TargetKind::GuiApp,
                method: LaunchMethod::Gui {
                    prog: p.to_string_lossy().into_owned(),
                    args_before: spec.cli_args_before.iter().map(|s| (*s).into()).collect(),
                    pass_path: spec.cli_pass_path,
                    focus_only: spec.cli_focus_only,
                },
            });
        }
    }

    #[cfg(target_os = "macos")]
    for &(bundle_name, open_accepts_path, open_focus_only) in spec.mac_bundles {
        let Some(bundle) = find_trusted_mac_bundle(bundle_name) else {
            continue;
        };
        for inner in spec.mac_inner_bins {
            if let Some(bin) = safe_bundle_bin(&bundle, inner) {
                return Some(InstalledTarget {
                    id: spec.id.into(),
                    label: spec.label.into(),
                    tag: spec.tag.into(),
                    kind: TargetKind::GuiApp,
                    method: LaunchMethod::Gui {
                        prog: bin.to_string_lossy().into_owned(),
                        args_before: Vec::new(),
                        pass_path: true,
                        focus_only: false,
                    },
                });
            }
        }
        if let Some(prefix) = spec.mac_url_prefix {
            return Some(InstalledTarget {
                id: spec.id.into(),
                label: spec.label.into(),
                tag: spec.tag.into(),
                kind: TargetKind::GuiApp,
                method: LaunchMethod::GuiUrl {
                    prog: "open".into(),
                    args_before: vec!["-a".into(), bundle.to_string_lossy().into_owned()],
                    url_prefix: prefix.into(),
                },
            });
        }
        let mut args_before = vec!["-a".into(), bundle.to_string_lossy().into_owned()];
        if open_accepts_path {
            args_before.push("--".into());
        }
        return Some(InstalledTarget {
            id: spec.id.into(),
            label: spec.label.into(),
            tag: spec.tag.into(),
            kind: TargetKind::GuiApp,
            method: LaunchMethod::Gui {
                prog: "open".into(),
                args_before,
                pass_path: open_accepts_path,
                focus_only: open_focus_only,
            },
        });
    }

    #[cfg(windows)]
    for &(rel, pass_path, focus_only) in spec.win_rel_exes {
        if let Some(exe) = find_win_exe(rel) {
            return Some(InstalledTarget {
                id: spec.id.into(),
                label: spec.label.into(),
                tag: spec.tag.into(),
                kind: TargetKind::GuiApp,
                method: LaunchMethod::Gui {
                    prog: exe.to_string_lossy().into_owned(),
                    args_before: Vec::new(),
                    pass_path,
                    focus_only,
                },
            });
        }
    }

    let _ = (
        spec.cli_args_before,
        spec.cli_pass_path,
        spec.cli_focus_only,
        spec.mac_bundles,
        spec.mac_url_prefix,
        spec.mac_inner_bins,
        spec.win_rel_exes,
    );
    None
}

/// Detect Codex / ChatGPT Desktop separately because OpenAI ships the Codex workspace inside
/// `ChatGPT.app` (`com.openai.codex`) on macOS or standalone `Codex.app`.
fn detect_codex_desktop() -> Option<InstalledTarget> {
    #[cfg(target_os = "macos")]
    {
        if let Some(bundle) = find_trusted_mac_bundle("Codex") {
            return Some(InstalledTarget {
                id: "codex-app".into(),
                label: "Codex App".into(),
                tag: "codex-app".into(),
                kind: TargetKind::GuiApp,
                method: LaunchMethod::Gui {
                    prog: "open".into(),
                    args_before: vec![
                        "-a".into(),
                        bundle.to_string_lossy().into_owned(),
                        "--".into(),
                    ],
                    pass_path: true,
                    focus_only: false,
                },
            });
        }
        if let Some(bundle) = find_trusted_mac_bundle("ChatGPT") {
            use std::io::Read;
            let bundle_str = bundle.to_string_lossy().into_owned();
            let mut buf = Vec::new();
            if let Ok(f) = std::fs::File::open(bundle.join("Contents/Info.plist")) {
                let _ = f.take(256 * 1024).read_to_end(&mut buf);
            }
            let has_codex = buf
                .windows(b"com.openai.codex".len())
                .any(|w| w == b"com.openai.codex")
                || buf
                    .windows(b"public.folder".len())
                    .any(|w| w == b"public.folder");
            let (label, args_before, pass_path, focus_only) = if has_codex {
                (
                    "Codex (ChatGPT)",
                    vec!["-a".into(), bundle_str, "--".into()],
                    true,
                    false,
                )
            } else {
                ("ChatGPT", vec!["-a".into(), bundle_str], false, true)
            };
            return Some(InstalledTarget {
                id: "codex-app".into(),
                label: label.into(),
                tag: "codex-app".into(),
                kind: TargetKind::GuiApp,
                method: LaunchMethod::Gui {
                    prog: "open".into(),
                    args_before,
                    pass_path,
                    focus_only,
                },
            });
        }
    }
    #[cfg(windows)]
    for rel in [r"Programs\Codex\Codex.exe", r"Codex\Codex.exe"] {
        if let Some(exe) = find_win_exe(rel) {
            return Some(InstalledTarget {
                id: "codex-app".into(),
                label: "Codex App".into(),
                tag: "codex-app".into(),
                kind: TargetKind::GuiApp,
                method: LaunchMethod::Gui {
                    prog: exe.to_string_lossy().into_owned(),
                    args_before: Vec::new(),
                    pass_path: true,
                    focus_only: false,
                },
            });
        }
    }
    None
}

/// Find the matching `OpenEditor` entry for `target` at `folder_path`.
pub fn find_open_for_target<'a>(
    target: &InstalledTarget,
    folder_path: &str,
    open: &'a [OpenEditor],
) -> Option<&'a OpenEditor> {
    open.iter()
        .find(|o| platform::same_path(&o.path, folder_path) && matches_open_target(target, o))
}

fn editor_configured_vim(cfg: &Config) -> bool {
    cfg.editor
        .as_deref()
        .and_then(|s| s.split_whitespace().next())
        .is_some_and(|p| {
            let base = Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(p);
            base.strip_suffix(".exe").unwrap_or(base) == "vim"
        })
}

fn resolve_program(prog: &str) -> Option<String> {
    let p = Path::new(prog);
    if p.is_absolute() {
        p.is_file().then(|| prog.to_string())
    } else {
        tools::resolve_on_path(prog).map(|pb| pb.to_string_lossy().into_owned())
    }
}

fn split_editor_launch(launch: &str) -> Option<(String, Vec<String>)> {
    let base = launch.strip_suffix(" .").unwrap_or(launch).trim();
    let mut parts = base.split_whitespace();
    let prog = parts.next()?.to_string();
    let args = parts.map(String::from).collect();
    Some((prog, args))
}

/// Discover installed GUI apps, editors, CLI AI agents, and valid custom tools.
pub fn discover_installed(reg: &Registry, cfg: &Config) -> Vec<InstalledTarget> {
    let mut out = Vec::new();
    let mut seen_ids = HashSet::new();

    for spec in GUI_SPECS {
        if let Some(t) = detect_gui_app(spec) {
            if seen_ids.insert(t.id.clone()) {
                out.push(t);
            }
        }
    }
    if let Some(t) = detect_codex_desktop() {
        if seen_ids.insert(t.id.clone()) {
            out.push(t);
        }
    }

    if let Some(p) = tools::resolve_on_path("nvim") {
        if seen_ids.insert("neovim".into()) {
            out.push(InstalledTarget {
                id: "neovim".into(),
                label: "Neovim".into(),
                tag: "neovim".into(),
                kind: TargetKind::TerminalEditor,
                method: LaunchMethod::TerminalEditor {
                    prog: p.to_string_lossy().into_owned(),
                    args_before: Vec::new(),
                },
            });
        }
    }

    if editor_configured_vim(cfg) {
        if let Some(p) = tools::resolve_on_path("vim") {
            if seen_ids.insert("vim".into()) {
                out.push(InstalledTarget {
                    id: "vim".into(),
                    label: "Vim".into(),
                    tag: "vim".into(),
                    kind: TargetKind::TerminalEditor,
                    method: LaunchMethod::TerminalEditor {
                        prog: p.to_string_lossy().into_owned(),
                        args_before: Vec::new(),
                    },
                });
            }
        }
    }

    // Built-in AI CLIs, deduplicated by canonical tag, skipping t3code (handled as GUI app above).
    let mut seen_cli_tags = HashSet::new();
    for t in reg.tools() {
        if t.kind != Some(Kind::Ai) || t.tag == "t3code" || !seen_cli_tags.insert(t.tag.clone()) {
            continue;
        }
        let Some(launch_cmd) = reg.launch_command(&t.tag) else {
            continue;
        };
        if !tools::valid_launch(&launch_cmd) {
            continue;
        }
        let Some(bin) = launch_cmd.split_whitespace().next() else {
            continue;
        };
        if !tools::on_path(bin) {
            continue;
        }
        let id = format!("cli:{}", t.tag);
        if seen_ids.insert(id.clone()) {
            out.push(InstalledTarget {
                id,
                label: bin.to_string(),
                tag: t.tag.clone(),
                kind: TargetKind::CliAgent,
                method: LaunchMethod::CliAgent {
                    tool_tag: t.tag.clone(),
                    launch_cmd,
                },
            });
        }
    }

    // Custom tools.
    for t in reg.tools() {
        if t.kind.is_some() || !tools::valid_launch(&t.launch) {
            continue;
        }
        let Some((prog, args_before)) = split_editor_launch(&t.launch) else {
            continue;
        };
        if resolve_program(&prog).is_none() {
            continue;
        }
        let id = format!("custom:{}", t.command);
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        if tools::is_terminal_editor(&prog) {
            out.push(InstalledTarget {
                id,
                label: t.command.clone(),
                tag: t.tag.clone(),
                kind: TargetKind::TerminalEditor,
                method: LaunchMethod::TerminalEditor { prog, args_before },
            });
        } else {
            out.push(InstalledTarget {
                id,
                label: t.command.clone(),
                tag: t.tag.clone(),
                kind: TargetKind::CliAgent,
                method: LaunchMethod::CliAgent {
                    tool_tag: t.tag.clone(),
                    launch_cmd: t.launch.clone(),
                },
            });
        }
    }

    // Fallback configured editor if not already represented.
    if let Some(ref fb) = cfg.editor {
        if tools::valid_launch(fb) {
            if let Some((prog, args_before)) = split_editor_launch(fb) {
                if let Some(resolved) = resolve_program(&prog) {
                    let base = Path::new(&prog)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&prog);
                    let tag = reg.normalize_tag(base);
                    let already = out.iter().any(|item| {
                        item.tag == tag
                            || match &item.method {
                                LaunchMethod::Gui { prog: p, .. }
                                | LaunchMethod::GuiUrl { prog: p, .. }
                                | LaunchMethod::TerminalEditor { prog: p, .. } => {
                                    p == &resolved || p == &prog
                                }
                                _ => false,
                            }
                    });
                    if !already {
                        let is_term = tools::is_terminal_editor(&prog);
                        out.push(InstalledTarget {
                            id: format!("cfg:{tag}"),
                            label: base.to_string(),
                            tag,
                            kind: if is_term {
                                TargetKind::TerminalEditor
                            } else {
                                TargetKind::GuiApp
                            },
                            method: if is_term {
                                LaunchMethod::TerminalEditor {
                                    prog: resolved,
                                    args_before,
                                }
                            } else {
                                LaunchMethod::Gui {
                                    prog: resolved,
                                    args_before,
                                    pass_path: true,
                                    focus_only: false,
                                }
                            },
                        });
                    }
                }
            }
        }
    }

    out
}

/// Percent-encode a string for URL query parameter values (RFC 3986 unreserved kept literal).
pub(crate) fn percent_encode_query(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// Build an argv-safe `EditorChoice` for GUI or terminal editors.
pub fn build_editor_choice(
    target: &InstalledTarget,
    folder_path: &str,
    open: Option<&OpenEditor>,
) -> Result<EditorChoice> {
    if !Path::new(folder_path).is_absolute()
        || platform::has_line_break_or_nul(folder_path)
        || folder_path.starts_with('-')
    {
        bail!("refusing unsafe folder path for editor launch");
    }
    if let Some(o) = open {
        if o.tag == target.tag && target.kind == TargetKind::GuiApp {
            if platform::has_line_break_or_nul(&o.prog)
                || o.args.iter().any(|a| platform::has_line_break_or_nul(a))
            {
                bail!("refusing editor arguments containing control characters");
            }
            return Ok(EditorChoice {
                prog: o.prog.clone(),
                args: o.args.clone(),
                in_terminal: false,
            });
        }
    }
    match &target.method {
        LaunchMethod::Gui {
            prog,
            args_before,
            pass_path,
            ..
        } => {
            if platform::has_line_break_or_nul(prog)
                || args_before
                    .iter()
                    .any(|a| platform::has_line_break_or_nul(a))
            {
                bail!("refusing editor command containing control characters");
            }
            let mut args = args_before.clone();
            if *pass_path {
                args.push(folder_path.to_string());
            }
            Ok(EditorChoice {
                prog: prog.clone(),
                args,
                in_terminal: false,
            })
        }
        LaunchMethod::GuiUrl {
            prog,
            args_before,
            url_prefix,
        } => {
            if platform::has_line_break_or_nul(prog)
                || platform::has_line_break_or_nul(url_prefix)
                || args_before
                    .iter()
                    .any(|a| platform::has_line_break_or_nul(a))
            {
                bail!("refusing editor command containing control characters");
            }
            let mut args = args_before.clone();
            args.push(format!("{url_prefix}{}", percent_encode_query(folder_path)));
            Ok(EditorChoice {
                prog: prog.clone(),
                args,
                in_terminal: false,
            })
        }
        LaunchMethod::TerminalEditor { prog, args_before } => {
            if platform::has_line_break_or_nul(prog)
                || args_before
                    .iter()
                    .any(|a| platform::has_line_break_or_nul(a))
            {
                bail!("refusing editor command containing control characters");
            }
            let mut args = args_before.clone();
            args.push(folder_path.to_string());
            Ok(EditorChoice {
                prog: prog.clone(),
                args,
                in_terminal: true,
            })
        }
        _ => bail!("target `{}` is not an editor", target.label),
    }
}

/// Resolve an editor launch for `ctrl-e`: open window first, then installed editor for tag,
/// then registry launch command, then `cfg.editor`.
pub fn resolve_editor_for_entry(
    installed: Option<&[InstalledTarget]>,
    reg: &Registry,
    cfg: &Config,
    e: &Entry,
    open: Option<&OpenEditor>,
) -> Option<EditorChoice> {
    if let Some(o) = open {
        return Some(EditorChoice {
            prog: o.prog.clone(),
            args: o.args.clone(),
            in_terminal: false,
        });
    }
    if let Some(tag) = e.tags.iter().find(|t| {
        reg.is_editor_tag(&t.tool)
            || matches!(t.tool.as_str(), "t3code" | "claude-desktop" | "codex-app")
    }) {
        if let Some(list) = installed {
            if let Some(target) = list.iter().find(|t| {
                t.tag == tag.tool
                    && matches!(t.kind, TargetKind::GuiApp | TargetKind::TerminalEditor)
            }) {
                if let Ok(ed) = build_editor_choice(target, &e.path, None) {
                    return Some(ed);
                }
            }
        }
        if let Some((prog, mut args, in_terminal)) = reg.editor_launch(&tag.tool) {
            args.push(e.path.clone());
            return Some(EditorChoice {
                prog,
                args,
                in_terminal,
            });
        }
    }
    if let Some(ref fb) = cfg.editor {
        let mut parts = fb.split_whitespace();
        if let Some(prog) = parts.next() {
            if tools::valid_launch(prog) {
                let mut args: Vec<String> = parts.map(String::from).collect();
                args.push(e.path.clone());
                let in_terminal = tools::is_terminal_editor(prog);
                return Some(EditorChoice {
                    prog: prog.to_string(),
                    args,
                    in_terminal,
                });
            }
        }
    }
    None
}

fn matches_open_target(target: &InstalledTarget, o: &OpenEditor) -> bool {
    if o.tag != target.tag {
        return false;
    }
    if target.id == "vscode-insiders" {
        return o.prog.to_ascii_lowercase().contains("insider");
    }
    if target.id == "vscode" {
        return !o.prog.to_ascii_lowercase().contains("insider");
    }
    true
}

pub struct LaunchMenu {
    pub folder_name: String,
    pub items: Vec<LaunchItem>,
    pub query: String,
    pub rows: Vec<LaunchRow>,
    pub selected: usize,
    pub offset: usize,
    pub right_offset: usize,
    pub cli_in_hub: bool,
    pub in_sidebar: bool,
    pub has_tmux: bool,
    matcher: Matcher,
    utf32_buf: Vec<char>,
}

impl LaunchMenu {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        entry: &Entry,
        installed: &[InstalledTarget],
        open: &[OpenEditor],
        active_session: Option<&Session>,
        global_counts: &HashMap<String, i64>,
        cli_in_hub: bool,
        has_tmux: bool,
        in_sidebar: bool,
    ) -> Self {
        let mut candidates: Vec<InstalledTarget> = installed.to_vec();
        if entry.has_tag("vim") && !candidates.iter().any(|t| t.id == "vim") {
            if let Some(p) = tools::resolve_on_path("vim") {
                candidates.push(InstalledTarget {
                    id: "vim".into(),
                    label: "Vim".into(),
                    tag: "vim".into(),
                    kind: TargetKind::TerminalEditor,
                    method: LaunchMethod::TerminalEditor {
                        prog: p.to_string_lossy().into_owned(),
                        args_before: Vec::new(),
                    },
                });
            }
        }

        let mut editors_raw = Vec::new();
        let mut clis_raw = Vec::new();
        for (idx, target) in candidates.into_iter().enumerate() {
            match target.kind {
                TargetKind::GuiApp | TargetKind::TerminalEditor => editors_raw.push((idx, target)),
                TargetKind::CliAgent => clis_raw.push((idx, target)),
                TargetKind::HubShell | TargetKind::Travel => {}
            }
        }

        let last_editor_tag = entry
            .tags
            .iter()
            .find(|t| editors_raw.iter().any(|(_, ed)| ed.tag == t.tool))
            .map(|t| t.tool.as_str());
        let last_ai_tag = entry
            .tags
            .iter()
            .find(|t| clis_raw.iter().any(|(_, c)| c.tag == t.tool))
            .map(|t| t.tool.as_str());

        let mut editors_scored: Vec<_> = editors_raw
            .into_iter()
            .map(|(idx, target)| {
                let is_open = open.iter().any(|o| matches_open_target(&target, o));
                let tag_info = entry.tags.iter().find(|t| t.tool == target.tag);
                let last_used = tag_info.map_or(0, |t| t.last_used);
                let runs = tag_info.map_or(0, |t| t.count);
                let global = global_counts.get(&target.tag).copied().unwrap_or(0);
                let is_last = last_editor_tag == Some(target.tag.as_str());
                let focus_only = matches!(
                    target.method,
                    LaunchMethod::Gui {
                        focus_only: true,
                        ..
                    }
                );
                let badge = if is_open {
                    "open".into()
                } else if is_last {
                    "last used".into()
                } else if runs > 0 {
                    format!("{runs} run{}", platform::plural(runs))
                } else if focus_only {
                    "focus".into()
                } else {
                    String::new()
                };
                (
                    (is_open, last_used, global),
                    idx,
                    LaunchItem {
                        target,
                        badge,
                        is_open,
                    },
                )
            })
            .collect();
        editors_scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

        let mut clis_scored: Vec<_> = clis_raw
            .into_iter()
            .map(|(idx, target)| {
                let is_live = active_session
                    .is_some_and(|s| s.tool == target.tag && s.status != Status::Done);
                let tag_info = entry.tags.iter().find(|t| t.tool == target.tag);
                let last_used = tag_info.map_or(0, |t| t.last_used);
                let runs = tag_info.map_or(0, |t| t.count);
                let global = global_counts.get(&target.tag).copied().unwrap_or(0);
                let is_last = last_ai_tag == Some(target.tag.as_str());
                let badge = if is_live {
                    "live".into()
                } else if is_last {
                    "last AI".into()
                } else if runs > 0 {
                    format!("{runs} run{}", platform::plural(runs))
                } else {
                    String::new()
                };
                (
                    (is_live, last_used, global),
                    idx,
                    LaunchItem {
                        target,
                        badge,
                        is_open: false,
                    },
                )
            })
            .collect();
        clis_scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

        let mut items: Vec<LaunchItem> = editors_scored
            .into_iter()
            .map(|(_, _, i)| i)
            .chain(clis_scored.into_iter().map(|(_, _, i)| i))
            .collect();

        if in_sidebar {
            items.push(LaunchItem {
                target: InstalledTarget {
                    id: "hub-shell".into(),
                    label: "new shell".into(),
                    tag: "hub".into(),
                    kind: TargetKind::HubShell,
                    method: LaunchMethod::HubShell,
                },
                badge: "workspace".into(),
                is_open: false,
            });
        } else {
            if has_tmux {
                items.push(LaunchItem {
                    target: InstalledTarget {
                        id: "hub-shell".into(),
                        label: "grace hub".into(),
                        tag: "hub".into(),
                        kind: TargetKind::HubShell,
                        method: LaunchMethod::HubShell,
                    },
                    badge: "tmux workspace".into(),
                    is_open: false,
                });
            }
            items.push(LaunchItem {
                target: InstalledTarget {
                    id: "travel".into(),
                    label: "terminal (cd)".into(),
                    tag: "cd".into(),
                    kind: TargetKind::Travel,
                    method: LaunchMethod::Travel,
                },
                badge: "travel here".into(),
                is_open: false,
            });
        }

        let clean_path = platform::sanitize_display(&entry.path);
        let folder_name = crate::picker::split_path(&clean_path).1.to_string();

        let mut menu = Self {
            folder_name,
            items,
            query: String::new(),
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            right_offset: 0,
            cli_in_hub,
            in_sidebar,
            has_tmux,
            matcher: Matcher::new(MatcherConfig::DEFAULT),
            utf32_buf: Vec::new(),
        };
        menu.refilter();
        menu
    }

    pub fn refilter(&mut self) {
        let mut best_item: Option<usize> = None;
        let matching: Vec<usize> = if self.query.trim().is_empty() {
            (0..self.items.len()).collect()
        } else {
            let q = self.query.trim();
            let q_lower = q.to_ascii_lowercase();
            let pat = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
            let mut haystack = String::new();
            let mut scored: Vec<((u8, u32), usize)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    haystack.clear();
                    haystack.push_str(&item.target.label);
                    haystack.push(' ');
                    haystack.push_str(&item.target.tag);
                    haystack.push(' ');
                    haystack.push_str(&item.target.id);
                    if !item.badge.is_empty() {
                        haystack.push(' ');
                        haystack.push_str(&item.badge);
                    }
                    let fuzzy_score = pat.score(
                        Utf32Str::new(&haystack, &mut self.utf32_buf),
                        &mut self.matcher,
                    )?;
                    let exact_rank: u8 = if item.target.label.eq_ignore_ascii_case(q)
                        || item.target.tag.eq_ignore_ascii_case(q)
                    {
                        3
                    } else if item.target.label.to_ascii_lowercase().starts_with(&q_lower)
                        || item.target.tag.to_ascii_lowercase().starts_with(&q_lower)
                    {
                        2
                    } else {
                        1
                    };
                    Some(((exact_rank, fuzzy_score), i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            best_item = scored.first().map(|(_, i)| *i);
            scored.into_iter().map(|(_, i)| i).collect()
        };

        let mut editors = Vec::new();
        let mut clis = Vec::new();
        let mut other = Vec::new();
        for i in matching {
            match self.items[i].target.kind {
                TargetKind::GuiApp | TargetKind::TerminalEditor => editors.push(i),
                TargetKind::CliAgent => clis.push(i),
                TargetKind::HubShell | TargetKind::Travel => other.push(i),
            }
        }

        self.rows.clear();
        if !editors.is_empty() {
            let hdr = if self.in_sidebar {
                "APPS"
            } else {
                "APPS & EDITORS"
            };
            self.rows.push(LaunchRow::Header(hdr));
            self.rows.extend(editors.into_iter().map(LaunchRow::Item));
        }
        if !clis.is_empty() {
            let hdr = if self.in_sidebar {
                "CLI → HUB"
            } else if self.cli_in_hub {
                "CLI AGENTS · HUB"
            } else {
                "CLI AGENTS"
            };
            self.rows.push(LaunchRow::Header(hdr));
            self.rows.extend(clis.into_iter().map(LaunchRow::Item));
        }
        if !other.is_empty() {
            self.rows.push(LaunchRow::Header("OTHER"));
            self.rows.extend(other.into_iter().map(LaunchRow::Item));
        }
        let first_item = self
            .rows
            .iter()
            .position(|r| matches!(r, LaunchRow::Item(_)))
            .unwrap_or(0);
        self.selected = best_item
            .and_then(|bi| {
                self.rows
                    .iter()
                    .position(|r| matches!(r, LaunchRow::Item(idx) if *idx == bi))
            })
            .unwrap_or(first_item);
        self.offset = 0;
        self.right_offset = 0;
    }

    /// Partition row indices into left (apps & editors) and right (CLI agents & other) columns.
    pub fn split_columns(&self) -> (Vec<usize>, Vec<usize>) {
        let mut left = Vec::new();
        let mut right = Vec::new();
        for (ri, row) in self.rows.iter().enumerate() {
            match row {
                LaunchRow::Header(hdr) => {
                    if hdr.starts_with("APPS") {
                        left.push(ri);
                    } else {
                        right.push(ri);
                    }
                }
                LaunchRow::Item(idx) => {
                    if matches!(
                        self.items[*idx].target.kind,
                        TargetKind::GuiApp | TargetKind::TerminalEditor
                    ) {
                        left.push(ri);
                    } else {
                        right.push(ri);
                    }
                }
            }
        }
        (left, right)
    }

    /// Jump selection between the Apps & Editors column and the CLI & Other column.
    pub fn toggle_section(&mut self) {
        let (left, right) = self.split_columns();
        let first_in = |col: &[usize]| {
            col.iter()
                .copied()
                .find(|&ri| matches!(self.rows.get(ri), Some(LaunchRow::Item(_))))
        };
        if left.contains(&self.selected) {
            if let Some(r) = first_in(&right) {
                self.selected = r;
            }
        } else if let Some(l) = first_in(&left) {
            self.selected = l;
        }
    }

    pub fn mv(&mut self, delta: isize) {
        let step = delta.signum();
        let mut left = delta.unsigned_abs();
        let mut i = self.selected as isize;
        while left > 0 {
            i += step;
            if i < 0 || i as usize >= self.rows.len() {
                break;
            }
            if matches!(self.rows[i as usize], LaunchRow::Item(_)) {
                self.selected = i as usize;
                left -= 1;
            }
        }
    }

    pub fn selected_item(&self) -> Option<&LaunchItem> {
        match self.rows.get(self.selected) {
            Some(LaunchRow::Item(i)) => self.items.get(*i),
            _ => None,
        }
    }

    /// Digit hotkey `'1'`..=`'9'` for `row_idx` when `query.is_empty()`.
    pub fn hotkey_for_row(&self, row_idx: usize) -> Option<char> {
        if !self.query.is_empty() || !matches!(self.rows.get(row_idx), Some(LaunchRow::Item(_))) {
            return None;
        }
        let item_pos = self.rows[..row_idx]
            .iter()
            .filter(|r| matches!(r, LaunchRow::Item(_)))
            .count();
        if item_pos < 9 {
            char::from_digit((item_pos + 1) as u32, 10)
        } else {
            None
        }
    }

    /// Instant hotkey lookup: ONLY digits `'1'`..=`'9'` and ONLY when `query.is_empty()`.
    pub fn item_for_hotkey(&self, c: char) -> Option<&LaunchItem> {
        if !self.query.is_empty() {
            return None;
        }
        let idx = match c {
            '1'..='9' => (c as usize) - ('1' as usize),
            _ => return None,
        };
        let item_idx = self
            .rows
            .iter()
            .filter_map(|r| match r {
                LaunchRow::Item(i) => Some(*i),
                LaunchRow::Header(_) => None,
            })
            .nth(idx)?;
        self.items.get(item_idx)
    }

    pub fn delete_word(&mut self) {
        let is_sep = |c: char| c.is_whitespace() || c == '/' || c == '\\';
        let trimmed = self.query.trim_end_matches(is_sep);
        let new_len = trimmed
            .char_indices()
            .rfind(|&(_, c)| is_sep(c))
            .map_or(0, |(i, c)| i + c.len_utf8());
        self.query.truncate(new_len);
        self.refilter();
    }

    pub fn clear_query(&mut self) {
        self.query.clear();
        self.refilter();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Tag;

    fn target(id: &str, label: &str, tag: &str, kind: TargetKind) -> InstalledTarget {
        let method = match kind {
            TargetKind::GuiApp => LaunchMethod::Gui {
                prog: id.into(),
                args_before: Vec::new(),
                pass_path: true,
                focus_only: false,
            },
            TargetKind::TerminalEditor => LaunchMethod::TerminalEditor {
                prog: id.into(),
                args_before: Vec::new(),
            },
            TargetKind::CliAgent => LaunchMethod::CliAgent {
                tool_tag: tag.into(),
                launch_cmd: label.into(),
            },
            TargetKind::HubShell => LaunchMethod::HubShell,
            TargetKind::Travel => LaunchMethod::Travel,
        };
        InstalledTarget {
            id: id.into(),
            label: label.into(),
            tag: tag.into(),
            kind,
            method,
        }
    }

    fn sample_installed() -> Vec<InstalledTarget> {
        vec![
            target(
                "antigravity",
                "Antigravity",
                "antigravity",
                TargetKind::GuiApp,
            ),
            target("vscode", "VS Code", "vscode", TargetKind::GuiApp),
            target("t3code", "T3 Code", "t3code", TargetKind::GuiApp),
            target("cli:claude", "claude", "claude", TargetKind::CliAgent),
            target("cli:opencode", "opencode", "opencode", TargetKind::CliAgent),
            target("cli:codex", "codex", "codex", TargetKind::CliAgent),
        ]
    }

    fn sample_entry() -> Entry {
        Entry {
            path: "/tmp/grace".into(),
            visits: 5,
            last_visit: 1000,
            pinned: false,
            tags: vec![
                Tag {
                    tool: "claude".into(),
                    count: 10,
                    last_used: 900,
                },
                Tag {
                    tool: "vscode".into(),
                    count: 3,
                    last_used: 800,
                },
            ],
        }
    }

    #[test]
    fn ranking_prefers_open_and_last_used_and_live() {
        let e = sample_entry();
        let installed = sample_installed();
        let open = vec![OpenEditor {
            path: "/tmp/grace".into(),
            tag: "antigravity".into(),
            prog: "open".into(),
            args: vec!["-a".into(), "Antigravity".into()],
        }];
        let sess = Session {
            id: 1,
            path: "/tmp/grace".into(),
            tool: "opencode".into(),
            status: Status::Waiting,
            shell_pid: None,
            shell_start: None,
            agent_pid: None,
            agent_start: None,
            started_at: 950,
            updated_at: 990,
            session_id: None,
            detail: None,
            tmux_pane: None,
        };
        let mut global = HashMap::new();
        global.insert("codex".into(), 50);

        let menu = LaunchMenu::new(
            &e,
            &installed,
            &open,
            Some(&sess),
            &global,
            true,
            true,
            false,
        );

        let editor_labels: Vec<&str> = menu
            .items
            .iter()
            .filter(|i| i.target.kind == TargetKind::GuiApp)
            .map(|i| i.target.label.as_str())
            .collect();
        // Open editor first, then folder-used vscode, then remaining t3code.
        assert_eq!(editor_labels, vec!["Antigravity", "VS Code", "T3 Code"]);
        assert_eq!(menu.items[0].badge, "open");
        assert_eq!(menu.items[1].badge, "last used");

        let cli_labels: Vec<&str> = menu
            .items
            .iter()
            .filter(|i| i.target.kind == TargetKind::CliAgent)
            .map(|i| i.target.label.as_str())
            .collect();
        // Live session first (opencode), then folder-used (claude), then global count (codex).
        assert_eq!(cli_labels, vec!["opencode", "claude", "codex"]);
        assert_eq!(menu.items[3].badge, "live");
        assert_eq!(menu.items[4].badge, "last AI");
    }

    #[test]
    fn filter_and_digit_hotkeys_work_together() {
        let e = sample_entry();
        let menu_installed = sample_installed();
        let mut menu = LaunchMenu::new(
            &e,
            &menu_installed,
            &[],
            None,
            &HashMap::new(),
            true,
            true,
            false,
        );
        // Query empty: digits 1..9 select rows cleanly, letters are NOT hotkeys.
        assert_eq!(menu.item_for_hotkey('1').unwrap().target.id, "vscode");
        assert!(menu.item_for_hotkey('t').is_none());
        assert!(menu.item_for_hotkey('h').is_none());

        // Typing "t3" filters to T3 Code without triggering hotkey '3'.
        menu.query.push('t');
        menu.refilter();
        assert!(menu.item_for_hotkey('3').is_none());
        menu.query.push('3');
        menu.refilter();
        assert_eq!(menu.selected_item().unwrap().target.id, "t3code");

        // Filtering "opencode" selects opencode.
        menu.clear_query();
        menu.query = "opencode".into();
        menu.refilter();
        assert_eq!(menu.selected_item().unwrap().target.id, "cli:opencode");
    }

    #[test]
    fn build_editor_choice_rejects_unsafe_paths() {
        let t = target("vscode", "VS Code", "vscode", TargetKind::GuiApp);
        assert!(build_editor_choice(&t, "relative/dir", None).is_err());
        assert!(build_editor_choice(&t, "-rf", None).is_err());
        assert!(build_editor_choice(&t, "/tmp/bad\npath", None).is_err());
        assert!(build_editor_choice(&t, "/tmp/bad\0path", None).is_err());

        // "/tmp/..." is not absolute on Windows.
        let clean = if cfg!(windows) {
            r"C:\tmp\clean project"
        } else {
            "/tmp/clean project"
        };
        let ok = build_editor_choice(&t, clean, None).unwrap();
        assert_eq!(ok.prog, "vscode");
        assert_eq!(ok.args, vec![clean]);
        assert!(!ok.in_terminal);
    }

    // macOS `open -a` paths; Unix-style absolute folders.
    #[test]
    #[cfg(unix)]
    fn focus_only_gui_omits_path_when_configured() {
        let t = InstalledTarget {
            id: "t3code".into(),
            label: "T3 Code".into(),
            tag: "t3code".into(),
            kind: TargetKind::GuiApp,
            method: LaunchMethod::Gui {
                prog: "open".into(),
                args_before: vec!["-a".into(), "/Applications/T3 Code.app".into()],
                pass_path: false,
                focus_only: true,
            },
        };
        let ed = build_editor_choice(&t, "/tmp/myproj", None).unwrap();
        assert_eq!(ed.prog, "open");
        assert_eq!(ed.args, vec!["-a", "/Applications/T3 Code.app"]);
    }

    #[test]
    fn claude_cli_tag_does_not_boost_claude_desktop() {
        let e = Entry {
            path: "/tmp/proj".into(),
            visits: 5,
            last_visit: 1000,
            pinned: false,
            tags: vec![
                Tag {
                    tool: "claude".into(),
                    count: 20,
                    last_used: 990,
                },
                Tag {
                    tool: "vscode".into(),
                    count: 2,
                    last_used: 500,
                },
            ],
        };
        let mut installed = vec![target(
            "claude-desktop",
            "Claude Desktop",
            "claude-desktop",
            TargetKind::GuiApp,
        )];
        installed.extend(sample_installed());

        let mut global = HashMap::new();
        global.insert("claude".into(), 100);
        global.insert("vscode".into(), 50);

        let mut menu = LaunchMenu::new(&e, &installed, &[], None, &global, true, true, false);
        let editor_order: Vec<(&str, &str)> = menu
            .items
            .iter()
            .filter(|i| i.target.kind == TargetKind::GuiApp)
            .map(|i| (i.target.id.as_str(), i.badge.as_str()))
            .collect();
        // VS Code (used on folder) ranks ahead of Claude Desktop; Claude Desktop must NOT get "last used".
        assert_eq!(editor_order[0], ("vscode", "last used"));
        let desktop = editor_order
            .iter()
            .find(|(id, _)| *id == "claude-desktop")
            .unwrap();
        assert_eq!(desktop.1, "");

        // Typing "claude" selects the exact CLI match (`cli:claude`), NOT Claude Desktop.
        menu.query = "claude".into();
        menu.refilter();
        assert_eq!(menu.selected_item().unwrap().target.id, "cli:claude");

        // Typing "claude d" matches Claude Desktop specifically.
        menu.query = "claude d".into();
        menu.refilter();
        assert_eq!(menu.selected_item().unwrap().target.id, "claude-desktop");
    }

    #[test]
    fn split_columns_and_toggle_section() {
        let e = sample_entry();
        let installed = sample_installed();
        let mut menu = LaunchMenu::new(
            &e,
            &installed,
            &[],
            None,
            &HashMap::new(),
            true,
            true,
            false,
        );
        let (left, right) = menu.split_columns();
        assert!(!left.is_empty());
        assert!(!right.is_empty());
        assert!(left.iter().all(|&ri| match menu.rows[ri] {
            LaunchRow::Header(h) => h.starts_with("APPS"),
            LaunchRow::Item(idx) => matches!(
                menu.items[idx].target.kind,
                TargetKind::GuiApp | TargetKind::TerminalEditor
            ),
        }));
        assert!(right.iter().all(|&ri| match menu.rows[ri] {
            LaunchRow::Header(h) => !h.starts_with("APPS"),
            LaunchRow::Item(idx) => !matches!(
                menu.items[idx].target.kind,
                TargetKind::GuiApp | TargetKind::TerminalEditor
            ),
        }));
        assert!(left.contains(&menu.selected));
        menu.toggle_section();
        assert!(right.contains(&menu.selected));
        menu.toggle_section();
        assert!(left.contains(&menu.selected));
    }

    // macOS `open -a` paths; Unix-style absolute folders.
    #[test]
    #[cfg(unix)]
    fn gui_url_encodes_folder_into_claude_code_deeplink() {
        let t = InstalledTarget {
            id: "claude-desktop".into(),
            label: "Claude Desktop".into(),
            tag: "claude-desktop".into(),
            kind: TargetKind::GuiApp,
            method: LaunchMethod::GuiUrl {
                prog: "open".into(),
                args_before: vec!["-a".into(), "/Applications/Claude.app".into()],
                url_prefix: "claude://code/new?folder=".into(),
            },
        };
        assert!(build_editor_choice(&t, "relative/dir", None).is_err());
        assert!(build_editor_choice(&t, "/tmp/bad\npath", None).is_err());

        let ed = build_editor_choice(&t, "/tmp/my proj#1", None).unwrap();
        assert_eq!(ed.prog, "open");
        assert_eq!(
            ed.args,
            vec![
                "-a",
                "/Applications/Claude.app",
                "claude://code/new?folder=%2Ftmp%2Fmy%20proj%231",
            ]
        );
        assert!(!ed.in_terminal);
    }

    #[test]
    fn codex_cli_tag_does_not_boost_codex_app_and_filter_matches_both() {
        let e = Entry {
            path: "/tmp/proj".into(),
            visits: 5,
            last_visit: 1000,
            pinned: false,
            tags: vec![
                Tag {
                    tool: "codex".into(),
                    count: 20,
                    last_used: 990,
                },
                Tag {
                    tool: "vscode".into(),
                    count: 2,
                    last_used: 500,
                },
            ],
        };
        let mut installed = vec![target(
            "codex-app",
            "Codex (ChatGPT)",
            "codex-app",
            TargetKind::GuiApp,
        )];
        installed.extend(sample_installed());

        let mut global = HashMap::new();
        global.insert("codex".into(), 100);
        global.insert("vscode".into(), 50);

        let mut menu = LaunchMenu::new(&e, &installed, &[], None, &global, true, true, false);
        let editor_order: Vec<(&str, &str)> = menu
            .items
            .iter()
            .filter(|i| i.target.kind == TargetKind::GuiApp)
            .map(|i| (i.target.id.as_str(), i.badge.as_str()))
            .collect();
        // VS Code ranks ahead of Codex (ChatGPT); codex CLI tag must NOT grant "last used" to codex-app.
        assert_eq!(editor_order[0], ("vscode", "last used"));
        let codex_gui = editor_order
            .iter()
            .find(|(id, _)| *id == "codex-app")
            .unwrap();
        assert_eq!(codex_gui.1, "");

        // Filtering "codex" selects the CLI agent (`cli:codex`).
        menu.query = "codex".into();
        menu.refilter();
        assert_eq!(menu.selected_item().unwrap().target.id, "cli:codex");

        // Filtering "chatgpt" selects the Desktop app (`codex-app`).
        menu.query = "chatgpt".into();
        menu.refilter();
        assert_eq!(menu.selected_item().unwrap().target.id, "codex-app");
    }
}
