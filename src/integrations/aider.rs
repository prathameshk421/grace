//! aider: two lines in `~/.aider.conf.yml`, each with a `# added by grace` marker.
use super::{HookResult, HookStatus, Integration};
use crate::model::Status;
use crate::platform;
use crate::setup::{read_opt, write_atomic};
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};

pub static INTEGRATION: Integration = Integration {
    name: "aider",
    label: "Aider",
    tool: "aider",
    setup,
    check,
    reads_stdin: false,
    hook,
    stdout: "",
};

const MARK: &str = "# added by grace";

fn conf_path() -> Result<PathBuf> {
    if let Some(v) = platform::env_non_empty("GRACE_AIDER_CONF") {
        return Ok(PathBuf::from(v));
    }
    platform::home_dir()
        .map(|h| h.join(".aider.conf.yml"))
        .ok_or_else(|| anyhow!("cannot find the home folder"))
}

fn setup(exe: &str, remove: bool) -> Result<String> {
    let p = conf_path()?;
    if remove {
        remove_at(&p)
    } else {
        add_at(&p, exe)
    }
}

fn check(exe: &str) -> HookStatus {
    let Ok(p) = conf_path() else {
        return HookStatus::NotInstalled;
    };
    let Ok(Some(text)) = read_opt(&p) else {
        if crate::tools::on_path("aider") {
            return HookStatus::Available(p);
        }
        return HookStatus::NotInstalled;
    };
    check_text(&p, &text, exe)
}

fn check_text(p: &Path, text: &str, exe: &str) -> HookStatus {
    let want_cmd = cmd_line_for(exe);
    let mut has_ours = false;
    let mut cmd_matches = false;
    let mut notif_enabled = false;
    for l in text.lines() {
        match top_key(l) {
            Some("notifications_command" | "notifications-command") => {
                if !is_ours(l) {
                    return HookStatus::Conflict(
                        p.to_path_buf(),
                        "custom notifications_command".into(),
                    );
                }
                has_ours = true;
                cmd_matches = l == want_cmd;
            }
            Some("notifications") => {
                if is_ours(l) {
                    has_ours = true;
                    notif_enabled = is_true(value_of(l));
                } else if is_true(value_of(l)) {
                    notif_enabled = true;
                } else {
                    return HookStatus::Conflict(p.to_path_buf(), "notifications: false".into());
                }
            }
            _ => {}
        }
    }
    if has_ours && cmd_matches && notif_enabled {
        HookStatus::Connected(p.to_path_buf())
    } else if has_ours {
        HookStatus::OutdatedPath(p.to_path_buf())
    } else {
        HookStatus::Available(p.to_path_buf())
    }
}

/// YAML single-quoted scalar: only `'` needs escaping (doubled).
fn yaml_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn cmd_line_for(exe: &str) -> String {
    format!(
        "notifications_command: {}  {MARK}",
        yaml_quote(&format!("{} session hook aider", platform::quote_cmd(exe)))
    )
}

fn is_ours(line: &str) -> bool {
    line.trim_end().ends_with(MARK)
}

/// The top-level key of a line, if it has one (`key:` at column 0).
fn top_key(line: &str) -> Option<&str> {
    if line.starts_with([' ', '\t', '#', '-']) {
        return None;
    }
    let (k, rest) = line.split_once(':')?;
    let k = k.trim().trim_matches(|c| c == '"' || c == '\'');
    (rest.is_empty() || rest.starts_with([' ', '\t'])).then_some(k)
}

/// The plain value after `key:`, without a trailing comment.
fn value_of(line: &str) -> &str {
    let v = line.split_once(':').map_or("", |(_, v)| v);
    v.split(" #").next().unwrap_or("").trim()
}

fn is_true(v: &str) -> bool {
    let v = v.trim_matches(|c| c == '"' || c == '\'');
    ["true", "yes", "on"]
        .iter()
        .any(|t| v.eq_ignore_ascii_case(t))
}

fn add_at(path: &Path, exe: &str) -> Result<String> {
    let text = read_opt(path)?.unwrap_or_default();
    let cmd_line = cmd_line_for(exe);
    let on_line = format!("notifications: true  {MARK}");
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let (mut have_on, mut have_cmd) = (false, false);
    for line in lines.iter_mut() {
        match top_key(line) {
            Some("notifications_command" | "notifications-command") => {
                if !is_ours(line) {
                    bail!(
                        "{} already has a notifications_command that grace did not add. Nothing changed",
                        path.display()
                    );
                }
                *line = cmd_line.clone();
                have_cmd = true;
            }
            Some("notifications") => {
                if is_ours(line) {
                    *line = on_line.clone();
                } else if !is_true(value_of(line)) {
                    bail!(
                        "{} sets notifications to '{}'. Delete that line or set it to true, then run this again. Nothing changed",
                        path.display(),
                        value_of(line)
                    );
                }
                have_on = true;
            }
            _ => {}
        }
    }
    if !have_on {
        lines.push(on_line);
    }
    if !have_cmd {
        lines.push(cmd_line);
    }
    let new = lines.join("\n") + "\n";
    if new == text {
        return Ok(format!(
            "{}: grace notifications already set up. Nothing to do.",
            path.display()
        ));
    }
    write_atomic(path, &new)?;
    Ok(format!(
        "Updated {}\n  notifications: true, notifications_command: grace (reports \"your turn\" only)\nRestart running aider sessions to pick this up.",
        path.display()
    ))
}

