//! `grace setup [target]`: connect CLI agents to grace so it knows their live status.
use crate::integrations::{self, HookStatus};
use crate::platform;
use crate::theme::Ansi;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Value as TomlValue};

/// Text that marks a Claude hook command as ours.
#[cfg(test)]
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

/// Normalize user-supplied target aliases (case-insensitive).
pub fn resolve_target(target: &str) -> &str {
    match target.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => "claude",
        "codex" | "openai-codex" => "codex",
        "gemini" | "gemini-cli" => "gemini",
        "qwen" | "qwen-code" => "qwen",
        "copilot" | "gh-copilot" | "github-copilot" => "copilot",
        "opencode" => "opencode",
        "agy" | "antigravity" | "antigravity-cli" => "agy",
        "crush" => "crush",
        "goose" => "goose",
        "amp" => "amp",
        "aider" => "aider",
        _ => target.trim(),
    }
}

/// Inspect all 11 supported CLI agents against `exe`.
pub fn inspect_all(exe: &str) -> Vec<(&'static str, &'static str, HookStatus)> {
    let mut out = vec![
        ("claude", "Claude Code", check_claude(exe)),
        ("codex", "Codex CLI", check_codex(exe)),
    ];
    for integ in integrations::all() {
        out.push((integ.name, integ.label, (integ.check)(exe)));
    }
    out
}

/// Entry point for `grace setup [target] [--all] [--remove]`.
pub fn run(target: Option<&str>, all: bool, remove: bool) -> Result<()> {
    if all && target.is_some() {
        bail!("cannot combine an agent name with `--all`");
    }
    if remove && !all && target.is_none() {
        bail!("specify an agent name or `--all` with `--remove`");
    }
    let exe = exe_string()?;
    if all {
        return run_all(&exe, remove);
    }
    let Some(raw) = target else {
        return print_overview(&exe);
    };
    let canonical = resolve_target(raw);
    let msg = run_one(canonical, &exe, remove)?;
    eprintln!("{msg}");
    Ok(())
}

fn run_one(target: &str, exe: &str, remove: bool) -> Result<String> {
    match target {
        "claude" => {
            let path = claude_settings_path()?;
            if remove {
                claude_remove(&path)
            } else {
                claude_add(&path, exe)
            }
        }
        "codex" => {
            let path = codex_config_path()?;
            if remove {
                codex_remove(&path)
            } else {
                codex_add(&path, exe)
            }
        }
        other if integrations::find(other).is_some() => {
            let integ = integrations::find(other).unwrap();
            (integ.setup)(exe, remove)
        }
        other => {
            let hint = if other.to_ascii_lowercase().starts_with("t3") {
                " T3 Code has no hook system; if it runs the Claude Code CLI, your Claude hooks already report its sessions."
            } else {
                ""
            };
            let names: Vec<&str> = ["claude", "codex"]
                .into_iter()
                .chain(integrations::all().into_iter().map(|i| i.name))
                .collect();
            bail!(
                "unknown target '{other}'. grace can set up: {}.{hint}",
                names.join(", ")
            )
        }
    }
}

fn run_all(exe: &str, remove: bool) -> Result<()> {
    let statuses = inspect_all(exe);
    let mut touched = 0usize;
    for (name, label, status) in statuses {
        let should_run = if remove {
            matches!(
                status,
                HookStatus::Connected(_) | HookStatus::OutdatedPath(_)
            )
        } else {
            matches!(
                status,
                HookStatus::Available(_) | HookStatus::OutdatedPath(_)
            )
        };
        if !should_run {
            continue;
        }
        match run_one(name, exe, remove) {
            Ok(msg) => {
                eprintln!("[{label}]\n{msg}\n");
                touched += 1;
            }
            Err(e) => eprintln!("[{label}] skipped: {e}"),
        }
    }
    if touched == 0 {
        if remove {
            eprintln!("No grace agent hooks found to remove.");
        } else {
            eprintln!("All detected CLI agents are already connected (or none installed yet).");
            eprintln!(
                "Run `grace setup <agent>` to connect one explicitly (e.g. `grace setup claude`)."
            );
        }
    }
    Ok(())
}

