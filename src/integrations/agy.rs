//! agy (Antigravity CLI): a plugin folder we own, `<gemini>/config/plugins/grace/`
//! with `plugin.json` and `hooks.json`. The user's own hooks files stay untouched.
//!
//! Verified: docs https://antigravity.google/docs/hooks (layout, events, stdin fields, stdout
//! schema). Local layout copied from installed plugins in `~/.gemini/config/plugins` and
//! `~/.gemini/antigravity-cli/plugins/ares-spike` (plugin.json + hooks.json). `agy plugin validate`
//! checked the generated folder (see the test report from the author).
//! Assumed: agy does not load a folder just by its presence. Installed plugins are also listed
//! in `~/.gemini/config/config.json`, so the user may need `agy plugin enable grace`.
//! Grace does not run that command.
//!
//! Approval flow safety. The docs say PreToolUse stdout REQUIRES `decision`, so `{}` there is
//! undefined and could act as deny or ask. We do NOT install PreToolUse at all. Working comes
//! from PreInvocation, PostToolUse and (as a late signal) the others. PreInvocation and PostInvocation
//! have only optional fields, PostToolUse expects `{}`, SessionStart has only optional `injectSteps`.
//! Stop lists `decision` as required, but the docs say "any other value, or no decision, lets
//! the stop proceed". Only `continue` keeps the agent running, and we never print it. So every
//! hook prints `{}` and never `injectSteps`, `terminationBehavior` or `decision`.
//!
//! Stdin is camelCase JSON with `workspacePaths` (array, first item = cwd) and no event name.
//! The event name is the first arg of each command.
use super::{HookResult, Integration};
use crate::model::Status;
use crate::setup::{read_opt, write_atomic};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub static INTEGRATION: Integration = Integration {
    name: "agy",
    tool: "antigravity",
    setup,
    reads_stdin: true,
    hook,
    stdout: "{}",
};

const MARK: &str = "Managed by grace. Remove with: grace setup agy --remove";
const EVENTS: [&str; 5] = ["SessionStart", "PreInvocation", "PostToolUse", "PostInvocation", "Stop"];

fn base_dir() -> Result<PathBuf> {
    crate::setup::env_dir("GRACE_AGY_DIR", ".gemini")
}

fn setup(exe: &str, remove: bool) -> Result<String> {
    setup_in(&base_dir()?, exe, remove)
}

fn plugin_json() -> String {
    let v = json!({"name": "grace", "version": "1.0.0", "description": MARK});
    serde_json::to_string_pretty(&v).unwrap() + "\n"
}

fn hooks_json(exe: &str) -> String {
    let mut hooks = serde_json::Map::new();
    hooks.insert("enabled".into(), json!(true));
    for ev in EVENTS {
        let handler = json!({"type": "command", "command": format!("{} session hook agy {ev}", super::gemini::quote_once(exe)), "timeout": 10});
        // PostToolUse takes matcher entries, the others take handlers directly.
        let entry = if ev == "PostToolUse" {
            json!([{"matcher": ".*", "hooks": [handler]}])
        } else {
            json!([handler])
        };
        hooks.insert(ev.into(), entry);
    }
    serde_json::to_string_pretty(&json!({ "grace": Value::Object(hooks) })).unwrap() + "\n"
}

fn is_ours(dir: &Path) -> bool {
    read_opt(&dir.join("plugin.json"))
        .ok()
        .flatten()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| {
            v.get("name").and_then(Value::as_str) == Some("grace")
                && v.get("description").and_then(Value::as_str).is_some_and(|d| d.contains("Managed by grace"))
        })
}

fn setup_in(base: &Path, exe: &str, remove: bool) -> Result<String> {
    let dir = base.join("config").join("plugins").join("grace");
    if remove {
        if !dir.exists() {
            return Ok("agy: nothing to remove".into());
        }
        if !is_ours(&dir) {
            return Ok(format!("agy: {} is not managed by grace. Left it alone", dir.display()));
        }
        std::fs::remove_dir_all(&dir).with_context(|| format!("cannot remove {}", dir.display()))?;
        return Ok(format!("agy: removed {}", dir.display()));
    }
    if dir.exists() && !is_ours(&dir) {
        anyhow::bail!("{} exists and is not managed by grace. Nothing changed", dir.display());
    }
    let (pj, hj) = (plugin_json(), hooks_json(exe));
    let same = read_opt(&dir.join("plugin.json"))?.as_deref() == Some(pj.as_str())
        && read_opt(&dir.join("hooks.json"))?.as_deref() == Some(hj.as_str());
    if same {
        return Ok(format!("agy: already set up ({})", dir.display()));
    }
    write_atomic(&dir.join("plugin.json"), &pj)?;
    write_atomic(&dir.join("hooks.json"), &hj)?;
    Ok(format!(
        "agy: wrote {}. If agy does not list it, run: agy plugin enable grace",
        dir.display()
    ))
}

fn hook(args: &[String], stdin: &str) -> HookResult {
    let status = match args.first().map(String::as_str) {
        Some("SessionStart" | "PostInvocation" | "Stop") => Status::Ready,
        Some("PreInvocation" | "PreToolUse" | "PostToolUse") => Status::Working,
        _ => return HookResult::default(),
    };
    let cwd = serde_json::from_str::<Value>(stdin)
        .ok()
        .and_then(|v| {
            v.get("workspacePaths")?
                .as_array()?
                .first()?
                .as_str()
                .filter(|s| !s.is_empty())
                .map(String::from)
        });
    HookResult { status: Some(status), cwd }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("grace-agy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn add_idempotent_remove() {
        let d = tmp("a");
        assert!(setup_in(&d, "/bin/grace", false).unwrap().contains("wrote"));
        let p = d.join("config/plugins/grace");
        let h: Value = serde_json::from_str(&std::fs::read_to_string(p.join("hooks.json")).unwrap()).unwrap();
        assert!(h["grace"].get("PreToolUse").is_none());
        assert_eq!(
            h["grace"]["Stop"][0]["command"],
            "/bin/grace session hook agy Stop"
        );
        assert!(setup_in(&d, "/bin/grace", false).unwrap().contains("already set up"));
        setup_in(&d, "/bin/grace", true).unwrap();
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn foreign_folder_kept() {
        let d = tmp("b");
        let p = d.join("config/plugins/grace");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("plugin.json"), r#"{"name":"grace"}"#).unwrap();
        setup_in(&d, "/bin/grace", true).unwrap();
        assert!(p.exists());
        assert!(setup_in(&d, "/bin/grace", false).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn hook_parse() {
        let a = |s: &str| vec![s.to_string()];
        let r = hook(&a("PreInvocation"), r#"{"workspacePaths":["/w","/x"]}"#);
        assert_eq!(r.status, Some(Status::Working));
        assert_eq!(r.cwd.as_deref(), Some("/w"));
        assert_eq!(hook(&a("Stop"), "{}").status, Some(Status::Ready));
        assert_eq!(hook(&a("Stop"), "{}").cwd, None);
        assert_eq!(hook(&a("Stop"), "not json").cwd, None);
        assert_eq!(hook(&a("Nope"), "{}"), HookResult::default());
        assert_eq!(hook(&[], ""), HookResult::default());
    }
}
