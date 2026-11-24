//! Import recent folders from editors. All reads are read-only.
use crate::db::{open_readonly_uri, Db};
use crate::editors_live::APPS;
use crate::model;
use crate::platform::{self, IgnoreMatcher};
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAX_WORKSPACE_JSON_BYTES: usize = 2 << 20;

/// Valid values for `grace import [source]`.
pub const SOURCES: &[&str] = &[
    "all",
    "claude",
    "vscode",
    "cursor",
    "windsurf",
    "antigravity",
    "zed",
    "t3code",
];

/// The sources that `all` runs, in order.
const ALL: &[&str] = &[
    "claude",
    "vscode",
    "cursor",
    "windsurf",
    "antigravity",
    "zed",
    "t3code",
];

/// Flatpak app IDs for VS Code family on Linux `(source, app_id, dir_name, tag)`.
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) const FLATPAK_APPS: &[(&str, &str, &str, &str)] = &[
    ("vscode", "com.visualstudio.code", "Code", "vscode"),
    (
        "vscode",
        "com.visualstudio.code-oss",
        "Code - OSS",
        "vscode",
    ),
    ("vscode", "com.vscodium.codium", "VSCodium", "vscodium"),
];

/// Import one source. `None` means the source was not found.
pub fn import_source(db: &Db, source: &str, ignore: &IgnoreMatcher) -> Option<usize> {
    match source {
        "claude" => crate::import::import_claude(db, ignore).ok().flatten(),
        "zed" => import_zed(db, ignore),
        "t3code" => import_t3code(db, ignore),
        s => import_vscode_family(db, s, ignore),
    }
}

/// Import all editor sources (not Claude history). Prints nothing. Returns newly added folder count.
pub fn import_editors_quiet(db: &Db, ignore: &IgnoreMatcher) -> usize {
    ALL.iter()
        .filter(|s| **s != "claude")
        .filter_map(|s| import_source(db, s, ignore))
        .sum()
}

/// Import every source. Returns `(source, newly_added_count)` for sources that exist.
pub fn import_all(db: &Db, ignore: &IgnoreMatcher) -> Vec<(&'static str, usize)> {
    ALL.iter()
        .filter_map(|s| import_source(db, s, ignore).map(|n| (*s, n)))
        .collect()
}

/// List configured import source paths that exist on this machine (for `grace doctor`).
pub fn detected_sources() -> Vec<(&'static str, PathBuf)> {
    let mut out = Vec::new();
    if let Some(h) = platform::home_dir() {
        let p = h.join(".claude.json");
        if p.is_file() {
            out.push(("claude", p));
        }
    }
    let root = config_root();
    for src in ["vscode", "cursor", "windsurf", "antigravity"] {
        let mut found = None;
        if let Some(r) = &root {
            for a in APPS.iter().filter(|a| a.source == src) {
                let p = r.join(a.dir);
                if p.is_dir() {
                    found = Some(p);
                    break;
                }
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        if found.is_none() {
            if let Some(home) = platform::home_dir() {
                for &(s, app_id, dir_name, _) in FLATPAK_APPS {
                    if s == src {
                        let p = home
                            .join(".var/app")
                            .join(app_id)
                            .join("config")
                            .join(dir_name);
                        if p.is_dir() {
                            found = Some(p);
                            break;
                        }
                    }
                }
            }
        }
        if let Some(p) = found {
            out.push((src, p));
        }
    }
    if let Some(p) = zed_db_path() {
        if p.is_file() {
            out.push(("zed", p));
        }
    }
    if let Some(p) = t3code_db_path() {
        if p.is_file() {
            out.push(("t3code", p));
        }
    }
    out
}

// ---------- locations ----------

/// Folder that holds the app data of editors (`Application Support`, `%APPDATA%`, `~/.config`).
pub(crate) fn config_root() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        platform::home_dir().map(|h| h.join("Library").join("Application Support"))
    } else if cfg!(windows) {
        platform::env_non_empty("APPDATA").map(PathBuf::from)
    } else {
        platform::env_non_empty("XDG_CONFIG_HOME")
            .filter(|v| Path::new(v).is_absolute())
            .map(PathBuf::from)
            .or_else(|| platform::home_dir().map(|h| h.join(".config")))
    }
}