fn print_overview(exe: &str) -> Result<()> {
    let cfg = crate::config::load();
    let a = Ansi::stderr(&cfg);
    let home = platform::home_dir().map(|h| platform::clean_path_str(&h.to_string_lossy()));
    let short = |p: &Path| platform::short_path(p, home.as_deref());
    eprintln!();
    eprintln!(
        "  {}{} grace setup{} {}— connect CLI agents for live session status{}",
        a.gold, a.star, a.reset, a.dim, a.reset
    );
    eprintln!();
    eprintln!(
        "  {}{:<10}  {:<13}  CONFIG{}",
        a.dim, "AGENT", "STATUS", a.reset
    );
    for (name, _label, st) in inspect_all(exe) {
        let (badge, cfg_s, action) = match st {
            HookStatus::Connected(p) => (
                format!("{}{} connected{}  ", a.green, a.check, a.reset),
                short(&p),
                String::new(),
            ),
            HookStatus::OutdatedPath(p) => (
                format!("{}! outdated{}   ", a.yellow, a.reset),
                short(&p),
                format!("  {}→ grace setup {name}{}", a.dim, a.reset),
            ),
            HookStatus::Available(p) => (
                format!("{}○ available{}  ", a.yellow, a.reset),
                short(&p),
                format!("  {}→ grace setup {name}{}", a.dim, a.reset),
            ),
            HookStatus::NotInstalled => (
                format!("{}· not found{}  ", a.dim, a.reset),
                String::new(),
                String::new(),
            ),
            HookStatus::Conflict(p, why) => (
                format!("{}! conflict{}   ", a.yellow, a.reset),
                format!("{} ({})", short(&p), platform::sanitize_display(&why)),
                String::new(),
            ),
        };
        eprintln!("  {name:<10}  {badge}  {cfg_s}{action}");
    }
    eprintln!();
    eprintln!(
        "  {}Run `grace setup <agent>` to connect one, or `grace setup --all` for all installed agents.{}",
        a.dim, a.reset
    );
    Ok(())
}

pub(crate) fn env_dir(var: &str, default_sub: &str) -> Result<PathBuf> {
    platform::env_dir(var, default_sub).ok_or_else(|| anyhow!("cannot find the home folder"))
}

pub fn claude_settings_path() -> Result<PathBuf> {
    Ok(env_dir("CLAUDE_CONFIG_DIR", ".claude")?.join("settings.json"))
}

pub fn codex_config_path() -> Result<PathBuf> {
    Ok(env_dir("CODEX_HOME", ".codex")?.join("config.toml"))
}

/// Absolute path of the running grace binary, one that survives upgrades.
pub fn exe_string() -> Result<String> {
    let exe = platform::stable_exe().context("cannot find the grace binary")?;
    let s = platform::clean_path_str(&exe.to_string_lossy());
    if platform::has_line_break_or_nul(&s) {
        bail!("executable path contains unsafe newline or NUL characters");
    }
    Ok(s)
}

pub fn check_claude(exe: &str) -> HookStatus {
    let Ok(path) = claude_settings_path() else {
        return HookStatus::NotInstalled;
    };
    json_hooks_check(&path, exe, "claude", CLAUDE_EVENTS)
}

pub fn check_codex(exe: &str) -> HookStatus {
    let Ok(path) = codex_config_path() else {
        return HookStatus::NotInstalled;
    };
    let Ok(Some(text)) = read_opt(&path) else {
        if path.parent().is_some_and(|p| p.is_dir()) || crate::tools::on_path("codex") {
            return HookStatus::Available(path);
        }
        return HookStatus::NotInstalled;
    };
    let Ok(doc) = parse_toml(&path, Some(&text)) else {
        return HookStatus::Conflict(path, "invalid TOML".into());
    };
    let Some(item) = doc.get("notify") else {
        return HookStatus::Available(path);
    };
    let Some(strs) = notify_strings(item) else {
        return HookStatus::Conflict(path, "non-array notify".into());
    };
    if strs_are_ours(&strs) {
        if strs[0] == exe {
            HookStatus::Connected(path)
        } else {
            HookStatus::OutdatedPath(path)
        }
    } else {
        HookStatus::Available(path)
    }
}

