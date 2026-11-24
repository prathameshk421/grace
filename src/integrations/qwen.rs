//! qwen: Qwen Code. Hooks go in `~/.qwen/settings.json` (Claude-style `hooks` map).
use super::gemini::parse_event_json;
use super::{HookResult, HookStatus, Integration};
use crate::model::Status;
use crate::setup::{env_dir, json_hooks_add, json_hooks_check, json_hooks_remove};
use anyhow::Result;
use std::path::PathBuf;

pub static INTEGRATION: Integration = Integration {
    name: "qwen",
    label: "Qwen Code",
    tool: "qwen",
    setup,
    check,
    reads_stdin: true,
    hook,
    stdout: "{}",
};

const EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("PreToolUse", None),
    ("PostToolUse", None),
    ("PermissionRequest", None),
    ("Notification", None),
    ("Stop", None),
    ("SessionEnd", None),
];

fn settings_path() -> Result<PathBuf> {
    Ok(env_dir("GRACE_QWEN_DIR", ".qwen")?.join("settings.json"))
}

fn setup(exe: &str, remove: bool) -> Result<String> {
    let path = settings_path()?;
    if remove {
        json_hooks_remove(&path, "qwen", "Qwen Code")
    } else {
        json_hooks_add(&path, exe, "qwen", EVENTS, "Qwen Code")
    }
}

fn check(exe: &str) -> HookStatus {
    let Ok(path) = settings_path() else {
        return HookStatus::NotInstalled;
    };
    json_hooks_check(&path, exe, "qwen", EVENTS)
}

fn status_for(event: &str, ntype: &str) -> Option<Status> {
    match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some(Status::Working),
        "PermissionRequest" => Some(Status::Waiting),
        "Notification" => match ntype {
            "idle_prompt" => Some(Status::Ready),
            "auth_success" => None,
            _ => Some(Status::Waiting),
        },
        "Stop" | "SessionStart" => Some(Status::Ready),
        "SessionEnd" => Some(Status::Done),
        _ => None,
    }
}

fn hook(_args: &[String], stdin: &str) -> HookResult {
    parse_event_json(stdin, status_for)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::fs;

    const EXE: &str = "/opt/grace/bin/grace";

    fn read(p: &std::path::Path) -> Value {
        serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn add_idempotent_remove() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        let other = json!({"model":"q","hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep.sh"}]}]},"z":[1]});
        fs::write(&p, other.to_string()).unwrap();
        let msg = json_hooks_add(&p, EXE, "qwen", EVENTS, "Qwen Code").unwrap();
        assert!(msg.ends_with("Restart running Qwen Code sessions to pick this up."));
        let v = read(&p);
        assert_eq!(
            v.to_string().matches("session hook qwen").count(),
            EVENTS.len()
        );
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
        let again = json_hooks_add(&p, EXE, "qwen", EVENTS, "Qwen Code").unwrap();
        assert!(again.ends_with("already set up. Nothing to do."));
        let up = json_hooks_add(&p, "/new/grace", "qwen", EVENTS, "Qwen Code").unwrap();
        assert!(up.contains("updated path"));
        assert!(!fs::read_to_string(&p).unwrap().contains(EXE));
        let rm = json_hooks_remove(&p, "qwen", "Qwen Code").unwrap();
        assert!(rm.ends_with("Restart running Qwen Code sessions to pick this up."));
        assert_eq!(read(&p), other);
        let v = read(&p);
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["model", "hooks", "z"]);
    }

    #[test]
    fn bad_json_errors() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(&p, "{").unwrap();
        assert!(json_hooks_add(&p, EXE, "qwen", EVENTS, "Qwen Code").is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "{");
    }

    #[test]
    fn event_mapping() {
        let s = status_for;
        for e in ["UserPromptSubmit", "PreToolUse", "PostToolUse"] {
            assert_eq!(s(e, ""), Some(Status::Working));
        }
        assert_eq!(s("PermissionRequest", ""), Some(Status::Waiting));
        assert_eq!(
            s("Notification", "permission_prompt"),
            Some(Status::Waiting)
        );
        assert_eq!(s("Notification", "idle_prompt"), Some(Status::Ready));
        assert_eq!(s("Notification", "auth_success"), None);
        assert_eq!(s("Stop", ""), Some(Status::Ready));
        assert_eq!(s("SessionStart", ""), Some(Status::Ready));
        assert_eq!(s("SessionEnd", ""), Some(Status::Done));
        assert_eq!(s("PreCompact", ""), None);
    }

    #[test]
    fn hook_parse() {
        let r = hook(&[], r#"{"hook_event_name":"PermissionRequest","cwd":"/w"}"#);
        assert_eq!(r.status, Some(Status::Waiting));
        assert_eq!(r.cwd.as_deref(), Some("/w"));
        assert_eq!(hook(&[], "garbage"), HookResult::default());
        assert_eq!(hook(&[], "").status, None);
        let r = hook(
            &[],
            r#"{"hook_event_name":"Notification","notification_type":"idle_prompt","cwd":"/w"}"#,
        );
        assert_eq!(r.status, Some(Status::Ready));
    }
}
