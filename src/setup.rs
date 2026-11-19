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
        other => bail!("unknown target '{other}'. Use one of: claude, codex"),
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

fn notify_is_ours(item: &Item) -> bool {
    item.as_array().is_some_and(|a| {
        let strs: Vec<&str> = a.iter().filter_map(|v| v.as_str()).collect();
        ["session", "hook", "codex"].iter().all(|w| strs.contains(w))
    })
}

/// Set `notify` in a Codex config file. Returns a summary.
pub fn codex_add(path: &Path, exe: &str) -> Result<String> {
    let text = read_opt(path)?;
    let mut doc = parse_toml(path, text.as_deref())?;
    let mut arr = Array::new();
    for s in [exe, "session", "hook", "codex"] {
        arr.push(s);
    }
    let verb = match doc.get("notify") {
        None => "added",
        Some(item) if notify_is_ours(item) => {
            let same = item.as_array().is_some_and(|a| {
                a.iter().filter_map(|v| v.as_str()).next() == Some(exe)
            });
            if same {
                return Ok(format!(
                    "{}: grace notify already set up. Nothing to do.",
                    path.display()
                ));
            }
            "updated path"
        }
        Some(item) => bail!(
            "{} already sets notify = {}\nCodex supports one notify program. Merge grace by hand \
             (call `{} session hook codex` from your program) or remove your notify line first. \
             Nothing changed.",
            path.display(),
            item.to_string().trim(),
            shell_quote(exe)
        ),
    };
    doc["notify"] = Item::Value(TomlValue::Array(arr));
    write_atomic(path, &doc.to_string())?;
    Ok(format!(
        "Updated {}\n  notify: {verb}\nRestart running Codex sessions to pick this up.",
        path.display()
    ))
}

/// Remove `notify` from a Codex config file when it is ours.
pub fn codex_remove(path: &Path) -> Result<String> {
    let text = read_opt(path)?;
    if text.is_none() {
        return Ok(format!("{} does not exist. Nothing to do.", path.display()));
    }
    let mut doc = parse_toml(path, text.as_deref())?;
    match doc.get("notify") {
        None => Ok(format!("{}: no notify found. Nothing to do.", path.display())),
        Some(item) if !notify_is_ours(item) => Ok(format!(
            "{}: notify is not set by grace. Left it as is.",
            path.display()
        )),
        Some(_) => {
            doc.remove("notify");
            write_atomic(path, &doc.to_string())?;
            Ok(format!(
                "Updated {}\n  notify: removed\nRestart running Codex sessions to pick this up.",
                path.display()
            ))
        }
    }
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

    #[test]
    fn codex_refuses_foreign_notify() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        let src = "notify = [\"say\", \"done\"]\n";
        fs::write(&p, src).unwrap();
        let e = codex_add(&p, EXE).unwrap_err().to_string();
        assert!(e.contains("say") && e.contains("one notify program"));
        assert_eq!(fs::read_to_string(&p).unwrap(), src);
        // remove leaves foreign notify alone
        codex_remove(&p).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), src);
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
