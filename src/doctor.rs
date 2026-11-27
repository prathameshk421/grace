//! `grace doctor [--fix]`: diagnose environment, shell hooks, database health,
//! CLI agent integrations, tmux hub readiness, and editor import sources.
use crate::db::Db;
use crate::hub::{self, Tmux};
use crate::integrations::HookStatus;
use crate::theme::Ansi;
use crate::{config, import_editors, model, platform, setup, tools};
use anyhow::Result;
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
    fixable_warnings: usize,
    fixed: usize,
}

fn ok_line(a: &Ansi, section: &str, msg: &str) {
    eprintln!(
        "  {g}{c}{r} {b}{section:<11}{r} {msg}",
        g = a.green,
        c = a.check,
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

fn warn_fixable(a: &Ansi, s: &mut Summary, section: &str, msg: &str, hint: Option<&str>) {
    s.fixable_warnings += 1;
    warn_line(a, s, section, msg, hint);
}

fn err_line(a: &Ansi, s: &mut Summary, section: &str, msg: &str, hint: Option<&str>) {
    s.warnings += 1;
    eprintln!(
        "  {rd}{c}{r} {b}{section:<11}{r} {msg}",
        rd = a.red,
        c = a.cross,
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
            return ShellCheck::Active(platform::sanitize_display(trimmed).into_owned());
        }
    }
    let Some(h) = home else {
        return ShellCheck::Missing;
    };
    let mut candidates = Vec::new();
    let mut push_unique = |p: PathBuf| {
        if !candidates.contains(&p) {
            candidates.push(p);
        }
    };
    if let Some(zdotdir) = platform::env_non_empty("ZDOTDIR") {
        push_unique(PathBuf::from(zdotdir).join(".zshrc"));
    }
    push_unique(h.join(".zshrc"));
    push_unique(h.join(".bashrc"));
    push_unique(h.join(".bash_profile"));
    push_unique(h.join(".profile"));
    if let Some(fish_dir) = platform::xdg_config_dir(None, "fish") {
        push_unique(fish_dir.join("config.fish"));
    }
    push_unique(h.join(".config/fish/config.fish"));
    if cfg!(windows) {
        push_unique(h.join("Documents/PowerShell/Microsoft.PowerShell_profile.ps1"));
        push_unique(h.join("Documents/WindowsPowerShell/Microsoft.PowerShell_profile.ps1"));
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

/// Validate parsed config TOML keys and value types/enums against `Config`.
fn validate_config(text: &str) -> std::result::Result<Vec<String>, toml_edit::TomlError> {
    let doc = text.parse::<toml_edit::DocumentMut>()?;
    let mut issues = Vec::new();
    let valid_layout = |v: &str| matches!(v.trim(), "full" | "code+diff" | "sidebar+main" | "zen");

    for (key, item) in doc.as_table().iter() {
        match key {
            "theme" => match item.as_str() {
                Some(v) if matches!(v.trim(), "auto" | "dark" | "light") => {}
                Some(v) => issues.push(format!("`theme` must be auto/dark/light (got `{v}`)")),
                None => issues.push("`theme` must be a string".into()),
            },
            "editor" => match item.as_str() {
                Some(v) if !v.trim().is_empty() => {}
                Some(_) => issues.push("`editor` must not be empty".into()),
                None => issues.push("`editor` must be a string".into()),
            },
            "ascii" => {
                if item.as_bool().is_none() {
                    issues.push("`ascii` must be a boolean".into());
                }
            }
            "hub_layout" => match item.as_str() {
                Some(v) if valid_layout(v) => {}
                Some(v) => issues.push(format!(
                    "`hub_layout` must be full/code+diff/sidebar+main/zen (got `{v}`)"
                )),
                None => issues.push("`hub_layout` must be a string".into()),
            },
            "default_action" => match item.as_str() {
                Some(v) if matches!(v.trim(), "travel" | "launch") => {}
                Some(v) => issues.push(format!(
                    "`default_action` must be travel/launch (got `{v}`)"
                )),
                None => issues.push("`default_action` must be a string".into()),
            },
            "cli_in_hub" => {
                if item.as_bool().is_none() {
                    issues.push("`cli_in_hub` must be a boolean".into());
                }
            }
            "ignore" => match item.as_array() {
                Some(arr) => {
                    for (idx, val) in arr.iter().enumerate() {
                        match val.as_str() {
                            Some(s) if !s.trim().is_empty() => {}
                            _ => issues.push(format!("`ignore[{idx}]` must be a non-empty string")),
                        }
                    }
                }
                None => issues.push("`ignore` must be an array of strings".into()),
            },
            "hub" => match item.as_table_like() {
                Some(tbl) => {
                    for (sub_k, sub_v) in tbl.iter() {
                        if sub_k == "layout" {
                            match sub_v.as_str() {
                                Some(v) if valid_layout(v) => {}
                                Some(v) => issues.push(format!(
                                    "`hub.layout` must be full/code+diff/sidebar+main/zen (got `{v}`)"
                                )),
                                None => issues.push("`hub.layout` must be a string".into()),
                            }
                        } else {
                            issues.push(format!("unknown key `hub.{sub_k}`"));
                        }
                    }
                }
                None => issues.push("`hub` must be a table".into()),
            },
            other => issues.push(format!("unknown key `{other}`")),
        }
    }
    Ok(issues)
}

/// Run `grace doctor [--fix]`.
pub fn run(fix: bool) -> Result<()> {
    let cfg = config::load();
    let a = Ansi::stderr(&cfg);
    let mut sum = Summary::default();
    let home_pb = platform::home_dir();
    let home_str = home_pb
        .as_ref()
        .map(|h| platform::clean_path_str(&h.to_string_lossy()));
    let home_ref = home_str.as_deref();

    eprintln!();
    eprintln!(
        "  {g}{} grace doctor{r} {d}v{}{}{r}",
        a.star,
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
    let exe_shown = platform::short_path(exe_path, home_ref);
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
                    platform::short_path(&rc, home_ref),
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
        let cfg_shown = platform::short_path(&cfg_path, home_ref);
        if cfg_path.is_file() {
            match platform::read_to_string_capped(&cfg_path, 256 * 1024) {
                Ok(txt) => match validate_config(&txt) {
                    Ok(issues) if issues.is_empty() => {
                        ok_line(
                            &a,
                            "config",
                            &format!("{cfg_shown} {d}(valid TOML){r}", d = a.dim, r = a.reset),
                        );
                    }
                    Ok(issues) => {
                        warn_line(
                            &a,
                            &mut sum,
                            "config",
                            &format!("{cfg_shown}: {}", issues.join("; ")),
                            Some("Fix invalid keys or values in your config file"),
                        );
                    }
                    Err(_) => {
                        warn_line(
                            &a,
                            &mut sum,
                            "config",
                            &format!("{cfg_shown} has invalid TOML (using defaults)"),
                            Some("Fix TOML syntax in your config file"),
                        );
                    }
                },
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
            "  {g}{} Fixed {} issue(s).{r}",
            a.check,
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
        let fix_hint = if !fix && sum.fixable_warnings > 0 {
            " Run `grace doctor --fix` to repair auto-fixable items."
        } else {
            ""
        };
        eprintln!(
            "  {y}{} item(s) need attention.{r}{fix_hint}",
            sum.warnings,
            y = a.yellow,
            r = a.reset
        );
    }
    eprintln!();
    Ok(())
}

fn check_private_file(
    a: &Ansi,
    sum: &mut Summary,
    path: &Path,
    shown: &str,
    section: &str,
    hint: &str,
    fix: bool,
) {
    let Some((mode, private)) = mode_is_private(path) else {
        return;
    };
    if private {
        return;
    }
    if fix {
        platform::restrict_file_0600(path);
        if mode_is_private(path).is_some_and(|(_, p)| p) {
            sum.fixed += 1;
            ok_line(
                a,
                section,
                &format!("{shown} permissions tightened to 0600"),
            );
        } else {
            warn_line(
                a,
                sum,
                section,
                &format!("{shown} permissions still {mode:04o}"),
                None,
            );
        }
    } else {
        warn_fixable(
            a,
            sum,
            section,
            &format!("{shown} permissions are {mode:04o} (expected 0600)"),
            Some(hint),
        );
    }
}

fn check_database(a: &Ansi, sum: &mut Summary, home_ref: Option<&str>, fix: bool) -> Result<()> {
    let dir = crate::db::data_dir()?;
    let db_path = dir.join("grace.db");
    let shown_dir = platform::short_path(&dir, home_ref);
    let shown_db = platform::short_path(&db_path, home_ref);

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
                let msg = format!("{shown_dir} permissions are {mode:04o} (expected 0700)");
                let hint = Some("Run `grace doctor --fix` or `chmod 700` on the data directory");
                if fix {
                    warn_line(a, sum, "data dir", &msg, hint);
                } else {
                    warn_fixable(a, sum, "data dir", &msg, hint);
                }
            }
        }
    }

    let lock_path = dir.join("hub.lock");
    if lock_path.is_file() {
        let shown_lock = platform::short_path(&lock_path, home_ref);
        check_private_file(
            a,
            sum,
            &lock_path,
            &shown_lock,
            "hub lock",
            "Run `grace doctor --fix` to restrict hub.lock to owner-only",
            fix,
        );
    }

    if !db_path.is_file() {
        info_line(
            a,
            "database",
            &format!("{shown_db} not initialized yet (created on first run)"),
        );
        return Ok(());
    }

    for (path, shown) in [
        (db_path.clone(), shown_db.clone()),
        (
            dir.join("grace.db-wal"),
            platform::short_path(&dir.join("grace.db-wal"), home_ref),
        ),
        (
            dir.join("grace.db-shm"),
            platform::short_path(&dir.join("grace.db-shm"), home_ref),
        ),
    ] {
        if path.is_file() {
            check_private_file(
                a,
                sum,
                &path,
                &shown,
                "db perms",
                "Run `grace doctor --fix` to restrict permissions to owner-only",
                fix,
            );
        }
    }

    let now = model::now();
    let db = if fix {
        Db::open(&db_path)?
    } else {
        let Some(db) = Db::open_readonly(&db_path)? else {
            return Ok(());
        };
        db
    };

    let (active, stale_or_dead) = db.inspect_sessions(now).unwrap_or((0, 0));
    let live_sessions = if fix {
        let mut sessions_cleaned = false;
        let result = if stale_or_dead > 0 {
            let _ = db.sessions(now);
            let (active_after, stale_after) =
                db.inspect_sessions(now).unwrap_or((active, stale_or_dead));
            let cleaned = (active + stale_or_dead).saturating_sub(active_after + stale_after);
            if cleaned > 0 {
                sum.fixed += 1;
                sessions_cleaned = true;
                ok_line(
                    a,
                    "sessions",
                    &format!("cleaned {cleaned} stale/dead session(s)"),
                );
            }
            active_after + stale_after
        } else {
            active
        };
        let wal_path = dir.join("grace.db-wal");
        let wal_big = wal_path.metadata().is_ok_and(|m| m.len() > 1_048_576);
        if (wal_big || sessions_cleaned)
            && wal_path.is_file()
            && db.wal_checkpoint_truncate().is_ok()
        {
            sum.fixed += 1;
            ok_line(a, "wal", "checkpointed and truncated WAL log");
        }
        result
    } else {
        if stale_or_dead > 0 {
            warn_fixable(
                a,
                sum,
                "sessions",
                &format!("{stale_or_dead} stale/dead session(s) still open"),
                Some("Run `grace doctor --fix` to mark them done"),
            );
        }
        active + stale_or_dead
    };

    let ok = db.integrity_check().unwrap_or(false);
    let dirs = db.list_dir_flags().unwrap_or_default();
    let total_dirs = dirs.len();
    let missing_dirs = dirs.iter().filter(|(p, _)| !Path::new(p).is_dir()).count();
    let pinned_dirs = dirs.iter().filter(|(_, pinned)| *pinned).count();
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
    let mut statuses = setup::inspect_all(exe);
    // A hook that calls another grace on PATH still works. Only a hook whose
    // grace is gone (or matches none) is outdated, so `--fix` leaves the rest.
    let mut via_other: Vec<(&str, String)> = Vec::new();
    if statuses
        .iter()
        .any(|(_, _, st)| matches!(st, HookStatus::OutdatedPath(_)))
    {
        for alt in platform::other_graces_on_path(exe) {
            let alt_statuses = setup::inspect_all(&alt);
            for (st, (name, _, alt_st)) in statuses.iter_mut().zip(alt_statuses) {
                if matches!(st.2, HookStatus::OutdatedPath(_))
                    && matches!(alt_st, HookStatus::Connected(_))
                {
                    st.2 = alt_st;
                    via_other.push((name, alt.clone()));
                }
            }
        }
    }
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

    let mut alts: Vec<&str> = via_other.iter().map(|(_, alt)| alt.as_str()).collect();
    alts.dedup();
    for alt in alts {
        let names: Vec<&str> = via_other
            .iter()
            .filter(|(_, p)| p == alt)
            .map(|(n, _)| *n)
            .collect();
        info_line(
            a,
            "agent hooks",
            &format!(
                "{} call {}, not this grace {d}(run `grace setup <cli>` to switch){r}",
                names.join(", "),
                platform::short_path(Path::new(alt), home_ref),
                d = a.dim,
                r = a.reset
            ),
        );
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
                            platform::short_path(&p, home_ref)
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
        warn_fixable(
            a,
            sum,
            "agent hook",
            &format!(
                "{name} points to an older grace binary ({})",
                platform::short_path(&p, home_ref)
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
                platform::short_path(&p, home_ref),
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
            let active = if hub::socket_exists(&t.socket) {
                t.windows().ok().map(|w| w.len())
            } else {
                None
            };
            if let Some(n) = active {
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
            platform::short_path(Path::new("/home/me/.config/grace"), Some("/home/me")),
            "~/.config/grace"
        );
        assert_eq!(
            platform::short_path(Path::new("/home/me2/proj"), Some("/home/me")),
            "/home/me2/proj"
        );
        assert_eq!(
            platform::short_path(Path::new("/tmp/x\x1b[31m"), None),
            "/tmp/x?[31m"
        );
    }

    #[test]
    fn validates_config_schema_and_values() {
        let ok_issues = validate_config(
            r#"
theme = "dark"
editor = "nvim"
ascii = false
hub_layout = "code+diff"
default_action = "launch"
cli_in_hub = true
ignore = ["/tmp/*", "~/Downloads/*"]
"#,
        )
        .unwrap();
        assert!(ok_issues.is_empty(), "{ok_issues:?}");

        let ok_table = validate_config("[hub]\nlayout = \"zen\"\n").unwrap();
        assert!(ok_table.is_empty(), "{ok_table:?}");

        let bad = validate_config(
            r#"
theme = "blue"
ascii = "yes"
default_action = "fly"
cli_in_hub = "maybe"
ignore = [""]
extra = 1
[hub]
foo = "bar"
"#,
        )
        .unwrap();
        assert_eq!(bad.len(), 7, "{bad:?}");
    }
}
