//! `grace setup <claude|codex>`: connect CLI agents to grace so it knows their live status.
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Value as TomlValue};

/// Text that marks a Claude hook command as ours.
const CLAUDE_MARK: &str = "session hook claude";
/// Claude events we hook, with the matcher each one needs (None = no matcher).
const CLAUDE_EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("PreToolUse", Some("*")),
    ("PostToolUse", Some("*")),
    ("Notification", None),
    ("Stop", None),
    ("SessionEnd", None),
];

/// target: "claude" or "codex". remove = undo the setup.
pub fn run(target: &str, remove: bool) -> Result<()> {
    match target {
        "claude" => {
            let path = claude_settings_path()?;
            let msg = if remove {
                claude_remove(&path)?
            } else {
                claude_add(&path, &exe_string()?)?
            };
            eprintln!("{msg}");
        }
        "codex" => {
            let path = codex_config_path()?;
            let msg = if remove {
                codex_remove(&path)?
            } else {
                codex_add(&path, &exe_string()?)?
            };
            eprintln!("{msg}");
        }
        other => {
            let hint = if other.to_ascii_lowercase().starts_with("t3") {
                " T3 Code has no hook system; if it runs the Claude Code CLI, your Claude hooks already report its sessions."
            } else {
                ""
            };
            bail!("unknown target '{other}'. grace can set up: claude, codex.{hint}")
        }
    }
    Ok(())
}

