//! gemini: Gemini CLI. Hooks go in `~/.gemini/settings.json` (Claude-style `hooks` map).
use super::{HookResult, HookStatus, Integration};
use crate::model::Status;
use crate::platform;
use crate::session::non_empty_str;
use crate::setup::{env_dir, json_hooks_add, json_hooks_check, json_hooks_remove};
use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;

pub static INTEGRATION: Integration = Integration {
    name: "gemini",
    label: "Gemini CLI",
    tool: "gemini",
    setup,
    check,
    reads_stdin: true,
    hook,
    stdout: "{}",
};

/// Events we install (no matcher, so each one fires for everything).
const EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("BeforeAgent", None),
    ("BeforeTool", None),
    ("AfterTool", None),
    ("AfterAgent", None),
    ("Notification", None),
    ("SessionEnd", None),
];

fn settings_path() -> Result<PathBuf> {
    Ok(env_dir("GRACE_GEMINI_DIR", ".gemini")?.join("settings.json"))
}

fn setup(exe: &str, remove: bool) -> Result<String> {
    let path = settings_path()?;
    if remove {
        json_hooks_remove(&path, "gemini", "Gemini CLI")
    } else {
        json_hooks_add(&path, exe, "gemini", EVENTS, "Gemini CLI")
    }
}

fn check(exe: &str) -> HookStatus {
    let Ok(path) = settings_path() else {
        return HookStatus::NotInstalled;
    };
    json_hooks_check(&path, exe, "gemini", EVENTS)
}

fn status_for(event: &str, ntype: &str) -> Option<Status> {
    match event {
        "BeforeAgent" | "BeforeTool" | "AfterTool" => Some(Status::Working),
        "AfterAgent" | "SessionStart" => Some(Status::Ready),
        "Notification" if ntype.to_ascii_lowercase().contains("idle") => Some(Status::Ready),
        "Notification" => Some(Status::Waiting),
        "SessionEnd" => Some(Status::Done),
        _ => None,
    }
}

fn hook(_args: &[String], stdin: &str) -> HookResult {
    parse_event_json(stdin, status_for)
}

/// Parse a Claude-style hook JSON (`hook_event_name`, `cwd`, `notification_type`, `session_id`, `tool_name`).
/// Bad JSON or an unknown event gives status None.
pub(super) fn parse_event_json(stdin: &str, map: fn(&str, &str) -> Option<Status>) -> HookResult {
    let Ok(v) = serde_json::from_str::<Value>(stdin) else {
        return HookResult::default();
    };
    let ev = non_empty_str(&v, "hook_event_name").unwrap_or_default();
    let ntype = non_empty_str(&v, "notification_type").unwrap_or_default();
    let status = map(&ev, &ntype);
    let detail = match status {
        Some(Status::Waiting) if !ntype.is_empty() => {
            Some(platform::sanitize_display(&ntype).into_owned())
        }
        Some(Status::Working) => {
            non_empty_str(&v, "tool_name").map(|t| platform::sanitize_display(&t).into_owned())
        }
        _ => None,
    };
    HookResult {
        status,
        cwd: non_empty_str(&v, "cwd"),
        session_id: non_empty_str(&v, "session_id"),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::Path;

    const EXE: &str = "/opt/grace/bin/grace";
    const MARK: &str = "session hook gemini";

    fn run(p: &Path, f: impl Fn(&Path) -> Result<String>) -> String {
        f(p).unwrap()
    }
    fn add(p: &Path, exe: &str) -> String {
        json_hooks_add(p, exe, "gemini", EVENTS, "Gemini CLI").unwrap()
    }
    fn read(p: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn add_idempotent_update_remove() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        let other = json!({"theme":"dark","hooks":{"BeforeTool":[{"matcher":"x","hooks":[{"type":"command","command":"other.sh"}]}]},"z":1});
        fs::write(&p, other.to_string()).unwrap();
        let msg = add(&p, EXE);
        assert!(msg.ends_with("Restart running Gemini CLI sessions to pick this up."));
        let v = read(&p);
        assert_eq!(v.to_string().matches(MARK).count(), EVENTS.len());
        assert_eq!(v["hooks"]["BeforeTool"].as_array().unwrap().len(), 2);
        assert!(p.with_file_name("settings.json.grace-backup").exists());
        assert!(add(&p, EXE).ends_with("already set up. Nothing to do."));
        assert!(add(&p, "/new/grace").contains("updated path"));
        assert!(fs::read_to_string(&p)
            .unwrap()
            .contains("/new/grace session hook gemini"));
        assert!(!fs::read_to_string(&p).unwrap().contains(EXE));
        run(&p, |p| json_hooks_remove(p, "gemini", "Gemini CLI"));
        assert_eq!(read(&p), other);
        assert!(run(&p, |p| json_hooks_remove(p, "gemini", "Gemini CLI")).contains("Nothing to do"));
    }

    #[test]
    fn remove_drops_empty_hooks_key() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(&p, r#"{"a":1}"#).unwrap();
        add(&p, EXE);
        run(&p, |p| json_hooks_remove(p, "gemini", "Gemini CLI"));
        assert_eq!(read(&p), json!({"a":1}));
    }

    #[test]
    fn bad_json_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(&p, "{ nope").unwrap();
        assert!(json_hooks_add(&p, EXE, "gemini", EVENTS, "Gemini CLI").is_err());
        assert!(json_hooks_remove(&p, "gemini", "Gemini CLI").is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "{ nope");
    }

    #[test]
    fn exe_quoted_cleanly() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        add(&p, "/a b/grace");
        let v = read(&p);
        let want = format!("{} session hook gemini", platform::quote_cmd("/a b/grace"));
        assert_eq!(v["hooks"]["SessionEnd"][0]["hooks"][0]["command"], want);
        assert!(add(&p, "/a b/grace").ends_with("Nothing to do."));
    }

    #[test]
    fn event_mapping() {
        let s = status_for;
        assert_eq!(s("BeforeAgent", ""), Some(Status::Working));
        assert_eq!(s("BeforeTool", ""), Some(Status::Working));
        assert_eq!(s("AfterTool", ""), Some(Status::Working));
        assert_eq!(s("AfterAgent", ""), Some(Status::Ready));
        assert_eq!(s("SessionStart", ""), Some(Status::Ready));
        assert_eq!(s("Notification", "ToolPermission"), Some(Status::Waiting));
        assert_eq!(s("Notification", "idle_prompt"), Some(Status::Ready));
        assert_eq!(s("SessionEnd", ""), Some(Status::Done));
        assert_eq!(s("PreCompress", ""), None);
    }

    #[test]
    fn hook_parse() {
        let r = hook(
            &[],
            r#"{"hook_event_name":"BeforeTool","cwd":"/p/x","session_id":"1","tool_name":"shell"}"#,
        );
        assert_eq!(r.status, Some(Status::Working));
        assert_eq!(r.cwd.as_deref(), Some("/p/x"));
        assert_eq!(r.session_id.as_deref(), Some("1"));
        assert_eq!(r.detail.as_deref(), Some("shell"));
        assert_eq!(hook(&[], "not json"), HookResult::default());
        assert_eq!(hook(&[], "").status, None);
        assert_eq!(hook(&[], "[1]").status, None);
    }
}
