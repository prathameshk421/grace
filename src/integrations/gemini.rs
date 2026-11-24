//! gemini: Gemini CLI. Hooks go in `~/.gemini/settings.json` (Claude-style `hooks` map).
use super::{HookResult, HookStatus, Integration};
use crate::model::Status;
use crate::platform;
use crate::setup::{env_dir, parse_settings, read_opt, write_atomic};
use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

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
const EVENTS: &[&str] = &[
    "SessionStart",
    "BeforeAgent",
    "BeforeTool",
    "AfterTool",
    "AfterAgent",
    "Notification",
    "SessionEnd",
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
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
    let ev = text("hook_event_name");
    let ntype = text("notification_type");
    let status = map(ev, ntype);
    let cwd = Some(text("cwd"))
        .filter(|s| !s.is_empty())
        .map(String::from);
    let session_id = Some(text("session_id"))
        .filter(|s| !s.is_empty())
        .map(String::from);
    let detail = match status {
        Some(Status::Waiting) if !ntype.is_empty() => Some(platform::sanitize_display(ntype)),
        Some(Status::Working) => {
            let tn = text("tool_name");
            (!tn.is_empty()).then(|| platform::sanitize_display(tn))
        }
        _ => None,
    };
    HookResult {
        status,
        cwd,
        session_id,
        detail,
    }
}

fn is_ours(hook: &Value, mark: &str) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(mark))
}

pub(super) fn json_hooks_check(path: &Path, exe: &str, name: &str, events: &[&str]) -> HookStatus {
    let Ok(Some(text)) = read_opt(path) else {
        if path.parent().is_some_and(|p| p.is_dir()) || crate::tools::on_path(name) {
            return HookStatus::Available(path.to_path_buf());
        }
        return HookStatus::NotInstalled;
    };
    let Ok(root) = parse_settings(path, Some(&text)) else {
        return HookStatus::Conflict(path.to_path_buf(), "invalid JSON".into());
    };
    let mark = format!("session hook {name}");
    let want = format!("{} {mark}", platform::quote_posix(exe));
    let Some(hooks) = root.get("hooks").and_then(Value::as_object) else {
        return HookStatus::Available(path.to_path_buf());
    };
    let mut has_ours = false;
    let mut matches_all = true;
    for event in events {
        let found = hooks
            .get(*event)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|g| g.get("hooks").and_then(Value::as_array))
            .flatten()
            .find(|h| is_ours(h, &mark));
        match found {
            Some(h) => {
                has_ours = true;
                if h.get("command").and_then(Value::as_str) != Some(want.as_str()) {
                    matches_all = false;
                }
            }
            None => matches_all = false,
        }
    }
    if has_ours && matches_all {
        HookStatus::Connected(path.to_path_buf())
    } else if has_ours {
        HookStatus::OutdatedPath(path.to_path_buf())
    } else {
        HookStatus::Available(path.to_path_buf())
    }
}

/// Add or update our command hooks in a Claude-style settings file.
pub(super) fn json_hooks_add(
    path: &Path,
    exe: &str,
    name: &str,
    events: &[&str],
    label: &str,
) -> Result<String> {
    let mark = format!("session hook {name}");
    let cmd = format!("{} {mark}", platform::quote_posix(exe));
    let text = read_opt(path)?;
    let mut root = parse_settings(path, text.as_deref())?;
    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            anyhow!(
                "'hooks' in {} is not an object. Nothing changed",
                path.display()
            )
        })?;
    let (mut added, mut updated) = (vec![], vec![]);
    for event in events {
        let groups = hooks
            .entry(event.to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| {
                anyhow!(
                    "hooks.{event} in {} is not an array. Nothing changed",
                    path.display()
                )
            })?;
        let (mut found, mut changed) = (false, false);
        for g in groups.iter_mut() {
            let Some(list) = g.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            for h in list.iter_mut().filter(|h| is_ours(h, &mark)) {
                found = true;
                if h["command"].as_str() != Some(&cmd) {
                    h["command"] = Value::String(cmd.clone());
                    changed = true;
                }
            }
        }
        if !found {
            let mut g = Map::new();
            g.insert(
                "hooks".into(),
                json!([{ "type": "command", "command": cmd }]),
            );
            groups.push(Value::Object(g));
            added.push(*event);
        } else if changed {
            updated.push(*event);
        }
    }
    if added.is_empty() && updated.is_empty() {
        return Ok(format!(
            "{}: grace hooks already set up. Nothing to do.",
            path.display()
        ));
    }
    let mut out = serde_json::to_string_pretty(&root)?;
    out.push('\n');
    write_atomic(path, &out)?;
    let mut msg = format!("Updated {}", path.display());
    if !added.is_empty() {
        msg.push_str(&format!("\n  added: {}", added.join(", ")));
    }
    if !updated.is_empty() {
        msg.push_str(&format!("\n  updated path: {}", updated.join(", ")));
    }
    msg.push_str(&format!(
        "\nRestart running {label} sessions to pick this up."
    ));
    Ok(msg)
}

/// Remove our command hooks. Drops emptied groups, events and the `hooks` key.
pub(super) fn json_hooks_remove(path: &Path, name: &str, label: &str) -> Result<String> {
    let mark = format!("session hook {name}");
    let text = read_opt(path)?;
    if text.is_none() {
        return Ok(format!("{} does not exist. Nothing to do.", path.display()));
    }
    let mut root = parse_settings(path, text.as_deref())?;
    let mut removed = vec![];
    if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        let events: Vec<String> = hooks.keys().cloned().collect();
        for ev in events {
            let Some(groups) = hooks.get_mut(&ev).and_then(Value::as_array_mut) else {
                continue;
            };
            let mut n = 0;
            for g in groups.iter_mut() {
                if let Some(list) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                    let before = list.len();
                    list.retain(|h| !is_ours(h, &mark));
                    n += before - list.len();
                }
            }
            if n > 0 {
                groups.retain(|g| {
                    g.get("hooks")
                        .and_then(Value::as_array)
                        .is_none_or(|l| !l.is_empty())
                });
                removed.push(ev.clone());
                if groups.is_empty() {
                    hooks.shift_remove(&ev);
                }
            }
        }
        if hooks.is_empty() && !removed.is_empty() {
            root.as_object_mut().unwrap().shift_remove("hooks");
        }
    }
    if removed.is_empty() {
        return Ok(format!(
            "{}: no grace hooks found. Nothing to do.",
            path.display()
        ));
    }
    let mut out = serde_json::to_string_pretty(&root)?;
    out.push('\n');
    write_atomic(path, &out)?;
    Ok(format!(
        "Updated {}\n  removed: {}\nRestart running {label} sessions to pick this up.",
        path.display(),
        removed.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
        assert_eq!(
            v["hooks"]["SessionEnd"][0]["hooks"][0]["command"],
            "'/a b/grace' session hook gemini"
        );
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
