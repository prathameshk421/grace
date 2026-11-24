//! copilot: GitHub Copilot CLI. We own one file, `~/.copilot/hooks/grace.json`.
use super::{managed_file_check, remove_owned_backup, HookResult, HookStatus, Integration};
use crate::model::Status;
use crate::platform;
use crate::session::non_empty_str;
use crate::setup::{env_dir, read_opt, write_atomic};
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub static INTEGRATION: Integration = Integration {
    name: "copilot",
    label: "GitHub Copilot CLI",
    tool: "copilot",
    setup,
    check,
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
    let base = match platform::env_non_empty("GRACE_COPILOT_DIR") {
        Some(v) => PathBuf::from(v),
        None => env_dir("COPILOT_HOME", ".copilot")?,
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

fn is_ours_text(t: &str) -> bool {
    t.contains(MARK)
}

fn check(exe: &str) -> HookStatus {
    let Ok(path) = hooks_file() else {
        return HookStatus::NotInstalled;
    };
    let dir = path
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf);
    managed_file_check(
        path,
        dir.as_deref(),
        "copilot",
        "custom grace.json",
        is_ours_text,
        |old| serde_json::from_str::<Value>(old).ok().as_ref() == Some(&desired(exe)),
    )
}

fn desired(exe: &str) -> Value {
    let sh_exe = platform::quote_posix(exe);
    let ps_exe = platform::quote_powershell(exe);
    let mut hooks = Map::new();
    for ev in EVENTS {
        hooks.insert(
            ev.to_string(),
            json!([{
                "type": "command",
                "bash": format!("{sh_exe} {MARK} {ev}"),
                "powershell": format!("& {ps_exe} {MARK} {ev}"),
            }]),
        );
    }
    json!({ "version": 1, "hooks": hooks })
}

fn add_file(path: &Path, exe: &str) -> Result<String> {
    let want = desired(exe);
    if let Some(old) = read_opt(path)? {
        if !old.contains(MARK) {
            bail!(
                "{} exists and is not a grace file. Nothing changed",
                path.display()
            );
        }
        if serde_json::from_str::<Value>(&old).ok().as_ref() == Some(&want) {
            return Ok(format!(
                "{}: grace hooks already set up. Nothing to do.",
                path.display()
            ));
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
        Some(t) if !t.contains(MARK) => Ok(format!(
            "{}: not a grace file. Left as it is. Nothing to do.",
            path.display()
        )),
        Some(_) => {
            std::fs::remove_file(path)?;
            remove_owned_backup(path, MARK);
            Ok(format!(
                "Removed {}\nRestart running Copilot CLI sessions to pick this up.",
                path.display()
            ))
        }
    }
}

fn status_for(event: &str, ntype: &str) -> Option<Status> {
    let mut e = event.to_string();
    if let Some(c) = e.get_mut(0..1) {
        c.make_ascii_lowercase();
    }
    match e.as_str() {
        "userPromptSubmitted" | "userPromptSubmit" | "preToolUse" | "postToolUse" => {
            Some(Status::Working)
        }
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
    let ev_str = non_empty_str(&v, "hook_event_name").unwrap_or_default();
    let event = match args.first() {
        Some(a) if !a.is_empty() => a.as_str(),
        _ => ev_str.as_str(),
    };
    let ntype = non_empty_str(&v, "notification_type").unwrap_or_default();
    let status = status_for(event, &ntype);
    let detail = match status {
        Some(Status::Waiting) if !ntype.is_empty() => {
            Some(platform::sanitize_display(&ntype).into_owned())
        }
        Some(Status::Working) => {
            non_empty_str(&v, "toolName").map(|t| platform::sanitize_display(&t).into_owned())
        }
        _ => None,
    };
    HookResult {
        status,
        cwd: non_empty_str(&v, "cwd"),
        session_id: non_empty_str(&v, "sessionId"),
        detail,
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
        assert!(add_file(&p, EXE)
            .unwrap()
            .ends_with("already set up. Nothing to do."));
        add_file(&p, "/new/grace").unwrap();
        assert!(fs::read_to_string(&p)
            .unwrap()
            .contains("/new/grace session hook"));
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
    fn quoted_exe_in_powershell_and_bash() {
        let v = desired("/a b/grace");
        assert_eq!(
            v["hooks"]["sessionEnd"][0]["powershell"],
            "& '/a b/grace' session hook copilot sessionEnd"
        );
        assert_eq!(
            v["hooks"]["sessionEnd"][0]["bash"],
            "'/a b/grace' session hook copilot sessionEnd"
        );
        let win = desired(r"C:\My Tools\grace.exe");
        assert_eq!(
            win["hooks"]["sessionEnd"][0]["powershell"],
            r"& 'C:\My Tools\grace.exe' session hook copilot sessionEnd"
        );
    }

    #[test]
    fn remove_keeps_other_hook_files() {
        let d = tempfile::tempdir().unwrap();
        let other = d.path().join("hooks").join("mine.json");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        fs::write(
            &other,
            r#"{"version":1,"hooks":{"agentStop":[{"type":"command","bash":"echo hi"}]}}"#,
        )
        .unwrap();
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
        assert_eq!(
            s("notification", "elicitation_dialog"),
            Some(Status::Waiting)
        );
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
        let r = hook(
            &a("agentStop"),
            r#"{"sessionId":"1","timestamp":1,"cwd":"/c"}"#,
        );
        assert_eq!(r.status, Some(Status::Ready));
        assert_eq!(r.cwd.as_deref(), Some("/c"));
        assert_eq!(r.session_id.as_deref(), Some("1"));
        let r = hook(&[], r#"{"hook_event_name":"SessionEnd","cwd":"/c"}"#);
        assert_eq!(r.status, Some(Status::Done));
        let r = hook(
            &a("notification"),
            r#"{"cwd":"/c","hook_event_name":"Notification","notification_type":"permission_prompt"}"#,
        );
        assert_eq!(r.status, Some(Status::Waiting));
        assert_eq!(hook(&a("agentStop"), "bad"), HookResult::default());
        assert_eq!(hook(&a("agentStop"), "").status, None);
        assert_eq!(hook(&[], "bad"), HookResult::default());
        assert_eq!(hook(&a("agentStop"), "[1]").status, None);
    }
}
