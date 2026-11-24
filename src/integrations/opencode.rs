//! opencode: a plugin file we own, `<config>/plugins/grace.js`.
use super::{
    managed_file_check, remove_owned_backup, simple_arg_hook, HookResult, HookStatus, Integration,
};
use crate::model::Status;
use crate::platform;
use crate::setup::{read_opt, write_atomic};
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

pub static INTEGRATION: Integration = Integration {
    name: "opencode",
    label: "OpenCode",
    tool: "opencode",
    setup,
    check,
    reads_stdin: false,
    hook,
    stdout: "",
};

const MARK: &str = "Managed by grace";

fn base_dir() -> Result<PathBuf> {
    platform::xdg_config_dir(Some("GRACE_OPENCODE_DIR"), "opencode")
        .ok_or_else(|| anyhow!("cannot find the home folder"))
}

fn setup(exe: &str, remove: bool) -> Result<String> {
    setup_in(&base_dir()?, exe, remove)
}

fn is_ours_text(t: &str) -> bool {
    t.contains(MARK)
}

fn check(exe: &str) -> HookStatus {
    let Ok(base) = base_dir() else {
        return HookStatus::NotInstalled;
    };
    let path = base.join("plugins").join("grace.js");
    managed_file_check(
        path,
        Some(&base),
        "opencode",
        "custom grace.js",
        is_ours_text,
        |cur| cur == plugin_source(exe),
    )
}

fn plugin_source(exe: &str) -> String {
    let js_exe = serde_json::to_string(exe).unwrap_or_else(|_| "\"grace\"".into());
    format!(
        r#"// {MARK}. Remove with: grace setup opencode --remove
import {{ spawn }} from "node:child_process";

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
  "location.shutdown": "done",
  "session.idle": "ready",
  "permission.updated": "waiting",
}};

function keyFor(event) {{
  if (event.type === "session.status") {{
    const s = (event.properties && event.properties.status) || (event.data && event.data.status);
    const t = s && s.type;
    if (t === "busy") return "working";
    if (t === "idle") return "ready";
    return null;
  }}
  return MAP[event.type] || null;
}}

// Fire and forget: the agent never waits for grace, and errors are ignored.
function send(key, dir) {{
  try {{
    const child = spawn(GRACE, ["session", "hook", "opencode", key, dir || ""], {{
      stdio: "ignore",
      detached: true,
      windowsHide: true,
    }});
    child.on("error", () => {{}});
    child.unref();
  }} catch (_) {{}}
}}

export default {{
  id: "grace",
  // OpenCode 2.x
  async setup(ctx) {{
    const home = ctx.location && ctx.location.directory;
    const stop = new AbortController();
    (async () => {{
      for await (const event of ctx.event.subscribe({{ signal: stop.signal }})) {{
        const key = keyFor(event);
        if (key) send(key, (event.location && event.location.directory) || home);
      }}
    }})().catch(() => {{}});
    return () => stop.abort();
  }},
  // OpenCode 1.x
  async server({{ directory }}) {{
    return {{
      event: async ({{ event }}) => {{
        const key = keyFor(event);
        if (key) send(key, directory);
      }},
    }};
  }},
}};
"#
    )
}

fn setup_in(base: &Path, exe: &str, remove: bool) -> Result<String> {
    let path = base.join("plugins").join("grace.js");
    let current = read_opt(&path)?;
    if remove {
        return match current {
            Some(t) if is_ours_text(&t) => {
                std::fs::remove_file(&path)
                    .with_context(|| format!("cannot remove {}", path.display()))?;
                remove_owned_backup(&path, MARK);
                Ok(format!("opencode: removed {}", path.display()))
            }
            Some(_) => Ok(format!(
                "opencode: {} is not managed by grace. Left it alone",
                path.display()
            )),
            None => Ok("opencode: nothing to remove".into()),
        };
    }
    if let Some(t) = &current {
        if !is_ours_text(t) {
            bail!(
                "{} exists and is not managed by grace. Nothing changed",
                path.display()
            );
        }
    }
    let src = plugin_source(exe);
    if current.as_deref() == Some(src.as_str()) {
        return Ok(format!("opencode: already set up ({})", path.display()));
    }
    write_atomic(&path, &src)?;
    Ok(format!("opencode: wrote {}", path.display()))
}

fn hook(args: &[String], _stdin: &str) -> HookResult {
    simple_arg_hook(args, |s| match s {
        "working" => Some(Status::Working),
        "waiting" => Some(Status::Waiting),
        "ready" => Some(Status::Ready),
        "done" => Some(Status::Done),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_idempotent_remove() {
        let d = tempfile::tempdir().unwrap();
        assert!(setup_in(d.path(), "/bin/grace", false)
            .unwrap()
            .contains("wrote"));
        let p = d.path().join("plugins/grace.js");
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.starts_with("// Managed by grace. Remove with: grace setup opencode --remove"));
        assert!(t.contains("const GRACE = \"/bin/grace\";"));
        assert!(t.contains("windowsHide: true"));
        assert!(setup_in(d.path(), "/bin/grace", false)
            .unwrap()
            .contains("already set up"));
        setup_in(d.path(), "/bin/grace", true).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn foreign_file_kept_on_install_and_remove() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("plugins")).unwrap();
        std::fs::write(d.path().join("plugins/grace.js"), "// mine").unwrap();
        assert!(setup_in(d.path(), "/bin/grace", false).is_err());
        setup_in(d.path(), "/bin/grace", true).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join("plugins/grace.js")).unwrap(),
            "// mine"
        );
    }

    #[test]
    fn quoted_exe() {
        let s = plugin_source("/a b/grace");
        assert!(s.contains("const GRACE = \"/a b/grace\";"));
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
