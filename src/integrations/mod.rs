//! One module per CLI agent. Each one installs/removes its hooks (`grace setup <name>`)
//! and turns a hook call (`grace session hook <name> ARGS`, maybe JSON on stdin) into a status.
//! Claude and Codex live in setup.rs / session.rs; everything newer lives here.
use crate::model::Status;
use anyhow::Result;

pub mod agy;
pub mod aider;
pub mod amp;
pub mod copilot;
pub mod crush;
pub mod gemini;
pub mod goose;
pub mod opencode;
pub mod qwen;

/// What one hook call tells grace.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct HookResult {
    /// New status, or None to ignore this event.
    pub status: Option<Status>,
    /// Project folder from the event. None = use the hook process's current folder.
    pub cwd: Option<String>,
}

pub struct Integration {
    /// Setup target and hook name, e.g. "gemini".
    pub name: &'static str,
    /// grace tag for the sessions, e.g. "gemini" (goes through the tool registry).
    pub tool: &'static str,
    /// Install (remove = false) or remove the hooks. `exe` is the absolute grace path
    /// (already shell-quoted if needed). Returns a short summary for stderr.
    pub setup: fn(exe: &str, remove: bool) -> Result<String>,
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