fn zed_db_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        platform::home_dir()?.join("Library/Application Support/Zed")
    } else if cfg!(windows) {
        PathBuf::from(platform::env_non_empty("LOCALAPPDATA")?).join("Zed")
    } else {
        platform::env_non_empty("XDG_DATA_HOME")
            .filter(|v| Path::new(v).is_absolute())
            .map(PathBuf::from)
            .or_else(|| platform::home_dir().map(|h| h.join(".local/share")))?
            .join("zed")
    };
    let db_dir = base.join("db");
    for channel in ["0-stable", "0-preview", "0-nightly"] {
        let p = db_dir.join(channel).join("db.sqlite");
        if p.is_file() {
            return Some(p);
        }
    }
    Some(db_dir.join("0-stable").join("db.sqlite"))
}

fn mtime(p: &Path) -> Option<i64> {
    std::fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

// ---------- pure helpers ----------

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// Decode `%XX` escapes. Returns `None` if the result is not valid UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && hex(b[i + 1]).is_some() && hex(b[i + 2]).is_some() {
            out.push(hex(b[i + 1])? * 16 + hex(b[i + 2])?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Turn a `file://` URI into a path. `windows` turns `/c:/Users/x` into `c:\Users\x`.
/// Returns `None` for other schemes and for URIs with a host name.
pub fn uri_to_path(uri: &str, windows: bool) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    if !rest.starts_with('/') {
        return None;
    }
    let mut p = percent_decode(rest)?;
    if windows {
        let b = p.as_bytes();
        if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
            p = p[1..].to_string();
        }
        p = p.replace('/', "\\");
    }
    let p = platform::clean_path_str(&p);
    (!p.is_empty()).then_some(p)
}

fn file_uri_to_path(uri: &str) -> Option<String> {
    uri_to_path(uri, cfg!(windows))
}

/// Extract parent folder from a `.code-workspace` file path or `file://` URI.
pub(crate) fn workspace_dir_from_cfg(cfg: &str, windows: bool) -> Option<String> {
    let file = if cfg.starts_with("file:") {
        uri_to_path(cfg, windows)?
    } else if cfg.contains("://") {
        return None;
    } else {
        platform::clean_path_str(cfg)
    };
    let (dir, name) = file.rsplit_once(['/', '\\'])?;
    if !name.ends_with(".code-workspace") || dir.is_empty() {
        return None;
    }
    Some(platform::clean_path_str(dir))
}

/// Folder paths decoded from `history.recentlyOpenedPathsList`, including `.code-workspace` files.
pub fn parse_recent_folders(json: &str, windows: bool) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(arr) = v.get("entries").and_then(|e| e.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in arr {
        if let Some(u) = entry.get("folderUri").and_then(|u| u.as_str()) {
            if let Some(p) = uri_to_path(u, windows) {
                out.push(p);
            }
            continue;
        }
        if let Some(cfg) = entry
            .get("workspace")
            .and_then(|w| w.get("configPath"))
            .and_then(|c| c.as_str())
        {
            if let Some(dir) = workspace_dir_from_cfg(cfg, windows) {
                out.push(dir);
            }
        }
    }
    out
}

/// Days since 1970-01-01 for a civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse "YYYY-MM-DD HH:MM:SS" or ISO-8601 timestamp (including `+HH:MM` / `-HH:MM` offsets) into Unix seconds.
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time_raw) = s.split_once([' ', 'T'])?;
    let mut d = date.split('-');
    let (y, mo, da) = (d.next()?, d.next()?, d.next()?);
    if d.next().is_some() {
        return None;
    }
    let (time_no_tz, tz_offset_secs) = split_tz_offset(time_raw.trim());
    let time = time_no_tz.split('.').next()?;
    let mut t = time.split(':');
    let (h, mi, se) = (t.next()?, t.next()?, t.next()?);
    let n = |x: &str| x.parse::<i64>().ok();
    let (y, mo, da, h, mi, se) = (n(y)?, n(mo)?, n(da)?, n(h)?, n(mi)?, n(se)?);
    if !(1970..=9999).contains(&y)
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&da)
        || !(0..=23).contains(&h)
        || !(0..=59).contains(&mi)
        || !(0..=60).contains(&se)
    {
        return None;
    }
    let utc = days_from_civil(y, mo, da)
        .saturating_mul(86400)
        .saturating_add(h * 3600 + mi * 60 + se);
    Some(utc.saturating_sub(tz_offset_secs))
}

