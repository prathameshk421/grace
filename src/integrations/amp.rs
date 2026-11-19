//! amp: a plugin file we own, `<config>/plugins/grace.ts`.
//!
//! Verified (https://ampcode.com/manual/plugin-api and /docs/customize/plugins, fetched 2026-11-19):
//! - System plugins live in `$XDG_CONFIG_HOME/amp/plugins/`, else `~/.config/amp/plugins/`.
//!   A single-file plugin is a `.ts` or `.js` file there. Plugins run in Bun.
//! - A plugin is `export default function (amp) { ... }` with an optional static
//!   `export const description = '...'` (at most 300 characters).
//! - `amp.on(name, handler)` events: `session.start`, `agent.start`, `agent.end` (with
//!   `status` done, error or cancelled), plus `tool.call`, `tool.result`, `changes.prompt`.
//! - `amp.$` is a tagged-template shell runner that is not tied to one hook call.
//!   It resolves (never rejects) on a non-zero exit, and has no `.nothrow()` or `.quiet()`.
//! - `amp.system.workspaceRoot` is a URI or null. `amp.helpers.filePathFromURI` makes a path.
//! - New threads load plugins. A running thread needs `plugins: reload`.
//!
//! Mapping: agent.start (Working), agent.end (Ready, for every status), session.start (Ready).
//! Amp documents no "needs you" event and no "session end" event, so grace never shows
//! Waiting or Done for amp. We do not subscribe to `tool.call`: it is a request event, and a
//! wrong result could change the approval flow.
//!
//! Assumed: the docs do not say if `amp.$` escapes interpolated values like Bun's `$`.
//! To be safe either way, the plugin passes the workspace root only when it has no shell
//! special characters. Otherwise grace gets no folder (cwd None). The call is not awaited
//! and has a `.catch`, so it never blocks or throws into amp.
//! `GRACE_AMP_DIR` replaces the amp config folder (the parent of `plugins/`).
use super::{HookResult, Integration};
use crate::model::Status;
use crate::setup::{read_opt, write_atomic};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

pub static INTEGRATION: Integration = Integration {
    name: "amp",
    tool: "amp",
    setup,
    reads_stdin: false,
    hook,
    stdout: "",
};

const MARK: &str = "Managed by grace";

fn base_dir() -> Result<PathBuf> {
    for (var, sub) in [("GRACE_AMP_DIR", ""), ("XDG_CONFIG_HOME", "amp")] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                return Ok(PathBuf::from(v).join(sub));
            }
        }
    }
    crate::platform::home_dir()
        .map(|h| h.join(".config").join("amp"))
        .ok_or_else(|| anyhow!("cannot find the home folder"))
}

fn setup(exe: &str, remove: bool) -> Result<String> {
    setup_in(&base_dir()?, exe, remove)
}

/// The exe arrives shell-quoted. Strip one layer of quotes for the JS string.
fn raw_exe(exe: &str) -> String {
    let e = exe.trim();
    if e.len() >= 2 && ((e.starts_with('\'') && e.ends_with('\'')) || (e.starts_with('"') && e.ends_with('"'))) {
        e[1..e.len() - 1].replace("'\\''", "'")
    } else {
        e.to_string()
    }
}

fn plugin_source(exe: &str) -> String {
    let js_exe = serde_json::to_string(&raw_exe(exe)).unwrap_or_else(|_| "\"grace\"".into());
    format!(
        r#"// {MARK}. Remove with: grace setup amp --remove
export const description = 'Reports the agent status (working, ready) to grace.'

const GRACE = {js_exe}
const PLAIN = /^[A-Za-z0-9_\/.\-:+@%]+$/

export default function (amp) {{
  let root = ''
  try {{
    const uri = amp.system && amp.system.workspaceRoot
    if (uri) root = amp.helpers.filePathFromURI(uri) || ''
  }} catch (_) {{}}
  if (!PLAIN.test(root)) root = ''

  const send = (key) => {{
    try {{
      // Fire and forget: no await, so amp never waits for grace.
      const p = root
        ? amp.$`${{GRACE}} session hook amp ${{key}} ${{root}}`
        : amp.$`${{GRACE}} session hook amp ${{key}}`
      if (p && typeof p.catch === 'function') p.catch(() => {{}})
    }} catch (_) {{}}
  }}

  amp.on('session.start', () => {{ send('ready') }})
  amp.on('agent.start', () => {{ send('working'); return undefined }})
  amp.on('agent.end', () => {{ send('ready'); return undefined }})
}}
"#
    )
}