fn env_dir(var: &str, default_sub: &str) -> Result<PathBuf> {
    if let Ok(v) = std::env::var(var) {
        if !v.is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    crate::platform::home_dir()
        .map(|h| h.join(default_sub))
        .ok_or_else(|| anyhow!("cannot find the home folder"))
}

pub fn claude_settings_path() -> Result<PathBuf> {
    Ok(env_dir("CLAUDE_CONFIG_DIR", ".claude")?.join("settings.json"))
}

pub fn codex_config_path() -> Result<PathBuf> {
    Ok(env_dir("CODEX_HOME", ".codex")?.join("config.toml"))
}

/// Absolute path of the running grace binary.
pub fn exe_string() -> Result<String> {
    let exe = std::env::current_exe().context("cannot find the grace binary")?;
    Ok(crate::platform::normalize(&exe))
}

/// Quote a path for the shell when it has special characters.
fn shell_quote(s: &str) -> String {
    let plain = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/_-.:\\+@%".contains(c));
    if plain {
        s.to_string()
    } else if cfg!(windows) {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn claude_command(exe: &str) -> String {
    format!("{} {}", shell_quote(exe), CLAUDE_MARK)
}

/// Write a file atomically: temp file in the same folder, then rename.
/// Copy the old file to `<name>.grace-backup` once, before the first write.
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let dir = path.parent().ok_or_else(|| anyhow!("bad path {}", path.display()))?;
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    if path.exists() {
        let backup = dir.join(format!("{name}.grace-backup"));
        if !backup.exists() {
            fs::copy(path, &backup)
                .with_context(|| format!("cannot write backup {}", backup.display()))?;
        }
    }
    let tmp = dir.join(format!("{name}.grace-tmp"));
    fs::write(&tmp, content).with_context(|| format!("cannot write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| {
        let _ = fs::remove_file(&tmp);
        format!("cannot replace {}", path.display())
    })
}

fn read_opt(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn parse_settings(path: &Path, text: Option<&str>) -> Result<Value> {
    match text {
        None => Ok(json!({})),
        Some(t) if t.trim().is_empty() => Ok(json!({})),
        Some(t) => {
            let v: Value = serde_json::from_str(t)
                .with_context(|| format!("{} is not valid JSON. Nothing changed", path.display()))?;
            if !v.is_object() {
                bail!("{} is not a JSON object. Nothing changed", path.display());
            }
            Ok(v)
        }
    }
}

fn is_ours(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(CLAUDE_MARK))
}

/// Add or update our hooks in a Claude settings file. Returns a summary.
pub fn claude_add(path: &Path, exe: &str) -> Result<String> {
    let text = read_opt(path)?;
    let mut root = parse_settings(path, text.as_deref())?;
    let cmd = claude_command(exe);
    let obj = root.as_object_mut().unwrap();
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| anyhow!("'hooks' in {} is not an object. Nothing changed", path.display()))?;
    let (mut added, mut updated) = (vec![], vec![]);
    for (event, matcher) in CLAUDE_EVENTS {
        let groups = hooks.entry(event.to_string()).or_insert_with(|| json!([]));
        let groups = groups.as_array_mut().ok_or_else(|| {
            anyhow!("hooks.{event} in {} is not an array. Nothing changed", path.display())
        })?;
        let mut found = false;
        let mut changed = false;
        for g in groups.iter_mut() {
            let Some(list) = g.get_mut("hooks").and_then(Value::as_array_mut) else { continue };
            for h in list.iter_mut().filter(|h| is_ours(h)) {
                found = true;
                if h["command"].as_str() != Some(&cmd) {
                    h["command"] = Value::String(cmd.clone());
                    changed = true;
                }
            }
        }
        if !found {
            let mut g = Map::new();
            if let Some(m) = matcher {
                g.insert("matcher".into(), json!(m));
            }
            g.insert("hooks".into(), json!([{ "type": "command", "command": cmd }]));
            groups.push(Value::Object(g));
            added.push(*event);
        } else if changed {
            updated.push(*event);
        }
    }
    let mut out = serde_json::to_string_pretty(&root)?;
    out.push('\n');
    if added.is_empty() && updated.is_empty() {
        return Ok(format!(
            "{}: grace hooks already set up. Nothing to do.",
            path.display()
        ));
    }
    write_atomic(path, &out)?;
    let mut msg = format!("Updated {}", path.display());
    if !added.is_empty() {
        msg.push_str(&format!("\n  added: {}", added.join(", ")));
    }
    if !updated.is_empty() {
        msg.push_str(&format!("\n  updated path: {}", updated.join(", ")));
    }
    msg.push_str("\nRestart running Claude Code sessions to pick this up.");
    Ok(msg)
}

/// Remove our hooks from a Claude settings file. Returns a summary.
pub fn claude_remove(path: &Path) -> Result<String> {
    let text = read_opt(path)?;
    if text.is_none() {
        return Ok(format!("{} does not exist. Nothing to do.", path.display()));
    }
    let mut root = parse_settings(path, text.as_deref())?;
    let mut removed = vec![];
    if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        let events: Vec<String> = hooks.keys().cloned().collect();
        for ev in events {
            let Some(groups) = hooks.get_mut(&ev).and_then(Value::as_array_mut) else { continue };
            let mut n = 0;
            for g in groups.iter_mut() {
                if let Some(list) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                    let before = list.len();
                    list.retain(|h| !is_ours(h));
                    let d = before - list.len();
                    n += d;
                }
            }
            if n > 0 {
                groups.retain(|g| {
                    g.get("hooks").and_then(Value::as_array).is_none_or(|l| !l.is_empty())
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
        return Ok(format!("{}: no grace hooks found. Nothing to do.", path.display()));
    }
    let mut out = serde_json::to_string_pretty(&root)?;
    out.push('\n');
    write_atomic(path, &out)?;
    Ok(format!(
        "Updated {}\n  removed: {}\nRestart running Claude Code sessions to pick this up.",
        path.display(),
        removed.join(", ")
    ))
}

fn parse_toml(path: &Path, text: Option<&str>) -> Result<DocumentMut> {
    text.unwrap_or("")
        .parse::<DocumentMut>()
        .with_context(|| format!("{} is not valid TOML. Nothing changed", path.display()))
}

/// The notify array as strings. None when notify is not an array of strings.
fn notify_strings(item: &Item) -> Option<Vec<String>> {
    item.as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// True when the notify items start with `<exe> session hook codex`.
fn strs_are_ours(strs: &[String]) -> bool {
    strs.len() >= 4 && strs[1..4] == ["session", "hook", "codex"]
}

/// Items after `--then` in one of our notify arrays (empty when there is no `--then`).
fn chained_items(strs: &[String]) -> &[String] {
    match strs.get(4) {
        Some(w) if w == "--then" => &strs[5..],
        _ => &[],
    }
}

fn to_array(items: &[String]) -> Array {
    let mut arr = Array::new();
    for s in items {
        arr.push(s.as_str());
    }
    arr
}

/// Set `notify` in a Codex config file. Returns a summary.
/// An existing foreign notify program is kept: grace forwards each event to it.
pub fn codex_add(path: &Path, exe: &str) -> Result<String> {
    let text = read_opt(path)?;
    let mut doc = parse_toml(path, text.as_deref())?;
    let mut items: Vec<String> = ["session", "hook", "codex"].map(String::from).to_vec();
    items.insert(0, exe.to_string());
    let mut note = String::new();
    let verb = match doc.get("notify") {
        None => "added",
        Some(item) => {
            let Some(old) = notify_strings(item) else {
                bail!(
                    "{} already sets notify = {}\nCodex supports one notify program, and grace \
                     cannot chain this value. Merge grace by hand (call `{} session hook codex` \
                     from your program) or remove your notify line first. Nothing changed.",
                    path.display(),
                    item.to_string().trim(),
                    shell_quote(exe)
                );
            };
            if strs_are_ours(&old) {
                if old[0] == exe {
                    return Ok(format!(
                        "{}: grace notify already set up. Nothing to do.",
                        path.display()
                    ));
                }
                let rest = &old[4..];
                items.extend(rest.iter().cloned());
                "updated path"
            } else {
                items.push("--then".into());
                items.extend(old.iter().cloned());
                note = format!(
                    "\ngrace now forwards each Codex event to your old notify program:\n  {}",
                    old.iter().map(|s| shell_quote(s)).collect::<Vec<_>>().join(" ")
                );
                "chained"
            }
        }
    };
    doc["notify"] = Item::Value(TomlValue::Array(to_array(&items)));
    write_atomic(path, &doc.to_string())?;
    Ok(format!(
        "Updated {}\n  notify: {verb}{note}\nRestart running Codex sessions to pick this up.",
        path.display()
    ))
}

/// Undo our `notify`: restore the chained program, or remove the key when none.
pub fn codex_remove(path: &Path) -> Result<String> {
    let text = read_opt(path)?;
    if text.is_none() {
        return Ok(format!("{} does not exist. Nothing to do.", path.display()));
    }
    let mut doc = parse_toml(path, text.as_deref())?;
    let Some(item) = doc.get("notify") else {
        return Ok(format!("{}: no notify found. Nothing to do.", path.display()));
    };
    let strs = match notify_strings(item) {
        Some(s) if strs_are_ours(&s) => s,
        _ => {
            return Ok(format!(
                "{}: notify is not set by grace. Left it as is.",
                path.display()
            ))
        }
    };
    let has_then = strs.get(4).is_some_and(|w| w == "--then");
    let what = if has_then {
        doc["notify"] = Item::Value(TomlValue::Array(to_array(chained_items(&strs))));
        "restored your old program"
    } else {
        doc.remove("notify");
        "removed"
    };
    write_atomic(path, &doc.to_string())?;
    Ok(format!(
        "Updated {}\n  notify: {what}\nRestart running Codex sessions to pick this up.",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/opt/grace/bin/grace";

    fn read_json(p: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
    }

    fn count_ours(v: &Value) -> usize {
        v.to_string().matches(CLAUDE_MARK).count()
    }

    #[test]
    fn claude_add_to_empty() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("settings.json");
        claude_add(&p, EXE).unwrap();
        let v = read_json(&p);
        assert_eq!(v["hooks"].as_object().unwrap().len(), CLAUDE_EVENTS.len());
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "*");
        assert!(v["hooks"]["Stop"][0].get("matcher").is_none());
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"],
            format!("{EXE} {CLAUDE_MARK}")
        );
        assert!(!p.with_file_name("settings.json.grace-backup").exists());
    }

    #[test]
    fn claude_idempotent_and_updates_path() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        claude_add(&p, EXE).unwrap();
        let first = fs::read_to_string(&p).unwrap();
        claude_add(&p, EXE).unwrap();
        assert_eq!(first, fs::read_to_string(&p).unwrap());
        claude_add(&p, "/new/grace").unwrap();
        let v = read_json(&p);
        assert_eq!(count_ours(&v), CLAUDE_EVENTS.len());
        assert!(v.to_string().contains("/new/grace"));
        assert!(!v.to_string().contains(EXE));
    }

    #[test]
    fn claude_keeps_other_hooks_and_keys_then_remove() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        let orig = json!({
            "model": "opus",
            "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "echo hi" } ] } ] },
            "theme": "dark"
        });
        fs::write(&p, serde_json::to_string_pretty(&orig).unwrap()).unwrap();
        claude_add(&p, EXE).unwrap();
        let v = read_json(&p);
        assert_eq!(v["model"], "opus");
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "echo hi");
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert!(p.with_file_name("settings.json.grace-backup").exists());
        claude_remove(&p).unwrap();
        assert_eq!(read_json(&p), orig);
    }

    #[test]
    fn claude_remove_drops_empty_hooks() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(&p, "{\"a\": 1}").unwrap();
        claude_add(&p, EXE).unwrap();
        claude_remove(&p).unwrap();
        assert_eq!(read_json(&p), json!({"a": 1}));
    }

    #[test]
    fn claude_invalid_json_errors() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(&p, "{ not json").unwrap();
        assert!(claude_add(&p, EXE).is_err());
        assert!(claude_remove(&p).is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "{ not json");
        assert!(!p.with_file_name("settings.json.grace-backup").exists());
    }

    #[test]
    fn codex_add_to_missing_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("nested").join("config.toml");
        codex_add(&p, EXE).unwrap();
        let t = fs::read_to_string(&p).unwrap();
        assert!(t.contains("notify"));
        let doc: DocumentMut = t.parse().unwrap();
        let a: Vec<&str> = doc["notify"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(a, [EXE, "session", "hook", "codex"]);
        // idempotent
        codex_add(&p, EXE).unwrap();
        assert_eq!(t, fs::read_to_string(&p).unwrap());
    }

    #[test]
    fn codex_keeps_comments_and_updates_path() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        fs::write(&p, "# my config\nmodel = \"o3\"  # fast\n\n[tui]\nx = 1\n").unwrap();
        codex_add(&p, EXE).unwrap();
        let t = fs::read_to_string(&p).unwrap();
        assert!(t.contains("# my config") && t.contains("# fast") && t.contains("[tui]"));
        assert!(p.with_file_name("config.toml.grace-backup").exists());
        codex_add(&p, "/new/grace").unwrap();
        let t = fs::read_to_string(&p).unwrap();
        assert!(t.contains("/new/grace") && !t.contains(EXE) && t.contains("# my config"));
        // notify is a top-level key even with tables present
        let doc: DocumentMut = t.parse().unwrap();
        assert!(doc.get("notify").is_some());
    }

    fn notify_of(p: &Path) -> Vec<String> {
        let doc: DocumentMut = fs::read_to_string(p).unwrap().parse().unwrap();
        notify_strings(&doc["notify"]).unwrap()
    }

    #[test]
    fn codex_refuses_weird_notify() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        let src = "notify = \"say done\"\n";
        fs::write(&p, src).unwrap();
        let e = codex_add(&p, EXE).unwrap_err().to_string();
        assert!(e.contains("say done") && e.contains("one notify program"));
        assert_eq!(fs::read_to_string(&p).unwrap(), src);
        codex_remove(&p).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), src);
    }

    #[test]
    fn codex_chains_foreign_notify_and_restores() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        let src = "# top\nnotify = [\"/a b/Client\", \"turn-ended\"]  # mine\nmodel = \"o3\"\n";
        fs::write(&p, src).unwrap();
        let msg = codex_add(&p, EXE).unwrap();
        assert!(msg.contains("forwards"));
        assert_eq!(
            notify_of(&p),
            [EXE, "session", "hook", "codex", "--then", "/a b/Client", "turn-ended"]
        );
        let t = fs::read_to_string(&p).unwrap();
        assert!(t.contains("# top") && t.contains("model"));
        assert!(p.with_file_name("config.toml.grace-backup").exists());
        // idempotent
        codex_add(&p, EXE).unwrap();
        assert_eq!(t, fs::read_to_string(&p).unwrap());
        // new exe path keeps the chain
        codex_add(&p, "/new/grace").unwrap();
        assert_eq!(
            notify_of(&p),
            ["/new/grace", "session", "hook", "codex", "--then", "/a b/Client", "turn-ended"]
        );
        // remove restores the exact original array
        codex_remove(&p).unwrap();
        assert_eq!(notify_of(&p), ["/a b/Client", "turn-ended"]);
        let t = fs::read_to_string(&p).unwrap();
        assert!(t.contains("# top") && t.contains("model"));
    }

    #[test]
    fn unknown_target_hint() {
        let e = run("t3code", false).unwrap_err().to_string();
        assert!(e.contains("grace can set up: claude, codex.") && e.contains("T3 Code"));
        let e = run("foo", false).unwrap_err().to_string();
        assert!(!e.contains("T3"));
    }

    #[test]
    fn codex_remove_ours() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        fs::write(&p, "# c\nmodel = \"o3\"\n").unwrap();
        codex_add(&p, EXE).unwrap();
        codex_remove(&p).unwrap();
        let t = fs::read_to_string(&p).unwrap();
        assert!(!t.contains("notify") && t.contains("# c") && t.contains("model"));
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("/a/b"), "/a/b");
        if cfg!(unix) {
            assert_eq!(shell_quote("/a b/c"), "'/a b/c'");
        }
    }
}
