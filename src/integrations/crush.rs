//! crush (charmbracelet/crush): `hook add` lines in the global `crushrc`.
//!
//! Verified (docs/hooks/README.md and README.md, fetched 2026-11-19):
//! - The global config is `~/.config/crush/crushrc` and crush respects `XDG_CONFIG_HOME`.
//!   A `crushrc` is Bash with crush builtins. Each hook is one
//!   `hook add <Event> --name N --command C` line. The old JSON config (`crush.json`) still
//!   works but the README calls it deprecated, so we edit `crushrc` as text.
//! - Events used: SessionStart (Ready), TurnStart (Working), PermissionRequest (Waiting),
//!   PermissionResult (Working), TurnEnd (Ready), SessionEnd (Done). We also add Interrupt and
//!   StopFailure (Ready): the docs do not say that TurnEnd fires after a cancel or a provider
//!   error, and without them the status could stay at Working.
//!   All eight are observe-only events, so crush ignores our output.
//! - Stdin JSON has `event`, `hook_event_name` and `cwd`. Event names are case-insensitive and
//!   accept snake_case. Exit 0 with empty stdout means "no opinion".
//! - Global hooks must use an absolute path or an inline command.
//!
//! Assumed / not handled: `CRUSH_GLOBAL_CONFIG` can move the user config, but the docs do not
//! say if it names a file or a folder, so we ignore it. The `--command` value goes through two
//! shells (crushrc, then the hook shell), so we single-quote the already-quoted command.
//! Our lines are the header and each `hook add ... --name grace` line that calls
//! `session hook crush`. Remove deletes only those lines.
use super::{HookResult, Integration};
use crate::model::Status;
use crate::setup::{read_opt, shell_quote, write_atomic};
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub static INTEGRATION: Integration = Integration {
    name: "crush",
    tool: "crush",
    setup,
    reads_stdin: true,
    hook,
    stdout: "",
};

const HEADER: &str = "# managed by grace. Remove with: grace setup crush --remove";
const EVENTS: [&str; 8] = [
    "SessionStart",
    "TurnStart",
    "PermissionRequest",
    "PermissionResult",
    "TurnEnd",
    "Interrupt",
    "StopFailure",
    "SessionEnd",
];

/// `$GRACE_CRUSH_DIR/crushrc`, else `$XDG_CONFIG_HOME/crush/crushrc`, else `~/.config/crush/crushrc`.
fn rc_path() -> Result<PathBuf> {
    for (var, sub) in [("GRACE_CRUSH_DIR", ""), ("XDG_CONFIG_HOME", "crush")] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                return Ok(PathBuf::from(v).join(sub).join("crushrc"));
            }
        }
    }
    crate::platform::home_dir()
        .map(|h| h.join(".config").join("crush").join("crushrc"))
        .ok_or_else(|| anyhow!("cannot find the home folder"))
}

fn setup(exe: &str, remove: bool) -> Result<String> {
    let p = rc_path()?;
    if remove {
        remove_at(&p)
    } else {
        add_at(&p, exe)
    }
}

fn is_ours(line: &str) -> bool {
    let t = line.trim();
    t == HEADER
        || (t.starts_with("hook add ") && t.contains("--name grace --command") && t.contains("session hook crush"))
}

fn block(exe: &str) -> String {
    let cmd = shell_quote(&format!("{} session hook crush", super::gemini::quote_once(exe)));
    let mut s = format!("{HEADER}\n");
    for e in EVENTS {
        s.push_str(&format!("hook add {e} --name grace --command {cmd}\n"));
    }
    s
}

fn strip_ours(text: &str) -> String {
    let mut out: String = text.lines().filter(|l| !is_ours(l)).map(|l| format!("{l}\n")).collect();
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

fn add_at(path: &Path, exe: &str) -> Result<String> {
    let text = read_opt(path)?.unwrap_or_default();
    let mut new = strip_ours(&text);
    if !new.is_empty() {
        new.push('\n');
    }
    new.push_str(&block(exe));
    if new == text {
        return Ok(format!("{}: grace hooks already set up. Nothing to do.", path.display()));
    }
    write_atomic(path, &new)?;
    Ok(format!(
        "Updated {}\n  hooks: {}\nRestart running crush sessions to pick this up.",
        path.display(),
        EVENTS.join(", ")
    ))
}

fn remove_at(path: &Path) -> Result<String> {
    let Some(text) = read_opt(path)? else {
        return Ok(format!("{} does not exist. Nothing to do.", path.display()));
    };
    if !text.lines().any(is_ours) {
        return Ok(format!("{}: no grace hooks found. Nothing to do.", path.display()));
    }
    write_atomic(path, &strip_ours(&text))?;
    Ok(format!(
        "Updated {}\n  removed grace hooks\nRestart running crush sessions to pick this up.",
        path.display()
    ))
}

fn hook(_args: &[String], stdin: &str) -> HookResult {
    let Ok(v) = serde_json::from_str::<Value>(stdin) else {
        return HookResult::default();
    };
    let ev = v
        .get("hook_event_name")
        .or_else(|| v.get("event"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let status = match ev.to_ascii_lowercase().replace('_', "").as_str() {
        "turnstart" | "permissionresult" => Status::Working,
        "permissionrequest" => Status::Waiting,
        "turnend" | "sessionstart" | "interrupt" | "stopfailure" => Status::Ready,
        "sessionend" => Status::Done,
        _ => return HookResult::default(),
    };
    let cwd = v.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()).map(String::from);
    HookResult { status: Some(status), cwd }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_idempotent_remove() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("crush").join("crushrc");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mine = "# my stuff\nhook add PreToolUse --command 'x'\n";
        std::fs::write(&p, mine).unwrap();
        add_at(&p, "/bin/grace").unwrap();
        let once = std::fs::read_to_string(&p).unwrap();
        assert!(once.starts_with(mine));
        assert!(once.contains("hook add TurnEnd --name grace --command '/bin/grace session hook crush'"));
        assert!(add_at(&p, "/bin/grace").unwrap().contains("Nothing to do"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), once);
        // A new exe path replaces our lines instead of adding more.
        add_at(&p, "/new/grace").unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert_eq!(t.matches("hook add TurnEnd").count(), 1);
        assert!(!t.contains("/bin/grace"));
        remove_at(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), mine);
        assert!(remove_at(&p).unwrap().contains("Nothing to do"));
    }

    #[test]
    fn quoted_exe_survives_two_shells() {
        let b = block("'/a b/grace'");
        assert!(b.contains(r#"--command ''\''/a b/grace'\'' session hook crush'"#));
    }

    #[test]
    fn parse() {
        let r = hook(&[], r#"{"hook_event_name":"PermissionRequest","cwd":"/a"}"#);
        assert_eq!(r.status, Some(Status::Waiting));
        assert_eq!(r.cwd.as_deref(), Some("/a"));
        assert_eq!(hook(&[], r#"{"event":"session_end"}"#).status, Some(Status::Done));
        assert_eq!(hook(&[], r#"{"event":"TurnStart"}"#).status, Some(Status::Working));
        assert_eq!(hook(&[], r#"{"event":"TurnEnd","cwd":""}"#).cwd, None);
        assert_eq!(hook(&[], "nope"), HookResult::default());
        assert_eq!(hook(&[], ""), HookResult::default());
        assert_eq!(hook(&[], r#"{"event":5}"#), HookResult::default());
        assert_eq!(hook(&[], r#"{"hook_event_name":"PreToolUse"}"#).status, None);
    }
}
