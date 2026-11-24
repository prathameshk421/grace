//! One module per CLI agent. Each one installs/removes its hooks (`grace setup <name>`)
//! and turns a hook call (`grace session hook <name> ARGS`, maybe JSON on stdin) into a status.
//! Claude and Codex live in setup.rs / session.rs; everything newer lives here.
use crate::model::Status;
use anyhow::Result;
use std::path::{Path, PathBuf};

pub mod agy;
pub mod aider;
pub mod amp;
pub mod copilot;
pub mod crush;
pub mod gemini;
pub mod goose;
pub mod opencode;
pub mod qwen;

/// Diagnostic state of one CLI agent integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookStatus {
    Connected(PathBuf),
    OutdatedPath(PathBuf),
    Available(PathBuf),
    NotInstalled,
    Conflict(PathBuf, String),
}

/// What one hook call tells grace.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct HookResult {
    /// New status, or None to ignore this event.
    pub status: Option<Status>,
    /// Project folder from the event. None = use the hook process's current folder.
    pub cwd: Option<String>,
    /// Agent-reported session identifier, when available.
    pub session_id: Option<String>,
    /// Concise detail, such as tool name or prompt kind.
    pub detail: Option<String>,
}

pub struct Integration {
    /// Setup target and hook name, e.g. "gemini".
    pub name: &'static str,
    /// Human-readable display label, e.g. "Gemini CLI".
    pub label: &'static str,
    /// grace tag for the sessions, e.g. "gemini" (goes through the tool registry).
    pub tool: &'static str,
    /// Install (remove = false) or remove the hooks. `exe` is the unquoted absolute grace path.
    pub setup: fn(exe: &str, remove: bool) -> Result<String>,
    /// Inspect current config without modifying disk. `exe` is the unquoted absolute grace path.
    pub check: fn(exe: &str) -> HookStatus,
    /// true if the CLI sends the event as JSON on stdin. If false, grace never reads stdin
    /// (it may be the user's terminal).
    pub reads_stdin: bool,
    /// Parse one hook call. `args` = everything after `grace session hook <name>`.
    /// `stdin` = the stdin text (empty if reads_stdin is false). Must never panic.
    pub hook: fn(args: &[String], stdin: &str) -> HookResult,
    /// Text the CLI needs on stdout after the hook (e.g. "{}" for CLIs that parse JSON). "" = nothing.
    pub stdout: &'static str,
}

pub fn all() -> [&'static Integration; 9] {
    [
        &gemini::INTEGRATION,
        &qwen::INTEGRATION,
        &copilot::INTEGRATION,
        &opencode::INTEGRATION,
        &agy::INTEGRATION,
        &crush::INTEGRATION,
        &goose::INTEGRATION,
        &amp::INTEGRATION,
        &aider::INTEGRATION,
    ]
}

pub fn find(name: &str) -> Option<&'static Integration> {
    all().into_iter().find(|i| i.name == name)
}

pub(super) fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

pub(super) fn ensure_not_symlink(p: &Path) -> Result<()> {
    if is_symlink(p) {
        anyhow::bail!("refusing to touch symlink {}", p.display());
    }
    Ok(())
}

pub(super) fn plugin_dir_check(
    dir: &Path,
    hp: PathBuf,
    avail_dir: Option<&Path>,
    tool: &str,
    is_ours: fn(&Path) -> bool,
    desired_hooks: &str,
) -> HookStatus {
    if is_symlink(dir) {
        return HookStatus::Conflict(hp, "symlink plugin folder".into());
    }
    if !dir.exists() {
        if avail_dir.is_some_and(|p| p.is_dir()) || crate::tools::on_path(tool) {
            return HookStatus::Available(hp);
        }
        return HookStatus::NotInstalled;
    }
    if !is_ours(dir) {
        return HookStatus::Conflict(hp, "foreign plugin folder".into());
    }
    if crate::setup::read_opt(&hp).ok().flatten().as_deref() == Some(desired_hooks) {
        HookStatus::Connected(hp)
    } else {
        HookStatus::OutdatedPath(hp)
    }
}

pub(super) fn managed_file_check(
    path: PathBuf,
    avail_dir: Option<&Path>,
    tool: &str,
    conflict_label: &str,
    is_ours: fn(&str) -> bool,
    matches: impl FnOnce(&str) -> bool,
) -> HookStatus {
    let Ok(Some(cur)) = crate::setup::read_opt(&path) else {
        if avail_dir.is_some_and(|p| p.is_dir()) || crate::tools::on_path(tool) {
            return HookStatus::Available(path);
        }
        return HookStatus::NotInstalled;
    };
    if !is_ours(&cur) {
        return HookStatus::Conflict(path, conflict_label.into());
    }
    if matches(&cur) {
        HookStatus::Connected(path)
    } else {
        HookStatus::OutdatedPath(path)
    }
}

pub(super) fn simple_arg_hook(args: &[String], map: fn(&str) -> Option<Status>) -> HookResult {
    let Some(status) = args.first().and_then(|s| map(s.as_str())) else {
        return HookResult::default();
    };
    let cwd = args.get(1).filter(|d| !d.is_empty()).cloned();
    HookResult {
        status: Some(status),
        cwd,
        session_id: None,
        detail: None,
    }
}

pub(super) fn remove_owned_backup(path: &Path, mark: &str) {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let backup = path.with_file_name(format!("{name}.grace-backup"));
    if !is_symlink(&backup) {
        if let Ok(Some(text)) = crate::setup::read_opt(&backup) {
            if text.contains(mark) {
                let _ = std::fs::remove_file(&backup);
            }
        }
    }
}
