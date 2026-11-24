//! amp: a plugin file we own, `<config>/plugins/grace.ts`.
use super::{HookResult, HookStatus, Integration};
use crate::model::Status;
use crate::setup::{read_opt, write_atomic};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

pub static INTEGRATION: Integration = Integration {
    name: "amp",
    label: "Amp",
    tool: "amp",
    setup,
    check,
    reads_stdin: false,
    hook,
    stdout: "",
};

const MARK: &str = "Managed by grace";

fn base_dir() -> Result<PathBuf> {
    if let Ok(v) = std::env::var("GRACE_AMP_DIR") {
        if !v.is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    if let Ok(v) = std::env::var("XDG_CONFIG_HOME") {
        if !v.is_empty() && Path::new(&v).is_absolute() {
            return Ok(PathBuf::from(v).join("amp"));
        }
    }
    crate::platform::home_dir()
        .map(|h| h.join(".config").join("amp"))
        .ok_or_else(|| anyhow!("cannot find the home folder"))
}

fn setup(exe: &str, remove: bool) -> Result<String> {
    setup_in(&base_dir()?, exe, remove)
}

fn check(exe: &str) -> HookStatus {
    let Ok(base) = base_dir() else {
        return HookStatus::NotInstalled;
    };
    let path = base.join("plugins").join("grace.ts");
    let Ok(Some(cur)) = read_opt(&path) else {
        if base.is_dir() || crate::tools::on_path("amp") {
            return HookStatus::Available(path);
        }
        return HookStatus::NotInstalled;
    };
    if !cur.starts_with(&format!("// {MARK}")) {
        return HookStatus::Conflict(path, "custom grace.ts".into());
    }
    if cur == plugin_source(exe) {
        HookStatus::Connected(path)
    } else {
        HookStatus::OutdatedPath(path)
    }
}

fn plugin_source(exe: &str) -> String {
    let js_exe = serde_json::to_string(exe).unwrap_or_else(|_| "\"grace\"".into());
    format!(
        r#"// {MARK}. Remove with: grace setup amp --remove
export const description = 'Reports the agent status (working, ready) to grace.'

const GRACE = {js_exe}

export default function (amp) {{
  let root = ''
  try {{
    const uri = amp.system && amp.system.workspaceRoot
    if (uri) root = amp.helpers.filePathFromURI(uri) || ''
  }} catch (_) {{}}

  const send = (key) => {{
    try {{
      // Spawn directly without a shell so spaces and Windows paths always work safely.
      const args = root ? [GRACE, 'session', 'hook', 'amp', key, root] : [GRACE, 'session', 'hook', 'amp', key]
      if (typeof Bun !== 'undefined' && typeof Bun.spawn === 'function') {{
        const p = Bun.spawn(args, {{ stdin: 'ignore', stdout: 'ignore', stderr: 'ignore' }})
        if (p && p.exited && typeof p.exited.catch === 'function') p.exited.catch(() => {{}})
      }}
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
                std::fs::remove_file(&path)
                    .with_context(|| format!("cannot remove {}", path.display()))?;
                Ok(format!(
                    "Removed {}\nRestart running amp sessions to pick this up.",
                    path.display()
                ))
            }
            Some(_) => Ok(format!(
                "{} is not managed by grace. Left it alone",
                path.display()
            )),
            None => Ok(format!("{} does not exist. Nothing to do.", path.display())),
        };
    }
    if let Some(t) = &current {
        if !t.starts_with(&format!("// {MARK}")) {
            anyhow::bail!(
                "{} exists and is not managed by grace. Nothing changed",
                path.display()
            );
        }
    }
    let src = plugin_source(exe);
    if current.as_deref() == Some(src.as_str()) {
        return Ok(format!(
            "{}: already set up. Nothing to do.",
            path.display()
        ));
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
    HookResult {
        status: Some(status),
        cwd,
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
        let other = d.path().join("plugins/other.ts");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, "// mine").unwrap();
        assert!(setup_in(d.path(), "/bin/grace", false)
            .unwrap()
            .contains("Wrote"));
        let p = d.path().join("plugins/grace.ts");
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.starts_with("// Managed by grace. Remove with: grace setup amp --remove\n"));
        assert!(t.contains("const GRACE = \"/bin/grace\""));
        assert!(t.contains("Bun.spawn(args"));
        assert!(setup_in(d.path(), "/bin/grace", false)
            .unwrap()
            .contains("Nothing to do"));
        assert!(setup_in(d.path(), "/a b/grace", false)
            .unwrap()
            .contains("Wrote"));
        assert!(std::fs::read_to_string(&p)
            .unwrap()
            .contains("const GRACE = \"/a b/grace\""));
        setup_in(d.path(), "/bin/grace", true).unwrap();
        assert!(!p.exists());
        assert!(other.exists());
        assert!(setup_in(d.path(), "/bin/grace", true)
            .unwrap()
            .contains("Nothing to do"));
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
