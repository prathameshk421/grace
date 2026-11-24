//! `grace doctor [--fix]`: diagnose environment, shell hooks, database health,
//! CLI agent integrations, tmux hub readiness, and editor import sources.
use crate::db::Db;
use crate::hub::{self, Tmux};
use crate::integrations::HookStatus;
use crate::{config, import_editors, model, platform, setup, tools};
use anyhow::Result;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShellCheck {
    Active(String),
    Configured(PathBuf),
    Missing,
}

#[derive(Debug, Default)]
struct Summary {
    warnings: usize,
    fixed: usize,
}

struct Ansi {
    gold: &'static str,
    green: &'static str,
    yellow: &'static str,
    red: &'static str,
    cyan: &'static str,
    dim: &'static str,
    bold: &'static str,
    reset: &'static str,
}

impl Ansi {
    fn stderr() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let clicolor_off = std::env::var("CLICOLOR").is_ok_and(|v| v == "0");
        let dumb = std::env::var("TERM").is_ok_and(|v| v == "dumb");
        if std::io::stderr().is_terminal() && !no_color && !clicolor_off && !dumb {
            Self {
                gold: "\x1b[1;33m",
                green: "\x1b[32m",
                yellow: "\x1b[33m",
                red: "\x1b[31m",
                cyan: "\x1b[36m",
                dim: "\x1b[2m",
                bold: "\x1b[1m",
                reset: "\x1b[0m",
            }
        } else {
            Self {
                gold: "",
                green: "",
                yellow: "",
                red: "",
                cyan: "",
                dim: "",
                bold: "",
                reset: "",
            }
        }
    }
}

fn short_path(p: &Path, home: Option<&str>) -> String {
    let raw = platform::clean_path_str(&p.to_string_lossy());
    let sanitized = platform::sanitize_display(&raw);
    platform::strip_home_prefix(&sanitized, home)
}

fn ok_line(a: &Ansi, section: &str, msg: &str) {
    eprintln!(
        "  {g}✓{r} {b}{section:<11}{r} {msg}",
        g = a.green,
        b = a.bold,
        r = a.reset
    );
}

fn warn_line(a: &Ansi, s: &mut Summary, section: &str, msg: &str, hint: Option<&str>) {
    s.warnings += 1;
    eprintln!(
        "  {y}!{r} {b}{section:<11}{r} {msg}",
        y = a.yellow,
        b = a.bold,
        r = a.reset
    );
    if let Some(h) = hint {
        eprintln!("    {d}→ {h}{r}", d = a.dim, r = a.reset);
    }
}

fn err_line(a: &Ansi, s: &mut Summary, section: &str, msg: &str, hint: Option<&str>) {
    s.warnings += 1;
    eprintln!(
        "  {rd}✗{r} {b}{section:<11}{r} {msg}",
        rd = a.red,
        b = a.bold,
        r = a.reset
    );
    if let Some(h) = hint {
        eprintln!("    {d}→ {h}{r}", d = a.dim, r = a.reset);
    }
}

fn info_line(a: &Ansi, section: &str, msg: &str) {
    eprintln!(
        "  {c}·{r} {b}{section:<11}{r} {msg}",
        c = a.cyan,
        b = a.bold,
        r = a.reset
    );
}

/// Inspect shell hook activation in environment and standard shell rc files.
fn check_shell(home: Option<&Path>) -> ShellCheck {
    if let Ok(sh) = std::env::var("GRACE_SHELL") {
        let trimmed = sh.trim();
        if !trimmed.is_empty() {
            return ShellCheck::Active(platform::sanitize_display(trimmed));
        }
    }
    let Some(h) = home else {
        return ShellCheck::Missing;
    };
    let mut candidates = vec![
        h.join(".zshrc"),
        h.join(".bashrc"),
        h.join(".bash_profile"),
        h.join(".config/fish/config.fish"),
    ];
    if cfg!(windows) {
        candidates.push(h.join("Documents/PowerShell/Microsoft.PowerShell_profile.ps1"));
        candidates.push(h.join("Documents/WindowsPowerShell/Microsoft.PowerShell_profile.ps1"));
    }
    if let Some(found) = find_grace_init_in_files(&candidates) {
        return ShellCheck::Configured(found);
    }
    ShellCheck::Missing
}

fn find_grace_init_in_files(paths: &[PathBuf]) -> Option<PathBuf> {
    for p in paths {
        if let Ok(text) = platform::read_to_string_capped(p, 512 * 1024) {
            if text.lines().any(|l| {
                let t = l.trim_start();
                !t.starts_with('#') && t.contains("grace init")
            }) {
                return Some(p.clone());
            }
        }
    }
    None
}