fn split_tz_offset(s: &str) -> (&str, i64) {
    if let Some(base) = s.strip_suffix('Z') {
        return (base, 0);
    }
    if let Some(pos) = s.rfind(['+', '-']) {
        if pos >= 8 {
            let sign = if s.as_bytes()[pos] == b'-' { -1 } else { 1 };
            let off = &s[pos + 1..];
            if let Some((hh, mm)) = off.split_once(':') {
                if let (Ok(h), Ok(m)) = (hh.parse::<i64>(), mm.parse::<i64>()) {
                    if (0..=23).contains(&h) && (0..=59).contains(&m) {
                        return (&s[..pos], sign * (h * 3600 + m * 60));
                    }
                }
            } else if off.is_ascii() && off.len() == 4 {
                if let (Ok(h), Ok(m)) = (off[..2].parse::<i64>(), off[2..].parse::<i64>()) {
                    if (0..=23).contains(&h) && (0..=59).contains(&m) {
                        return (&s[..pos], sign * (h * 3600 + m * 60));
                    }
                }
            }
        }
    }
    (s, 0)
}

/// Build a `file:` URI for SQLite from a path. Special characters are percent-encoded.
pub fn sqlite_uri(path: &str, query: &str) -> String {
    let p = platform::strip_verbatim(path).replace('\\', "/");
    let mut out = String::from("file:");
    if !p.starts_with('/') {
        out.push('/');
    }
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~' | b':') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out.push('?');
    out.push_str(query);
    out
}

// ---------- readers ----------

fn recent_from_state_db(db_path: &Path) -> Vec<(String, i64)> {
    let Some(conn) = open_readonly_uri(db_path, 200) else {
        return Vec::new();
    };
    let value: Option<rusqlite::types::Value> = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key = 'history.recentlyOpenedPathsList'",
            [],
            |r| r.get(0),
        )
        .ok();
    let json = match value {
        Some(rusqlite::types::Value::Text(t)) => t,
        Some(rusqlite::types::Value::Blob(b)) => String::from_utf8(b)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
        _ => return Vec::new(),
    };
    let base = mtime(db_path).unwrap_or_else(model::now);
    parse_recent_folders(&json, cfg!(windows))
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p, base.saturating_sub(i as i64)))
        .collect()
}

fn recent_from_workspace_storage(dir: &Path) -> Vec<(String, i64)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        if !e.file_type().is_ok_and(|ft| ft.is_dir()) {
            continue;
        }
        let sub = e.path();
        let Ok(text) =
            platform::read_to_string_capped(&sub.join("workspace.json"), MAX_WORKSPACE_JSON_BYTES)
        else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(path) = v
            .get("folder")
            .and_then(|f| f.as_str())
            .and_then(file_uri_to_path)
        else {
            continue;
        };
        out.push((path, mtime(&sub).unwrap_or_else(model::now)));
    }
    out
}

/// Add folders to the database using canonicalized paths. Returns how many new entries were inserted.
pub(crate) fn store_with_mtime(
    db: &Db,
    found: Vec<(String, i64)>,
    tag: &str,
    seen: &mut HashSet<String>,
    ignore: &IgnoreMatcher,
    use_dir_mtime: bool,
) -> usize {
    let mut raw_seen = HashSet::new();
    let mut items = Vec::new();
    for (raw, fallback_ts) in found {
        if raw.is_empty() || platform::has_line_break_or_nul(&raw) || !raw_seen.insert(raw.clone())
        {
            continue;
        }
        let dir = Path::new(&raw);
        let Ok(meta) = std::fs::metadata(dir) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let path = platform::normalize(dir);
        if ignore.is_ignored(&path) || !seen.insert(path.clone()) {
            continue;
        }
        let ts = if use_dir_mtime {
            meta.modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(fallback_ts)
        } else {
            fallback_ts
        };
        items.push((path, ts));
    }
    db.import_dirs_batch(&items, Some(tag)).unwrap_or(0)
}