fn setup_in(base: &Path, exe: &str, remove: bool) -> Result<String> {
    let path = base.join("plugins").join("grace.ts");
    let current = read_opt(&path)?;
    if remove {
        return match current {
            Some(t) if t.starts_with(&format!("// {MARK}")) => {
                std::fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
                Ok(format!(
                    "Removed {}\nRestart running amp sessions to pick this up.",
                    path.display()
                ))
            }
            Some(_) => Ok(format!("{} is not managed by grace. Left it alone", path.display())),
            None => Ok(format!("{} does not exist. Nothing to do.", path.display())),
        };
    }
    if let Some(t) = &current {
        if !t.starts_with(&format!("// {MARK}")) {
            anyhow::bail!("{} exists and is not managed by grace. Nothing changed", path.display());
        }
    }
    let src = plugin_source(exe);
    if current.as_deref() == Some(src.as_str()) {
        return Ok(format!("{}: already set up. Nothing to do.", path.display()));
    }
    write_atomic(&path, &src)?;
    Ok(format!(
        "Wrote {}\n  events: session.start, agent.start, agent.end\nRestart running amp sessions to pick this up.",
        path.display()
    ))
}

fn hook(args: &[String], _stdin: &str) -> HookResult {
    let status = match args.first().map(String::as_str) {
        Some("working") => Status::Working,
        Some("ready") => Status::Ready,
        _ => return HookResult::default(),
    };
    let cwd = args.get(1).filter(|d| !d.is_empty()).cloned();
    HookResult { status: Some(status), cwd }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_idempotent_remove() {
        let d = tempfile::tempdir().unwrap();
        let other = d.path().join("plugins/other.ts");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, "// mine").unwrap();
        assert!(setup_in(d.path(), "/bin/grace", false).unwrap().contains("Wrote"));
        let p = d.path().join("plugins/grace.ts");
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.starts_with("// Managed by grace. Remove with: grace setup amp --remove\n"));
        assert!(t.contains("const GRACE = \"/bin/grace\""));
        assert!(t.contains("amp.$`${GRACE} session hook amp ${key} ${root}`"));
        assert!(setup_in(d.path(), "/bin/grace", false).unwrap().contains("Nothing to do"));
        assert!(setup_in(d.path(), "'/a b/grace'", false).unwrap().contains("Wrote"));
        assert!(std::fs::read_to_string(&p).unwrap().contains("const GRACE = \"/a b/grace\""));
        setup_in(d.path(), "/bin/grace", true).unwrap();
        assert!(!p.exists());
        assert!(other.exists());
        assert!(setup_in(d.path(), "/bin/grace", true).unwrap().contains("Nothing to do"));
    }

    #[test]
    fn foreign_file_kept() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("plugins/grace.ts");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "// mine").unwrap();
        assert!(setup_in(d.path(), "/bin/grace", false).is_err());
        setup_in(d.path(), "/bin/grace", true).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "// mine");
    }

    #[test]
    fn hook_args() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let r = hook(&a(&["working", "/p"]), "");
        assert_eq!(r.status, Some(Status::Working));
        assert_eq!(r.cwd.as_deref(), Some("/p"));
        assert_eq!(hook(&a(&["ready"]), "").cwd, None);
        assert_eq!(hook(&a(&["ready", ""]), "").cwd, None);
        assert_eq!(hook(&a(&["waiting"]), ""), HookResult::default());
        assert_eq!(hook(&[], ""), HookResult::default());
    }
}
