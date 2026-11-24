//! goose: a plugin folder we own, `~/.agents/plugins/grace/`.
use super::{ensure_not_symlink, plugin_dir_check, HookResult, HookStatus, Integration};
use crate::model::Status;
use crate::platform;
use crate::session::non_empty_str;
use crate::setup::{env_dir, read_opt, write_atomic};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub static INTEGRATION: Integration = Integration {
    name: "goose",
    label: "Goose",
    tool: "goose",
    setup,
    check,
    reads_stdin: true,
    hook,
    stdout: "",
};

const MARK: &str = "Managed by grace. Remove with: grace setup goose --remove";
const EVENTS: [&str; 5] = [
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "Stop",
    "SessionEnd",
];

fn plugin_dir() -> Result<PathBuf> {
    Ok(env_dir("GRACE_GOOSE_DIR", ".agents")?
        .join("plugins")
        .join("grace"))
}

fn setup(exe: &str, remove: bool) -> Result<String> {
    let d = plugin_dir()?;
    if remove {
        remove_at(&d)
    } else {
        add_at(&d, exe)
    }
}

fn check(exe: &str) -> HookStatus {
    let Ok(d) = plugin_dir() else {
        return HookStatus::NotInstalled;
    };
    let hp = d.join("hooks").join("hooks.json");
    let base = d.parent().and_then(|p| p.parent());
    plugin_dir_check(&d, hp, base, "goose", is_ours, &hooks_json(exe))
}

fn is_ours(dir: &Path) -> bool {
    read_opt(&dir.join("plugin.json"))
        .ok()
        .flatten()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| v.get("description").and_then(Value::as_str) == Some(MARK))
}

fn hooks_json(exe: &str) -> String {
    let cmd = format!("{} session hook goose", platform::quote_cmd(exe));
    let mut hooks = serde_json::Map::new();
    for e in EVENTS {
        hooks.insert(
            e.into(),
            json!([{ "hooks": [{ "type": "command", "command": cmd, "timeout": 10 }] }]),
        );
    }
    serde_json::to_string_pretty(&json!({ "hooks": hooks })).unwrap() + "\n"
}

fn plugin_json() -> String {
    serde_json::to_string_pretty(
        &json!({ "name": "grace", "version": "1.0.0", "description": MARK }),
    )
    .unwrap()
        + "\n"
}

fn add_at(dir: &Path, exe: &str) -> Result<String> {
    ensure_not_symlink(dir)?;
    if dir.exists() && !is_ours(dir) {
        bail!(
            "{} exists and is not made by grace. Nothing changed",
            dir.display()
        );
    }
    let (hj, pj) = (hooks_json(exe), plugin_json());
    let hp = dir.join("hooks").join("hooks.json");
    let pp = dir.join("plugin.json");
    if read_opt(&hp)?.as_deref() == Some(hj.as_str())
        && read_opt(&pp)?.as_deref() == Some(pj.as_str())
    {
        return Ok(format!(
            "{}: grace plugin already set up. Nothing to do.",
            dir.display()
        ));
    }
    write_atomic(&pp, &pj)?;
    write_atomic(&hp, &hj)?;
    Ok(format!(
        "Wrote goose plugin {}\n  hooks: {}\nRestart running goose sessions to pick this up.",
        dir.display(),
        EVENTS.join(", ")
    ))
}

fn remove_at(dir: &Path) -> Result<String> {
    ensure_not_symlink(dir)?;
    if !dir.exists() {
        return Ok(format!("{} does not exist. Nothing to do.", dir.display()));
    }
    if !is_ours(dir) {
        bail!("{} is not made by grace. Left it alone", dir.display());
    }
    std::fs::remove_dir_all(dir).with_context(|| format!("cannot remove {}", dir.display()))?;
    Ok(format!(
        "Removed {}\nRestart running goose sessions to pick this up.",
        dir.display()
    ))
}

fn hook(_args: &[String], stdin: &str) -> HookResult {
    let Ok(v) = serde_json::from_str::<Value>(stdin) else {
        return HookResult::default();
    };
    let ev = v
        .get("event")
        .or_else(|| v.get("hook_event_name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let status = match ev {
        "SessionStart" | "Stop" => Status::Ready,
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Status::Working,
        "SessionEnd" => Status::Done,
        _ => return HookResult::default(),
    };
    let detail =
        non_empty_str(&v, "tool_name").map(|s| platform::sanitize_display(&s).into_owned());
    HookResult {
        status: Some(status),
        cwd: non_empty_str(&v, "working_dir"),
        session_id: non_empty_str(&v, "session_id"),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_idempotent_remove() {
        let d = tempfile::tempdir().unwrap();
        let other = d.path().join("plugins").join("other");
        std::fs::create_dir_all(&other).unwrap();
        let p = d.path().join("plugins").join("grace");
        add_at(&p, "/bin/grace").unwrap();
        let pj: Value =
            serde_json::from_str(&std::fs::read_to_string(p.join("plugin.json")).unwrap()).unwrap();
        assert_eq!(pj["name"], "grace");
        let h: Value =
            serde_json::from_str(&std::fs::read_to_string(p.join("hooks/hooks.json")).unwrap())
                .unwrap();
        assert_eq!(
            h["hooks"]["Stop"][0]["hooks"][0]["command"],
            "/bin/grace session hook goose"
        );
        assert!(h["hooks"].get("PreToolUse").is_none());
        assert!(add_at(&p, "/bin/grace").unwrap().contains("Nothing to do"));
        assert!(add_at(&p, "/new/grace").unwrap().contains("Wrote"));
        remove_at(&p).unwrap();
        assert!(!p.exists());
        assert!(other.exists());
        assert!(remove_at(&p).unwrap().contains("Nothing to do"));
    }

    #[test]
    fn foreign_folder_kept() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("grace");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("plugin.json"), r#"{"name":"grace"}"#).unwrap();
        assert!(add_at(&p, "g").is_err());
        assert!(remove_at(&p).is_err());
        assert_eq!(
            std::fs::read_to_string(p.join("plugin.json")).unwrap(),
            r#"{"name":"grace"}"#
        );
    }

    #[test]
    fn parse() {
        let r = hook(
            &[],
            r#"{"event":"PostToolUse","session_id":"s","working_dir":"/w"}"#,
        );
        assert_eq!(r.status, Some(Status::Working));
        assert_eq!(r.cwd.as_deref(), Some("/w"));
        assert_eq!(r.session_id.as_deref(), Some("s"));
        assert_eq!(
            hook(&[], r#"{"hook_event_name":"Stop"}"#).status,
            Some(Status::Ready)
        );
        assert_eq!(
            hook(&[], r#"{"event":"UserPromptSubmit"}"#).status,
            Some(Status::Working)
        );
        let end = hook(&[], r#"{"event":"SessionEnd"}"#);
        assert_eq!(end.status, Some(Status::Done));
        assert_eq!(end.cwd, None);
        assert_eq!(hook(&[], ""), HookResult::default());
        assert_eq!(hook(&[], "[1]"), HookResult::default());
        assert_eq!(
            hook(&[], r#"{"event":"BeforeReadFile"}"#),
            HookResult::default()
        );
    }
}