fn import_vscode_family(db: &Db, source: &str, ignore: &IgnoreMatcher) -> Option<usize> {
    let mut roots: Vec<(PathBuf, &str)> = Vec::new();
    if let Some(root) = config_root() {
        for a in APPS.iter().filter(|a| a.source == source) {
            let p = root.join(a.dir);
            if p.is_dir() {
                roots.push((p, a.tag));
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(home) = platform::home_dir() {
        for &(s, app_id, dir_name, tag) in FLATPAK_APPS {
            if s != source {
                continue;
            }
            let p = home
                .join(".var/app")
                .join(app_id)
                .join("config")
                .join(dir_name);
            if p.is_dir() {
                roots.push((p, tag));
            }
        }
    }
    if roots.is_empty() {
        return None;
    }
    let mut seen = HashSet::new();
    let mut count = 0;
    for (app_root, tag) in roots {
        let mut all = recent_from_state_db(&app_root.join("User/globalStorage/state.vscdb"));
        all.extend(recent_from_workspace_storage(
            &app_root.join("User/workspaceStorage"),
        ));
        count += store_with_mtime(db, all, tag, &mut seen, ignore, false);
    }
    Some(count)
}

fn zed_rows(conn: &Connection) -> Vec<(String, Option<String>)> {
    let queries = [
        "SELECT paths, timestamp FROM workspaces WHERE remote_connection_id IS NULL",
        "SELECT paths, timestamp FROM workspaces",
    ];
    for q in queries {
        let Ok(mut stmt) = conn.prepare(q) else {
            continue;
        };
        let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        }) else {
            continue;
        };
        return rows
            .flatten()
            .filter_map(|(p, t)| p.map(|p| (p, t)))
            .collect();
    }
    Vec::new()
}

fn import_zed(db: &Db, ignore: &IgnoreMatcher) -> Option<usize> {
    let path = zed_db_path().filter(|p| p.is_file())?;
    let Some(conn) = open_readonly_uri(&path, 200) else {
        return Some(0);
    };
    let now = model::now();
    let mut found = Vec::new();
    for (paths, ts) in zed_rows(&conn) {
        let ts = ts.as_deref().and_then(parse_timestamp).unwrap_or(now);
        for p in paths.split('\n').map(str::trim).filter(|p| !p.is_empty()) {
            found.push((platform::clean_path_str(p), ts));
        }
    }
    Some(store_with_mtime(
        db,
        found,
        "zed",
        &mut HashSet::new(),
        ignore,
        false,
    ))
}

fn t3code_db_path() -> Option<PathBuf> {
    Some(
        platform::home_dir()?
            .join(".t3")
            .join("userdata")
            .join("statev2.sqlite"),
    )
}

/// Live projects `(workspace_root, updated_at)`. Archived projects are kept.
fn t3code_rows(conn: &Connection) -> Vec<(String, Option<String>)> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT workspace_root, updated_at FROM projection_projects WHERE deleted_at IS NULL",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
        ))
    }) else {
        return Vec::new();
    };
    rows.flatten()
        .filter_map(|(p, t)| p.map(|p| (p, t)))
        .collect()
}