#[cfg(unix)]
fn mode_is_private(path: &Path) -> Option<(u32, bool)> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).ok()?;
    let mode = meta.permissions().mode() & 0o777;
    Some((mode, (mode & 0o077) == 0))
}

#[cfg(not(unix))]
fn mode_is_private(_path: &Path) -> Option<(u32, bool)> {
    None
}

/// Run `grace doctor [--fix]`.
pub fn run(fix: bool) -> Result<()> {
    let a = Ansi::stderr();
    let mut sum = Summary::default();
    let home_pb = platform::home_dir();
    let home_str = home_pb
        .as_ref()
        .map(|h| platform::clean_path_str(&h.to_string_lossy()));
    let home_ref = home_str.as_deref();

    eprintln!();
    eprintln!(
        "  {g}✦ grace doctor{r} {d}v{}{}{r}",
        env!("CARGO_PKG_VERSION"),
        if fix { " (--fix)" } else { "" },
        g = a.gold,
        d = a.dim,
        r = a.reset
    );
    eprintln!();

    // 1. Binary & PATH
    let exe_str = setup::exe_string()?;
    let exe_path = Path::new(&exe_str);
    let exe_shown = short_path(exe_path, home_ref);
    if tools::on_path("grace") {
        ok_line(
            &a,
            "binary",
            &format!("{exe_shown} {d}(on PATH){r}", d = a.dim, r = a.reset),
        );
    } else {
        warn_line(
            &a,
            &mut sum,
            "binary",
            &format!("{exe_shown} (not on PATH)"),
            Some("Add the folder containing `grace` to your shell PATH"),
        );
    }

    // 2. Shell hook
    match check_shell(home_pb.as_deref()) {
        ShellCheck::Active(sh) => {
            ok_line(&a, "shell", &format!("hook active in current shell ({sh})"));
        }
        ShellCheck::Configured(rc) => {
            ok_line(
                &a,
                "shell",
                &format!(
                    "configured in {} {d}(restart shell if not active yet){r}",
                    short_path(&rc, home_ref),
                    d = a.dim,
                    r = a.reset
                ),
            );
        }
        ShellCheck::Missing => {
            warn_line(
                &a,
                &mut sum,
                "shell",
                "no `grace init` hook detected in env or standard rc files",
                Some("Add `eval \"$(grace init zsh)\"` (or bash/fish/powershell) to your rc file"),
            );
        }
    }

    // 3. Optional config file
    if let Some(cfg_path) = config::config_path() {
        let cfg_shown = short_path(&cfg_path, home_ref);
        if cfg_path.is_file() {
            match platform::read_to_string_capped(&cfg_path, 256 * 1024) {
                Ok(txt) if txt.parse::<toml_edit::DocumentMut>().is_ok() => {
                    ok_line(
                        &a,
                        "config",
                        &format!("{cfg_shown} {d}(valid TOML){r}", d = a.dim, r = a.reset),
                    );
                }
                Ok(_) => {
                    warn_line(
                        &a,
                        &mut sum,
                        "config",
                        &format!("{cfg_shown} has invalid TOML (using defaults)"),
                        Some("Fix TOML syntax in your config file"),
                    );
                }
                Err(e) => {
                    warn_line(&a, &mut sum, "config", &format!("{cfg_shown}: {e}"), None);
                }
            }
        } else {
            info_line(
                &a,
                "config",
                &format!(
                    "zero-config defaults {d}(optional: {cfg_shown}){r}",
                    d = a.dim,
                    r = a.reset
                ),
            );
        }
    }

    // 4. Database health, permissions, and stats
    check_database(&a, &mut sum, home_ref, fix)?;

    // 5. CLI agent hooks
    check_agent_hooks(&a, &mut sum, &exe_str, home_ref, fix);

    // 6. Hub & tmux
    check_tmux(&a, &mut sum);

    // 7. Editor import sources
    let sources = import_editors::detected_sources();
    if sources.is_empty() {
        info_line(
            &a,
            "editors",
            "no VS Code / Cursor / Zed / Claude / T3 Code stores detected",
        );
    } else {
        let names: Vec<&str> = sources.iter().map(|(n, _)| *n).collect();
        ok_line(
            &a,
            "editors",
            &format!("{} detected ({})", sources.len(), names.join(", ")),
        );
    }

    // Footer summary
    eprintln!();
    if sum.fixed > 0 {
        eprintln!(
            "  {g}✓ Fixed {} issue(s).{r}",
            sum.fixed,
            g = a.green,
            r = a.reset
        );
    }
    if sum.warnings == 0 {
        eprintln!(
            "  {g}Everything looks good. Run `grace` to open the picker.{r}",
            g = a.green,
            r = a.reset
        );
    } else {
        eprintln!(
            "  {y}{} item(s) need attention.{r}{}",
            sum.warnings,
            if fix {
                ""
            } else {
                " Run `grace doctor --fix` to repair auto-fixable items."
            },
            y = a.yellow,
            r = a.reset
        );
    }
    eprintln!();
    Ok(())
}