/// Write a file atomically with strict symlink checks, private backup creation,
/// unique O_EXCL temp file, permission preservation (default `0600`), and Windows retry.
pub(crate) fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("bad path {}", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("bad path {}", path.display()))?
        .to_string_lossy()
        .to_string();
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;

    #[cfg(unix)]
    let orig_mode: Option<u32> = {
        use std::os::unix::fs::PermissionsExt;
        match fs::symlink_metadata(path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    bail!("refusing to overwrite symlink {}", path.display());
                }
                Some(meta.permissions().mode() & 0o777)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("cannot stat {}", path.display())),
        }
    };
    #[cfg(not(unix))]
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            bail!("refusing to overwrite symlink {}", path.display());
        }
    }

    if path.exists() {
        let backup = dir.join(format!("{name}.grace-backup"));
        if fs::symlink_metadata(&backup).is_err() {
            let existing = fs::read(path)
                .with_context(|| format!("cannot read {} for backup", path.display()))?;
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            match opts.open(&backup) {
                Ok(mut f) => {
                    f.write_all(&existing)
                        .with_context(|| format!("cannot write backup {}", backup.display()))?;
                    let _ = f.sync_all();
                    drop(f);
                    platform::restrict_file_0600(&backup);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("cannot create backup {}", backup.display()))
                }
            }
        }
    }

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!("{name}.grace-tmp-{}-{nanos}", std::process::id()));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(orig_mode.unwrap_or(0o600));
    }
    let mut f = opts
        .open(&tmp)
        .with_context(|| format!("cannot create {}", tmp.display()))?;
    if let Err(e) = f.write_all(content.as_bytes()).and_then(|_| f.sync_all()) {
        drop(f);
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("cannot write {}", tmp.display()));
    }
    drop(f);
    #[cfg(unix)]
    if let Some(m) = orig_mode {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(m));
    }
    // Retry briefly on Windows if an editor or AV holds a transient share lock.
    let mut last_err = None;
    for attempt in 0..4 {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e)
                if cfg!(windows)
                    && attempt < 3
                    && e.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => {
                last_err = Some(e);
                break;
            }
        }
    }
    let _ = fs::remove_file(&tmp);
    Err(last_err.unwrap()).with_context(|| format!("cannot replace {}", path.display()))
}

pub(crate) fn read_opt(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

pub(crate) fn parse_settings(path: &Path, text: Option<&str>) -> Result<Value> {
    match text {
        None => Ok(json!({})),
        Some(t) if t.trim().is_empty() => Ok(json!({})),
        Some(t) => {
            let v: Value = serde_json::from_str(t).with_context(|| {
                format!("{} is not valid JSON. Nothing changed", path.display())
            })?;
            if !v.is_object() {
                bail!("{} is not a JSON object. Nothing changed", path.display());
            }
            Ok(v)
        }
    }
}

fn is_ours_hook(hook: &Value, mark: &str) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(mark))
}

