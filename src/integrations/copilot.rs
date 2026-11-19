//! copilot: GitHub Copilot CLI. We own one file, `~/.copilot/hooks/grace.json`.
//!
//! Verified in the docs (https://docs.github.com/en/copilot/reference/hooks-configuration):
//! - Every `*.json` file in the user hooks folder loads. The folder is `~/.copilot/hooks/`,
//!   or `$COPILOT_HOME/hooks/` if `COPILOT_HOME` is set.
//! - Format: `{"version":1,"hooks":{"<event>":[{"type":"command","bash":"..","powershell":".."}]}}`.
//!   Default `timeoutSec` is 30.
//! - The payload is JSON on stdin with `cwd`. With a camelCase event name (what we install) the
//!   payload has NO event name. With a PascalCase name it has `hook_event_name`. So we put the
//!   event name in the command as an extra arg (`... session hook copilot agentStop`) and read it
//!   from `args`. If the arg is missing, we use `hook_event_name` from the payload.
//! - `notification` is fire-and-forget and has `notification_type`. `permission_prompt` and
//!   `elicitation_dialog` mean Waiting. Other types (shell_completed, agent_idle of a background
//!   agent, ...) are ignored.
//! - Stdout: "If the leftover output is empty ... falls through to default behavior". So `stdout: ""`.
//! - `permissionRequest` fires BEFORE the permission service runs (rules, session approvals,
//!   auto-allow). It can fire for a tool that never asks the user. We still map it to Waiting
//!   (from the brief). The next postToolUse sets Working again.
//!
//! Assumed: the powershell command is the same text. If the exe is quoted, we add `& ` in front.
//! Event `notification` is an addition to the brief. It catches real prompts and questions.
//! `GRACE_COPILOT_DIR` (grace only) wins over `COPILOT_HOME`, then `~/.copilot`.
use super::{HookResult, Integration};
use crate::model::Status;
use crate::setup::{env_dir, read_opt, write_atomic};
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub static INTEGRATION: Integration = Integration {
    name: "copilot",
    tool: "copilot",
    setup,
    reads_stdin: true,
    hook,
    stdout: "",
};

const MARK: &str = "session hook copilot";
const EVENTS: &[&str] = &[
    "sessionStart",
    "userPromptSubmitted",
    "preToolUse",
    "postToolUse",
    "notification",
    "agentStop",
    "sessionEnd",
];

fn hooks_file() -> Result<PathBuf> {
    let base = match std::env::var("GRACE_COPILOT_DIR") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => env_dir("COPILOT_HOME", ".copilot")?,
    };
    Ok(base.join("hooks").join("grace.json"))
}

fn setup(exe: &str, remove: bool) -> Result<String> {
    let path = hooks_file()?;
    if remove {
        remove_file(&path)
    } else {
        add_file(&path, exe)
    }
}

fn desired(exe: &str) -> Value {
    let exe = super::gemini::quote_once(exe);
    let ps_prefix = if exe.starts_with(['\'', '"']) { "& " } else { "" };
    let mut hooks = Map::new();
    for ev in EVENTS {
        hooks.insert(
            ev.to_string(),
            json!([{
                "type": "command",
                "bash": format!("{exe} {MARK} {ev}"),
                "powershell": format!("{ps_prefix}{exe} {MARK} {ev}"),
            }]),
        );
    }
    json!({ "version": 1, "hooks": hooks })
}

fn add_file(path: &Path, exe: &str) -> Result<String> {
    let want = desired(exe);
    if let Some(old) = read_opt(path)? {
        if !old.contains(MARK) {
            bail!("{} exists and is not a grace file. Nothing changed", path.display());
        }
        if serde_json::from_str::<Value>(&old).ok().as_ref() == Some(&want) {
            return Ok(format!("{}: grace hooks already set up. Nothing to do.", path.display()));
        }
    }
    let mut out = serde_json::to_string_pretty(&want)?;
    out.push('\n');
    write_atomic(path, &out)?;
    Ok(format!(
        "Wrote {}\n  events: {}\nRestart running Copilot CLI sessions to pick this up.",
        path.display(),
        EVENTS.join(", ")
    ))
}

fn remove_file(path: &Path) -> Result<String> {
    match read_opt(path)? {
        None => Ok(format!("{} does not exist. Nothing to do.", path.display())),
        Some(t) if !t.contains(MARK) => {
            Ok(format!("{}: not a grace file. Left as it is. Nothing to do.", path.display()))
        }
        Some(_) => {
            std::fs::remove_file(path)?;
            Ok(format!(
                "Removed {}\nRestart running Copilot CLI sessions to pick this up.",
                path.display()
            ))
        }
    }
}

