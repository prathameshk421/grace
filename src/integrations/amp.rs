//! amp: a plugin file we own, `<config>/plugins/grace.ts`.
use super::{
    managed_file_check, remove_owned_backup, simple_arg_hook, HookResult, HookStatus, Integration,
};
use crate::model::Status;
use crate::platform;
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
    platform::xdg_config_dir(Some("GRACE_AMP_DIR"), "amp")
        .ok_or_else(|| anyhow!("cannot find the home folder"))
}

fn setup(exe: &str, remove: bool) -> Result<String> {
    setup_in(&base_dir()?, exe, remove)
}

fn is_ours_text(t: &str) -> bool {
    t.starts_with(&format!("// {MARK}"))
}

fn check(exe: &str) -> HookStatus {
    let Ok(base) = base_dir() else {
        return HookStatus::NotInstalled;
    };
    let path = base.join("plugins").join("grace.ts");
    managed_file_check(
        path,
        Some(&base),
        "amp",
        "custom grace.ts",
        is_ours_text,
        |cur| cur == plugin_source(exe),
    )
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
            Some(t) if is_ours_text(&t) => {
                std::fs::remove_file(&path)
                    .with_context(|| format!("cannot remove {}", path.display()))?;
                remove_owned_backup(&path, MARK);
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
        if !is_ours_text(t) {
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
    simple_arg_hook(args, |s| match s {
        "working" => Some(Status::Working),
        "ready" => Some(Status::Ready),
        _ => None,
    })
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
