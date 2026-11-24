//! crush (charmbracelet/crush): `hook add` lines in the global `crushrc`.
use super::{HookResult, HookStatus, Integration};
use crate::model::Status;
use crate::platform;
use crate::setup::{read_opt, write_atomic};
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub static INTEGRATION: Integration = Integration {
    name: "crush",
    label: "Crush",
    tool: "crush",
    setup,
    check,
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

fn rc_path() -> Result<PathBuf> {
    if let Ok(v) = std::env::var("GRACE_CRUSH_DIR") {
        if !v.is_empty() {
            return Ok(PathBuf::from(v).join("crushrc"));
        }
    }
    if cfg!(windows) {
        if let Ok(v) = std::env::var("LOCALAPPDATA") {
            if !v.is_empty() {
                let p = PathBuf::from(v).join("crush").join("crushrc");
                if p.exists() {
                    return Ok(p);
                }
            }
        }
    }
    if let Ok(v) = std::env::var("XDG_CONFIG_HOME") {
        if !v.is_empty() && Path::new(&v).is_absolute() {
            return Ok(PathBuf::from(v).join("crush").join("crushrc"));
        }
    }
    platform::home_dir()
        .map(|h| h.join(".config").join("crush").join("crushrc"))
        .ok_or_else(|| anyhow!("cannot find the home folder"))
}

fn setup(exe: &str, remove: bool) -> Result<String> {
    if !remove && platform::has_line_break_or_nul(exe) {
        bail!("executable path contains unsafe newline or NUL characters");
    }
    let p = rc_path()?;
    if remove {
        remove_at(&p)
    } else {
        add_at(&p, exe)
    }
}

fn check(exe: &str) -> HookStatus {
    let Ok(p) = rc_path() else {
        return HookStatus::NotInstalled;
    };
    let Ok(Some(text)) = read_opt(&p) else {
        if p.parent().is_some_and(|d| d.is_dir()) || crate::tools::on_path("crush") {
            return HookStatus::Available(p);
        }
        return HookStatus::NotInstalled;
    };
    let want_block = block(exe);
    if text.contains(&want_block) {
        HookStatus::Connected(p)
    } else if text.lines().any(is_ours) {
        HookStatus::OutdatedPath(p)
    } else {
        HookStatus::Available(p)
    }
}

fn is_ours(line: &str) -> bool {
    let t = line.trim();
    t == HEADER
        || (t.starts_with("hook add ")
            && t.contains("--name grace --command")
            && t.contains("session hook crush"))
}

fn block(exe: &str) -> String {
    let cmd = platform::quote_posix(&format!(
        "{} session hook crush",
        platform::quote_posix(exe)
    ));
    let mut s = format!("{HEADER}\n");
    for e in EVENTS {
        s.push_str(&format!("hook add {e} --name grace --command {cmd}\n"));
    }
    s
}

fn strip_ours(text: &str) -> String {
    let mut out: String = text
        .lines()
        .filter(|l| !is_ours(l))
        .map(|l| format!("{l}\n"))
        .collect();
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

fn add_at(path: &Path, exe: &str) -> Result<String> {
    if platform::has_line_break_or_nul(exe) {
        bail!("executable path contains unsafe newline or NUL characters");
    }
    let text = read_opt(path)?.unwrap_or_default();
    let mut new = strip_ours(&text);
    if !new.is_empty() {
        new.push('\n');
    }
    new.push_str(&block(exe));
    if new == text {
        return Ok(format!(
            "{}: grace hooks already set up. Nothing to do.",
            path.display()
        ));
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
        return Ok(format!(
            "{}: no grace hooks found. Nothing to do.",
            path.display()
        ));
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
    let cwd = v
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(String::from);
    let session_id = v
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(String::from);
    let detail = v
        .get("tool_name")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(platform::sanitize_display);
    HookResult {
        status: Some(status),
        cwd,
        session_id,
        detail,
    }
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
        assert!(once
            .contains("hook add TurnEnd --name grace --command '/bin/grace session hook crush'"));
        assert!(add_at(&p, "/bin/grace").unwrap().contains("Nothing to do"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), once);
        add_at(&p, "/new/grace").unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert_eq!(t.matches("hook add TurnEnd").count(), 1);
        assert!(!t.contains("/bin/grace"));
        remove_at(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), mine);
        assert!(remove_at(&p).unwrap().contains("Nothing to do"));
    }

    #[test]
    fn quoted_exe_survives_two_shells_including_windows_backslashes() {
        let b = block("/a b/grace");
        assert!(b.contains(r#"--command ''\''/a b/grace'\'' session hook crush'"#));
        let w = block(r"C:\Tools\grace.exe");
        assert!(w.contains(r#"--command ''\''C:\Tools\grace.exe'\'' session hook crush'"#));
    }

    #[test]
    fn parse() {
        let r = hook(&[], r#"{"hook_event_name":"PermissionRequest","cwd":"/a"}"#);
        assert_eq!(r.status, Some(Status::Waiting));
        assert_eq!(r.cwd.as_deref(), Some("/a"));
        assert_eq!(
            hook(&[], r#"{"event":"session_end"}"#).status,
            Some(Status::Done)
        );
        assert_eq!(
            hook(&[], r#"{"event":"TurnStart"}"#).status,
            Some(Status::Working)
        );
        assert_eq!(hook(&[], r#"{"event":"TurnEnd","cwd":""}"#).cwd, None);
        assert_eq!(hook(&[], "nope"), HookResult::default());
        assert_eq!(hook(&[], ""), HookResult::default());
        assert_eq!(hook(&[], r#"{"event":5}"#), HookResult::default());
        assert_eq!(
            hook(&[], r#"{"hook_event_name":"PreToolUse"}"#).status,
            None
        );
    }
}