fn remove_at(path: &Path) -> Result<String> {
    let Some(text) = read_opt(path)? else {
        return Ok(format!("{} does not exist. Nothing to do.", path.display()));
    };
    let ours = |l: &str| top_key(l).is_some_and(|k| k.starts_with("notifications")) && is_ours(l);
    if !text.lines().any(ours) {
        return Ok(format!(
            "{}: no grace lines found. Nothing to do.",
            path.display()
        ));
    }
    let kept: Vec<&str> = text.lines().filter(|l| !ours(l)).collect();
    let new = if kept.is_empty() {
        String::new()
    } else {
        kept.join("\n") + "\n"
    };
    write_atomic(path, &new)?;
    Ok(format!(
        "Updated {}\n  removed grace notification lines\nRestart running aider sessions to pick this up.",
        path.display()
    ))
}

fn hook(args: &[String], _stdin: &str) -> HookResult {
    if !args.is_empty() {
        return HookResult::default();
    }
    HookResult {
        status: Some(Status::Ready),
        cwd: None,
        session_id: None,
        detail: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_idempotent_remove() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(".aider.conf.yml");
        let mine = "# my config\nmodel: sonnet\ndark-mode: true";
        std::fs::write(&p, mine).unwrap();
        add_at(&p, "/bin/grace").unwrap();
        let once = std::fs::read_to_string(&p).unwrap();
        assert!(once.starts_with("# my config\nmodel: sonnet\ndark-mode: true\n"));
        assert!(once.contains("notifications: true  # added by grace\n"));
        assert!(once.contains(
            "notifications_command: '/bin/grace session hook aider'  # added by grace\n"
        ));
        assert!(add_at(&p, "/bin/grace").unwrap().contains("Nothing to do"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), once);
        add_at(&p, "/a b/grace").unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert_eq!(t.matches("notifications_command").count(), 1);
        assert!(t.contains("notifications_command: '''/a b/grace'' session hook aider'"));
        remove_at(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), format!("{mine}\n"));
        assert!(remove_at(&p).unwrap().contains("Nothing to do"));
    }

    #[test]
    fn missing_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(".aider.conf.yml");
        assert!(remove_at(&p).unwrap().contains("Nothing to do"));
        add_at(&p, "/bin/grace").unwrap();
        assert!(std::fs::read_to_string(&p)
            .unwrap()
            .starts_with("notifications: true"));
        remove_at(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "");
    }

    #[test]
    fn refuses_foreign_command_both_spellings() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(".aider.conf.yml");
        let mine = "notifications: true\nnotifications-command: \"say 'Aider is ready'\"\n";
        std::fs::write(&p, mine).unwrap();
        assert!(add_at(&p, "/bin/grace").is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), mine);
        assert!(remove_at(&p).unwrap().contains("Nothing to do"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), mine);
    }

    #[test]
    fn foreign_notifications_flag() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(".aider.conf.yml");
        std::fs::write(&p, "notifications: false\n").unwrap();
        assert!(add_at(&p, "/bin/grace").is_err());
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "notifications: false\n"
        );
        std::fs::write(&p, "notifications: True # mine\n").unwrap();
        add_at(&p, "/bin/grace").unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert_eq!(t.matches("notifications:").count(), 1);
        remove_at(&p).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "notifications: True # mine\n"
        );
    }

    #[test]
    fn nested_keys_ignored() {
        assert_eq!(top_key("  notifications: false"), None);
        assert_eq!(top_key("# notifications: false"), None);
        assert_eq!(
            top_key("notifications_command: x"),
            Some("notifications_command")
        );
        assert_eq!(top_key("http://x"), None);
    }

    #[test]
    fn check_detects_foreign_false_and_missing_notifications_flag() {
        let p = Path::new("/tmp/.aider.conf.yml");
        assert!(matches!(
            check_text(p, "notifications: false\n", "/bin/grace"),
            HookStatus::Conflict(_, _)
        ));
        let cmd_only = format!("{}\n", cmd_line_for("/bin/grace"));
        assert!(matches!(
            check_text(p, &cmd_only, "/bin/grace"),
            HookStatus::OutdatedPath(_)
        ));
    }

    #[test]
    fn hook_parse() {
        assert_eq!(
            hook(&[], ""),
            HookResult {
                status: Some(Status::Ready),
                cwd: None,
                session_id: None,
                detail: None
            }
        );
        assert_eq!(hook(&["x".to_string()], ""), HookResult::default());
    }
}
