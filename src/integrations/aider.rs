//! aider: two lines in `~/.aider.conf.yml`, each with a `# added by grace` marker.
//!
//! Verified (https://aider.chat/docs/usage/notifications.html and /docs/config/options.html,
//! fetched 2026-11-19):
//! - `notifications: true` turns on notifications. aider then notifies "when the LLM has
//!   finished generating a response and is waiting for your input".
//! - `notifications_command: <cmd>` runs that command instead of the terminal bell.
//! - aider looks for `.aider.conf.yml` in the git root, the cwd and the home folder.
//!   We edit only the home file (`GRACE_AIDER_CONF` replaces its full path).
//!
//! So aider reports only "your turn" (Ready). No Working, Waiting or Done comes from aider.
//!
//! Assumed: the docs do not say how the command runs. We assume a shell (the docs example is
//! `say 'Aider is ready'`), so the shell-quoted exe works. We pass no args and grace never
//! reads stdin, because aider's stdin is the user's terminal. cwd is None: the hook runs in
//! aider's folder. `AIDER_NOTIFICATIONS_COMMAND`, `--notifications-command` or a project
//! `.aider.conf.yml` can override our lines.
//!
//! Text edit rules (there is no YAML crate). We look only at top-level `key:` lines.
//! - Our lines end with the marker. We replace them in place.
//! - A foreign `notifications_command` is an error and nothing changes.
//! - A foreign `notifications` line that is already true stays as it is (and stays on remove).
//!   A foreign one with any other value is an error and nothing changes.
//! - Remove deletes only lines with our marker.
use super::{HookResult, Integration};
use crate::model::Status;
use crate::setup::{read_opt, write_atomic};
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};

pub static INTEGRATION: Integration = Integration {
    name: "aider",
    tool: "aider",
    setup,
    reads_stdin: false,
    hook,
    stdout: "",
};

const MARK: &str = "# added by grace";

fn conf_path() -> Result<PathBuf> {
    if let Ok(v) = std::env::var("GRACE_AIDER_CONF") {
        if !v.is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    crate::platform::home_dir()
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

/// YAML single-quoted scalar: only `'` needs escaping (doubled).
fn yaml_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
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
    ["true", "yes", "on"].iter().any(|t| v.eq_ignore_ascii_case(t))
}

fn add_at(path: &Path, exe: &str) -> Result<String> {
    let text = read_opt(path)?.unwrap_or_default();
    let cmd_line = format!(
        "notifications_command: {}  {MARK}",
        yaml_quote(&format!("{} session hook aider", super::gemini::quote_once(exe)))
    );
    let on_line = format!("notifications: true  {MARK}");
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let (mut have_on, mut have_cmd) = (false, false);
    for line in lines.iter_mut() {
        match top_key(line) {
            Some("notifications_command") => {
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
        return Ok(format!("{}: grace notifications already set up. Nothing to do.", path.display()));
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
        return Ok(format!("{}: no grace lines found. Nothing to do.", path.display()));
    }
    let kept: Vec<&str> = text.lines().filter(|l| !ours(l)).collect();
    let new = if kept.is_empty() { String::new() } else { kept.join("\n") + "\n" };
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
    HookResult { status: Some(Status::Ready), cwd: None }
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
        assert!(once.contains("notifications_command: '/bin/grace session hook aider'  # added by grace\n"));
        assert!(add_at(&p, "/bin/grace").unwrap().contains("Nothing to do"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), once);
        // Ours gets updated in place.
        add_at(&p, "'/a b/grace'").unwrap();
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
        assert!(std::fs::read_to_string(&p).unwrap().starts_with("notifications: true"));
        remove_at(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "");
    }

    #[test]
    fn refuses_foreign_command() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(".aider.conf.yml");
        let mine = "notifications: true\nnotifications_command: \"say 'Aider is ready'\"\n";
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
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "notifications: false\n");
        // A foreign true stays, also after remove.
        std::fs::write(&p, "notifications: True # mine\n").unwrap();
        add_at(&p, "/bin/grace").unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert_eq!(t.matches("notifications:").count(), 1);
        remove_at(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "notifications: True # mine\n");
    }

    #[test]
    fn nested_keys_ignored() {
        assert_eq!(top_key("  notifications: false"), None);
        assert_eq!(top_key("# notifications: false"), None);
        assert_eq!(top_key("notifications_command: x"), Some("notifications_command"));
        assert_eq!(top_key("http://x"), None);
    }

    #[test]
    fn hook_parse() {
        assert_eq!(hook(&[], ""), HookResult { status: Some(Status::Ready), cwd: None });
        assert_eq!(hook(&["x".to_string()], ""), HookResult::default());
    }
}