fn check_database(a: &Ansi, sum: &mut Summary, home_ref: Option<&str>, fix: bool) -> Result<()> {
    let dir = crate::db::data_dir()?;
    let db_path = dir.join("grace.db");
    let shown_dir = short_path(&dir, home_ref);
    let shown_db = short_path(&db_path, home_ref);

    if let Some((mode, private)) = mode_is_private(&dir) {
        if !private {
            if fix && platform::create_dir_private(&dir).is_ok() {
                sum.fixed += 1;
                ok_line(
                    a,
                    "data dir",
                    &format!("{shown_dir} permissions tightened to 0700"),
                );
            } else {
                warn_line(
                    a,
                    sum,
                    "data dir",
                    &format!("{shown_dir} permissions are {mode:04o} (expected 0700)"),
                    Some("Run `grace doctor --fix` or `chmod 700` on the data directory"),
                );
            }
        }
    }

    let pre_db_mode = mode_is_private(&db_path);
    if !fix && !db_path.is_file() {
        info_line(
            a,
            "database",
            &format!("{shown_db} not initialized yet (created on first run)"),
        );
        return Ok(());
    }
    if let Some((mode, private)) = pre_db_mode {
        if !private {
            if fix {
                platform::restrict_file_0600(&db_path);
                if mode_is_private(&db_path).is_some_and(|(_, p)| p) {
                    sum.fixed += 1;
                    ok_line(
                        a,
                        "db perms",
                        &format!("{shown_db} permissions tightened to 0600"),
                    );
                } else {
                    warn_line(
                        a,
                        sum,
                        "db perms",
                        &format!("{shown_db} permissions still {mode:04o}"),
                        None,
                    );
                }
            } else {
                warn_line(
                    a,
                    sum,
                    "db perms",
                    &format!("{shown_db} permissions are {mode:04o} (expected 0600)"),
                    Some("Run `grace doctor --fix` to restrict permissions to owner-only"),
                );
            }
        }
    }

    let now = model::now();
    let db = if fix {
        Db::open(&db_path)?
    } else {
        let conn = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let _ = conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true);
        let _ = conn.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA,
            false,
        );
        Db { conn }
    };

    let live_before: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE status != 'done'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if fix {
        let sessions_after = db.sessions(now).unwrap_or_default();
        let live_after = sessions_after
            .iter()
            .filter(|s| s.status != model::Status::Done)
            .count() as i64;
        if live_before > live_after {
            let cleaned = live_before - live_after;
            sum.fixed += 1;
            ok_line(
                a,
                "sessions",
                &format!("cleaned {cleaned} stale/dead session(s)"),
            );
        }
    }

    let ok = db.integrity_check().unwrap_or(false);
    let entries = db.list_entries().unwrap_or_default();
    let total_dirs = entries.len();
    let missing_dirs = entries
        .iter()
        .filter(|e| !Path::new(&e.path).is_dir())
        .count();
    let pinned_dirs = entries.iter().filter(|e| e.pinned).count();
    let live_sessions: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE status != 'done'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if ok {
        ok_line(
            a,
            "database",
            &format!(
                "{shown_db} · OK · {total_dirs} folders ({pinned_dirs} pinned), {live_sessions} live session(s)"
            ),
        );
    } else {
        err_line(
            a,
            sum,
            "database",
            &format!("{shown_db} failed SQLite integrity check"),
            Some("Back up your DB before running `sqlite3` recovery"),
        );
    }

    if missing_dirs > 0 {
        warn_line(
            a,
            sum,
            "folders",
            &format!("{missing_dirs} tracked folder(s) no longer exist on disk"),
            Some("Run `grace prune` (or `grace prune --dry-run`) to clean stale folders"),
        );
    }
    Ok(())
}