fn status_for(event: &str, ntype: &str) -> Option<Status> {
    // Accept PascalCase too (same event, VS Code style name).
    let mut e = event.to_string();
    if let Some(c) = e.get_mut(0..1) {
        c.make_ascii_lowercase();
    }
    match e.as_str() {
        "userPromptSubmitted" | "userPromptSubmit" | "preToolUse" | "postToolUse" => {
            Some(Status::Working)
        }
        // permissionRequest also fires for auto-allowed tools, so it is not a reliable "needs you".
        "permissionRequest" => None,
        "notification" => match ntype {
            "permission_prompt" | "elicitation_dialog" => Some(Status::Waiting),
            _ => None,
        },
        "agentStop" | "sessionStart" => Some(Status::Ready),
        "sessionEnd" => Some(Status::Done),
        _ => None,
    }
}

/// Bad or non-object stdin gives status None (Copilot always sends a JSON object).
fn hook(args: &[String], stdin: &str) -> HookResult {
    let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(stdin) else {
        return HookResult::default();
    };
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
    let event = match args.first() {
        Some(a) if !a.is_empty() => a.as_str(),
        _ => text("hook_event_name"),
    };
    HookResult {
        status: status_for(event, text("notification_type")),
        cwd: Some(text("cwd")).filter(|s| !s.is_empty()).map(String::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const EXE: &str = "/opt/grace/bin/grace";

    fn a(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    #[test]
    fn add_idempotent_update_remove() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("hooks").join("grace.json");
        let msg = add_file(&p, EXE).unwrap();
        assert!(msg.ends_with("Restart running Copilot CLI sessions to pick this up."));
        let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(
            v["hooks"]["agentStop"][0]["bash"],
            "/opt/grace/bin/grace session hook copilot agentStop"
        );
        assert_eq!(v["hooks"].as_object().unwrap().len(), EVENTS.len());
        assert!(add_file(&p, EXE).unwrap().ends_with("already set up. Nothing to do."));
        add_file(&p, "/new/grace").unwrap();
        assert!(fs::read_to_string(&p).unwrap().contains("/new/grace session hook"));
        remove_file(&p).unwrap();
        assert!(!p.exists());
        assert!(remove_file(&p).unwrap().contains("Nothing to do"));
    }

    #[test]
    fn foreign_file_is_kept() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("grace.json");
        fs::write(&p, r#"{"version":1,"hooks":{}}"#).unwrap();
        assert!(add_file(&p, EXE).is_err());
        remove_file(&p).unwrap();
        assert!(p.exists());
    }

    #[test]
    fn quoted_exe_in_powershell() {
        let v = desired("'/a b/grace'");
        assert!(v["hooks"]["sessionEnd"][0]["powershell"].as_str().unwrap().starts_with("& '"));
        assert!(v["hooks"]["sessionEnd"][0]["bash"].as_str().unwrap().starts_with("'/a b"));
        if cfg!(unix) {
            let v = desired("/a b/grace");
            assert_eq!(v["hooks"]["sessionEnd"][0]["bash"], "'/a b/grace' session hook copilot sessionEnd");
        }
    }

    #[test]
    fn remove_keeps_other_hook_files() {
        let d = tempfile::tempdir().unwrap();
        let other = d.path().join("hooks").join("mine.json");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        fs::write(&other, r#"{"version":1,"hooks":{"agentStop":[{"type":"command","bash":"echo hi"}]}}"#).unwrap();
        let p = d.path().join("hooks").join("grace.json");
        add_file(&p, EXE).unwrap();
        remove_file(&p).unwrap();
        assert!(!p.exists());
        assert!(fs::read_to_string(&other).unwrap().contains("echo hi"));
    }

    #[test]
    fn event_mapping() {
        let s = status_for;
        for e in ["userPromptSubmitted", "preToolUse", "postToolUse"] {
            assert_eq!(s(e, ""), Some(Status::Working));
        }
        assert_eq!(s("permissionRequest", ""), None);
        assert_eq!(s("notification", "elicitation_dialog"), Some(Status::Waiting));
        assert_eq!(s("notification", "shell_completed"), None);
        assert_eq!(s("agentStop", ""), Some(Status::Ready));
        assert_eq!(s("sessionStart", ""), Some(Status::Ready));
        assert_eq!(s("SessionStart", ""), Some(Status::Ready));
        assert_eq!(s("sessionEnd", ""), Some(Status::Done));
        assert_eq!(s("errorOccurred", ""), None);
        assert_eq!(s("", ""), None);
    }

    #[test]
    fn hook_parse() {
        let r = hook(&a("agentStop"), r#"{"sessionId":"1","timestamp":1,"cwd":"/c"}"#);
        assert_eq!(r.status, Some(Status::Ready));
        assert_eq!(r.cwd.as_deref(), Some("/c"));
        let r = hook(&[], r#"{"hook_event_name":"SessionEnd","cwd":"/c"}"#);
        assert_eq!(r.status, Some(Status::Done));
        let r = hook(&a("notification"), r#"{"cwd":"/c","hook_event_name":"Notification","notification_type":"permission_prompt"}"#);
        assert_eq!(r.status, Some(Status::Waiting));
        assert_eq!(hook(&a("agentStop"), "bad"), HookResult::default());
        assert_eq!(hook(&a("agentStop"), "").status, None);
        assert_eq!(hook(&[], "bad"), HookResult::default());
        assert_eq!(hook(&a("agentStop"), "[1]").status, None);
    }
}