pub(crate) fn json_hooks_check(
    path: &Path,
    exe: &str,
    name: &str,
    events: &[(&str, Option<&str>)],
) -> HookStatus {
    let Ok(Some(text)) = read_opt(path) else {
        if path.parent().is_some_and(|p| p.is_dir()) || crate::tools::on_path(name) {
            return HookStatus::Available(path.to_path_buf());
        }
        return HookStatus::NotInstalled;
    };
    let Ok(root) = parse_settings(path, Some(&text)) else {
        return HookStatus::Conflict(path.to_path_buf(), "invalid JSON".into());
    };
    let Some(hooks_val) = root.get("hooks") else {
        return HookStatus::Available(path.to_path_buf());
    };
    let Some(hooks) = hooks_val.as_object() else {
        return HookStatus::Conflict(path.to_path_buf(), "'hooks' is not an object".into());
    };
    let mark = format!("session hook {name}");
    let want = format!("{} {mark}", platform::quote_cmd(exe));
    let mut has_ours = false;
    let mut matches_all = true;
    for (event, _) in events {
        let Some(ev_val) = hooks.get(*event) else {
            matches_all = false;
            continue;
        };
        let Some(groups) = ev_val.as_array() else {
            return HookStatus::Conflict(
                path.to_path_buf(),
                format!("hooks.{event} is not an array"),
            );
        };
        let found = groups
            .iter()
            .filter_map(|g| g.get("hooks").and_then(Value::as_array))
            .flatten()
            .find(|h| is_ours_hook(h, &mark));
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

/// Add or update our command hooks in a Claude-style JSON settings file. Returns a summary.
pub(crate) fn json_hooks_add(
    path: &Path,
    exe: &str,
    name: &str,
    events: &[(&str, Option<&str>)],
    label: &str,
) -> Result<String> {
    let mark = format!("session hook {name}");
    let cmd = format!("{} {mark}", platform::quote_cmd(exe));
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
    for (event, matcher) in events {
        let groups = hooks
            .entry((*event).to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| {
                anyhow!(
                    "hooks.{event} in {} is not an array. Nothing changed",
                    path.display()
                )
            })?;
        let mut found = false;
        let mut changed = false;
        for g in groups.iter_mut() {
            let Some(list) = g.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            for h in list.iter_mut().filter(|h| is_ours_hook(h, &mark)) {
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
pub(crate) fn json_hooks_remove(path: &Path, name: &str, label: &str) -> Result<String> {
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
                    list.retain(|h| !is_ours_hook(h, &mark));
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

/// Add or update our hooks in a Claude settings file. Returns a summary.
pub fn claude_add(path: &Path, exe: &str) -> Result<String> {
    json_hooks_add(path, exe, "claude", CLAUDE_EVENTS, "Claude Code")
}

/// Remove our hooks from a Claude settings file. Returns a summary.
pub fn claude_remove(path: &Path) -> Result<String> {
    json_hooks_remove(path, "claude", "Claude Code")
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
    let verb =
        match doc.get("notify") {
            None => "added",
            Some(item) => {
                let Some(old) = notify_strings(item) else {
                    bail!(
                    "{} already sets notify = {}\nCodex supports one notify program, and grace \
                     cannot chain this value. Merge grace by hand (call `{} session hook codex` \
                     from your program) or remove your notify line first. Nothing changed.",
                    path.display(),
                    item.to_string().trim(),
                    platform::quote_posix(exe)
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
                    note =
                        format!(
                    "\ngrace now forwards each Codex event to your old notify program:\n  {}",
                    old.iter().map(|s| platform::quote_posix(s)).collect::<Vec<_>>().join(" ")
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
        return Ok(format!(
            "{}: no notify found. Nothing to do.",
            path.display()
        ));
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

    #[cfg(unix)]
    #[test]
    fn write_atomic_preserves_0600_mode_and_refuses_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(&p, "{}").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        claude_add(&p, EXE).unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(p.with_file_name("settings.json.grace-backup"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let link = d.path().join("link.json");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        assert!(write_atomic(&link, "{}").is_err());

        // Read-only source file (0o444) must still produce an owner-only 0o600 backup.
        let ro = d.path().join("ro.json");
        fs::write(&ro, "{}").unwrap();
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o444)).unwrap();
        write_atomic(&ro, "{\"a\":1}").unwrap();
        assert_eq!(
            fs::metadata(ro.with_file_name("ro.json.grace-backup"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn codex_add_to_missing_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("nested").join("config.toml");
        codex_add(&p, EXE).unwrap();
        let t = fs::read_to_string(&p).unwrap();
        assert!(t.contains("notify"));
        let doc: DocumentMut = t.parse().unwrap();
        let a: Vec<&str> = doc["notify"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
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
            [
                EXE,
                "session",
                "hook",
                "codex",
                "--then",
                "/a b/Client",
                "turn-ended"
            ]
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
            [
                "/new/grace",
                "session",
                "hook",
                "codex",
                "--then",
                "/a b/Client",
                "turn-ended"
            ]
        );
        // remove restores the exact original array
        codex_remove(&p).unwrap();
        assert_eq!(notify_of(&p), ["/a b/Client", "turn-ended"]);
        let t = fs::read_to_string(&p).unwrap();
        assert!(t.contains("# top") && t.contains("model"));
    }

    #[test]
    fn unknown_target_hint() {
        let e = run(Some("t3code"), false, false).unwrap_err().to_string();
        assert!(e.contains("grace can set up: claude, codex, gemini") && e.contains("T3 Code"));
        let e = run(Some("foo"), false, false).unwrap_err().to_string();
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
    fn json_hooks_conflict_and_setup_cli_guards() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(&p, r#"{"hooks": []}"#).unwrap();
        assert!(matches!(
            json_hooks_check(&p, EXE, "claude", CLAUDE_EVENTS),
            HookStatus::Conflict(_, _)
        ));
        fs::write(&p, r#"{"hooks": {"Stop": {}}}"#).unwrap();
        assert!(matches!(
            json_hooks_check(&p, EXE, "claude", CLAUDE_EVENTS),
            HookStatus::Conflict(_, _)
        ));
        assert!(run(Some("claude"), true, false).is_err());
        assert!(run(None, false, true).is_err());
    }
}