fn check_agent_hooks(a: &Ansi, sum: &mut Summary, exe: &str, home_ref: Option<&str>, fix: bool) {
    let statuses = setup::inspect_all(exe);
    let mut connected = Vec::new();
    let mut available = Vec::new();
    let mut outdated = Vec::new();
    let mut conflicts = Vec::new();

    for (name, _label, st) in statuses {
        match st {
            HookStatus::Connected(_) => connected.push(name),
            HookStatus::Available(p) => available.push((name, p)),
            HookStatus::OutdatedPath(p) => outdated.push((name, p)),
            HookStatus::Conflict(p, why) => conflicts.push((name, p, why)),
            HookStatus::NotInstalled => {}
        }
    }

    if !connected.is_empty() {
        ok_line(
            a,
            "agents",
            &format!("{} connected ({})", connected.len(), connected.join(", ")),
        );
    } else if available.is_empty() && outdated.is_empty() && conflicts.is_empty() {
        info_line(a, "agents", "no supported CLI agents detected yet");
    }

    for (name, p) in outdated {
        if fix {
            match setup::run(Some(name), false, false) {
                Ok(()) => {
                    sum.fixed += 1;
                    ok_line(
                        a,
                        "agent hook",
                        &format!(
                            "{name} hook updated to current grace binary ({})",
                            short_path(&p, home_ref)
                        ),
                    );
                    continue;
                }
                Err(e) => {
                    warn_line(
                        a,
                        sum,
                        "agent hook",
                        &format!("{name}: failed to update ({e})"),
                        None,
                    );
                    continue;
                }
            }
        }
        warn_line(
            a,
            sum,
            "agent hook",
            &format!(
                "{name} points to an older grace binary ({})",
                short_path(&p, home_ref)
            ),
            Some(&format!("Run `grace setup {name}` or `grace doctor --fix`")),
        );
    }

    if !available.is_empty() {
        let names: Vec<&str> = available.iter().map(|(n, _)| *n).collect();
        info_line(
            a,
            "available",
            &format!(
                "{} detected without hooks: {} {d}(run `grace setup --all`){r}",
                names.len(),
                names.join(", "),
                d = a.dim,
                r = a.reset
            ),
        );
    }

    for (name, p, why) in conflicts {
        warn_line(
            a,
            sum,
            "agent hook",
            &format!(
                "{name} in {}: {}",
                short_path(&p, home_ref),
                platform::sanitize_display(&why)
            ),
            None,
        );
    }
}

fn check_tmux(a: &Ansi, sum: &mut Summary) {
    if cfg!(windows) {
        info_line(
            a,
            "hub",
            "tmux not native on Windows (run `grace hub` inside WSL)",
        );
        return;
    }
    match hub::tmux_version() {
        Ok((maj, min)) => {
            let t = Tmux::new();
            let running = t.ok(&["has-session", "-t", &format!("={}", hub::SESSION)]);
            if running {
                let n = t.windows().map(|w| w.len()).unwrap_or(0);
                ok_line(
                    a,
                    "hub",
                    &format!("tmux {maj}.{min} · active hub session ({n} workspace(s))"),
                );
            } else {
                ok_line(a, "hub", &format!("tmux {maj}.{min} · ready"));
            }
        }
        Err(e) => {
            let first = e.to_string();
            let summary = first.lines().next().unwrap_or("tmux unavailable");
            warn_line(
                a,
                sum,
                "hub",
                &platform::sanitize_display(summary),
                Some("Install tmux 3.2+ for `grace hub` (the standalone picker still works)"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_grace_init_ignoring_comments() {
        let dir = tempfile::tempdir().unwrap();
        let rc_commented = dir.path().join(".bashrc");
        std::fs::write(
            &rc_commented,
            "# eval \"$(grace init bash)\"\nexport FOO=1\n",
        )
        .unwrap();
        let rc_active = dir.path().join(".zshrc");
        std::fs::write(&rc_active, "eval \"$(grace init zsh)\"\n").unwrap();

        assert_eq!(
            find_grace_init_in_files(std::slice::from_ref(&rc_commented)),
            None
        );
        assert_eq!(
            find_grace_init_in_files(&[rc_commented, rc_active.clone()]),
            Some(rc_active)
        );
    }

    #[cfg(unix)]
    #[test]
    fn detects_private_and_open_unix_modes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("test.db");
        std::fs::write(&f, "x").unwrap();

        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        let (mode, private) = mode_is_private(&f).unwrap();
        assert_eq!(mode, 0o644);
        assert!(!private);

        platform::restrict_file_0600(&f);
        let (mode2, private2) = mode_is_private(&f).unwrap();
        assert_eq!(mode2, 0o600);
        assert!(private2);
    }

    #[test]
    fn short_path_masks_home_and_sanitizes() {
        assert_eq!(
            short_path(Path::new("/home/me/.config/grace"), Some("/home/me")),
            "~/.config/grace"
        );
        assert_eq!(
            short_path(Path::new("/home/me2/proj"), Some("/home/me")),
            "/home/me2/proj"
        );
        assert_eq!(short_path(Path::new("/tmp/x\x1b[31m"), None), "/tmp/x?[31m");
    }
}
