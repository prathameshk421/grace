//! opencode: a plugin file we own, `<config>/plugins/grace.js`.
//!
//! Verified: docs https://opencode.ai/docs/plugins/ say global plugins load from the plural
//! `~/.config/opencode/plugins/` folder. Local SDK types (@opencode-ai/plugin 1.4.0) give the
//! signature `async ({ $, directory }) => ({ event })`. The `plugin` array in opencode.json
//! is a different mechanism (npm packages). We never touch it.
//! Assumed: the event names and `properties.status.type` shape from the task brief
//! (session.status busy/idle, session.idle, permission.updated/asked/replied, session.created,
//! session.deleted). Not run against a live opencode session.
//!
//! The base folder is `$GRACE_OPENCODE_DIR`, else `$XDG_CONFIG_HOME/opencode`, else `~/.config/opencode`.
//! The plugin calls grace without await, so a slow or failing hook never slows the agent.
use super::{HookResult, Integration};
use crate::model::Status;
use crate::setup::{read_opt, write_atomic};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub static INTEGRATION: Integration = Integration {
    name: "opencode",
    tool: "opencode",
    setup,
    reads_stdin: false,
    hook,
    stdout: "",
};

const MARK: &str = "Managed by grace";

fn base_dir() -> Result<PathBuf> {
    if let Ok(v) = std::env::var("GRACE_OPENCODE_DIR") {
        if !v.is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    if let Ok(v) = std::env::var("XDG_CONFIG_HOME") {
        if !v.is_empty() {
            return Ok(PathBuf::from(v).join("opencode"));
        }
    }
    crate::setup::env_dir("GRACE_OPENCODE_DIR", ".config/opencode")
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
        r#"// {MARK}. Remove with: grace setup opencode --remove
const GRACE = {js_exe};

// OpenCode 2.x event names first, then the 1.x names.
const MAP = {{
  "session.execution.started": "working",
  "session.step.started": "working",
  "session.tool.called": "working",
  "session.execution.succeeded": "ready",
  "session.execution.failed": "ready",
  "session.execution.interrupted": "ready",
  "permission.asked": "waiting",
  "permission.replied": "working",
  "session.created": "ready",
  "session.deleted": "done",
  "session.idle": "ready",
  "permission.updated": "waiting",
}};

function keyFor(event) {{
  if (event.type === "session.status") {{
    const t = event.properties && event.properties.status && event.properties.status.type;
    if (t === "busy") return "working";
    if (t === "idle") return "ready";
    return null;
  }}
  return MAP[event.type] || null;
}}

export const Grace = async ({{ $, directory }}) => ({{
  event: async ({{ event }}) => {{
    try {{
      const key = keyFor(event);
      if (!key) return;
      // Fire and forget: no await, so the agent never waits for grace.
      $`${{GRACE}} session hook opencode ${{key}} ${{directory}}`.quiet().nothrow();
    }} catch (_) {{}}
  }},
}});
"#
    )
}

fn setup_in(base: &Path, exe: &str, remove: bool) -> Result<String> {
    let path = base.join("plugins").join("grace.js");
    let current = read_opt(&path)?;
    if remove {
        return match current {
            Some(t) if t.contains(MARK) => {
                std::fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
                Ok(format!("opencode: removed {}", path.display()))
            }
            Some(_) => Ok(format!("opencode: {} is not managed by grace. Left it alone", path.display())),
            None => Ok("opencode: nothing to remove".into()),
        };
    }
    let src = plugin_source(exe);
    if current.as_deref() == Some(src.as_str()) {
        return Ok(format!("opencode: already set up ({})", path.display()));
    }
    write_atomic(&path, &src)?;
    Ok(format!("opencode: wrote {}", path.display()))
}

fn hook(args: &[String], _stdin: &str) -> HookResult {
    let status = match args.first().map(String::as_str) {
        Some("working") => Status::Working,
        Some("waiting") => Status::Waiting,
        Some("ready") => Status::Ready,
        Some("done") => Status::Done,
        _ => return HookResult::default(),
    };
    let cwd = args.get(1).filter(|d| !d.is_empty()).cloned();
    HookResult { status: Some(status), cwd }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("grace-oc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn add_idempotent_remove() {
        let d = tmp("a");
        assert!(setup_in(&d, "/bin/grace", false).unwrap().contains("wrote"));
        let p = d.join("plugins/grace.js");
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.starts_with("// Managed by grace. Remove with: grace setup opencode --remove"));
        assert!(t.contains("const GRACE = \"/bin/grace\";"));
        assert!(setup_in(&d, "/bin/grace", false).unwrap().contains("already set up"));
        setup_in(&d, "/bin/grace", true).unwrap();
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn remove_keeps_foreign_file() {
        let d = tmp("b");
        std::fs::create_dir_all(d.join("plugins")).unwrap();
        std::fs::write(d.join("plugins/grace.js"), "// mine").unwrap();
        setup_in(&d, "/bin/grace", true).unwrap();
        assert!(d.join("plugins/grace.js").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn quoted_exe() {
        assert_eq!(raw_exe("'/a b/grace'"), "/a b/grace");
        assert_eq!(raw_exe("/x/grace"), "/x/grace");
    }

    #[test]
    fn hook_args() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let r = hook(&a(&["waiting", "/p"]), "");
        assert_eq!(r.status, Some(Status::Waiting));
        assert_eq!(r.cwd.as_deref(), Some("/p"));
        assert_eq!(hook(&a(&["done"]), "").cwd, None);
        assert_eq!(hook(&a(&["bogus"]), ""), HookResult::default());
        assert_eq!(hook(&[], ""), HookResult::default());
    }
}