fn import_t3code(db: &Db, ignore: &IgnoreMatcher) -> Option<usize> {
    let path = t3code_db_path().filter(|p| p.is_file())?;
    let Some(conn) = open_readonly_uri(&path, 200) else {
        return Some(0);
    };
    let now = model::now();
    let found = t3code_rows(&conn)
        .into_iter()
        .map(|(p, t)| {
            let ts = t.as_deref().and_then(parse_timestamp).unwrap_or(now);
            (platform::clean_path_str(p.trim()), ts)
        })
        .collect();
    Some(store_with_mtime(
        db,
        found,
        "t3code",
        &mut HashSet::new(),
        ignore,
        false,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_uris() {
        assert_eq!(
            uri_to_path("file:///Users/me/My%20Project", false).unwrap(),
            "/Users/me/My Project"
        );
        assert_eq!(uri_to_path("file:///a/b%3Ac/", false).unwrap(), "/a/b:c");
        assert_eq!(
            uri_to_path("file:///a/%C3%A9%E2%82%AC", false).unwrap(),
            "/a/\u{e9}\u{20ac}"
        );
        assert_eq!(
            uri_to_path("file:///c%3A/Users/x", true).unwrap(),
            r"c:\Users\x"
        );
        assert_eq!(
            uri_to_path("file:///c:/Users/My%20App", true).unwrap(),
            r"c:\Users\My App"
        );
        assert_eq!(uri_to_path("vscode-remote://ssh/x", false), None);
        assert_eq!(uri_to_path("file://server/share", false), None);
        assert_eq!(uri_to_path("file:///bad%FF", false), None);
        assert_eq!(uri_to_path("file:///100%", false).unwrap(), "/100%");
    }

    #[test]
    fn parses_timestamps() {
        assert_eq!(parse_timestamp("1970-01-01 00:00:00"), Some(0));
        assert_eq!(parse_timestamp("2024-03-05 12:30:45"), Some(1709641845));
        assert_eq!(parse_timestamp("2000-02-29 00:00:01"), Some(951782401));
        assert_eq!(parse_timestamp("garbage"), None);
        assert_eq!(parse_timestamp("2024-13-01 00:00:00"), None);
        assert_eq!(
            parse_timestamp("2024-03-05T14:30:45+02:00"),
            Some(1709641845)
        );
        assert_eq!(
            parse_timestamp("2024-03-05T07:30:45-05:00"),
            Some(1709641845)
        );
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_timestamp("2024-03-05T12:30:45.960Z"),
            Some(1709641845)
        );
        assert_eq!(parse_timestamp("nope"), None);
    }

    #[test]
    fn parses_vscode_entries() {
        let j = r#"{"entries":[
            {"folderUri":"file:///a/b"},
            {"fileUri":"file:///a/f.txt"},
            {"workspace":{"id":"x","configPath":"file:///ws/w.code-workspace"}},
            {"folderUri":"vscode-remote://ssh-remote%2Bhost/home/x"},
            {"folderUri":"file:///c/d%20e"}]}"#;
        assert_eq!(
            parse_recent_folders(j, false),
            vec!["/a/b", "/ws", "/c/d e"]
        );
        assert!(parse_recent_folders("nope", false).is_empty());
        assert!(parse_recent_folders("{}", false).is_empty());
    }

    #[test]
    fn builds_sqlite_uri() {
        assert_eq!(
            sqlite_uri(
                "/Users/me/Library/Application Support/Code/a?b#c.db",
                "mode=ro"
            ),
            "file:/Users/me/Library/Application%20Support/Code/a%3Fb%23c.db?mode=ro"
        );
        assert_eq!(
            sqlite_uri(r"\\?\C:\Users\x\s.db", "mode=ro"),
            "file:/C:/Users/x/s.db?mode=ro"
        );
    }

    /// Build a file URI the way VS Code writes it: file:///C:/a%20b on Windows, file:///a%20b on Unix.
    fn file_uri(path: &str) -> String {
        let p = path.replace('\\', "/").replace(' ', "%20");
        if p.starts_with('/') {
            format!("file://{p}")
        } else {
            format!("file:///{p}")
        }
    }

    #[test]
    fn reads_vscode_state_db() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("my proj");
        std::fs::create_dir(&proj).unwrap();
        let dbp = d.path().join("state.vscdb");
        let c = Connection::open(&dbp).unwrap();
        c.execute("CREATE TABLE ItemTable (key TEXT UNIQUE, value BLOB)", [])
            .unwrap();
        let json = format!(
            r#"{{"entries":[{{"folderUri":"{}"}}]}}"#,
            file_uri(&proj.to_string_lossy())
        );
        c.execute(
            "INSERT INTO ItemTable VALUES ('history.recentlyOpenedPathsList', ?1)",
            [json],
        )
        .unwrap();
        drop(c);
        let got = recent_from_state_db(&dbp);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, proj.to_string_lossy());
    }

    #[test]
    fn reads_t3code_projects() {
        let d = tempfile::tempdir().unwrap();
        let dbp = d.path().join("statev2.sqlite");
        let c = Connection::open(&dbp).unwrap();
        c.execute(
            "CREATE TABLE projection_projects (workspace_root TEXT, updated_at TEXT, deleted_at TEXT, archived_at TEXT)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO projection_projects VALUES ('/a', '2026-10-22T18:18:34.960Z', NULL, NULL)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO projection_projects VALUES ('/b', '2026-10-22T18:18:34.960Z', 'x', NULL)",
            [],
        )
        .unwrap();
        drop(c);
        let conn = open_readonly_uri(&dbp, 200).unwrap();
        let rows = t3code_rows(&conn);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "/a");
    }
}
