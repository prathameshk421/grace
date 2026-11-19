//! Import recent folders from editors. All reads are read-only.
use crate::db::Db;
use crate::model;
use crate::platform;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Valid values for `grace import [source]`.
pub const SOURCES: &[&str] = &["all", "claude", "vscode", "cursor", "windsurf", "antigravity", "zed", "t3code"];

/// The sources that `all` runs, in order.
const ALL: &[&str] = &["claude", "vscode", "cursor", "windsurf", "antigravity", "zed", "t3code"];

/// (app folder name, tag) for each source of the VS Code family.
fn vscode_apps(source: &str) -> &'static [(&'static str, &'static str)] {
    match source {
        "vscode" => &[("Code", "vscode"), ("Code - Insiders", "vscode"), ("VSCodium", "vscodium")],
        "cursor" => &[("Cursor", "cursor")],
        "windsurf" => &[("Windsurf", "windsurf")],
        "antigravity" => &[("Antigravity", "antigravity"), ("Antigravity IDE", "antigravity")],
        _ => &[],
    }
}

/// Import one source. `None` means the source was not found.
pub fn import_source(db: &Db, source: &str) -> Option<usize> {
    match source {
        "claude" => crate::import::import_claude(db).ok().flatten(),
        "zed" => import_zed(db),
        "t3code" => import_t3code(db),
        s => import_vscode_family(db, s),
    }
}

/// Import all editor sources (not Claude history). Prints nothing. Returns the folder count.
pub fn import_editors_quiet(db: &Db) -> usize {
    ALL.iter()
        .filter(|s| **s != "claude")
        .filter_map(|s| import_source(db, s))
        .sum()
}

/// Import every source. Returns `(source, count)` for sources that exist.
pub fn import_all(db: &Db) -> Vec<(&'static str, usize)> {
    ALL.iter()
        .filter_map(|s| import_source(db, s).map(|n| (*s, n)))
        .collect()
}

// ---------- locations ----------

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var(key).ok().filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Folder that holds the app data of editors (`Application Support`, `%APPDATA%`, `~/.config`).
pub(crate) fn config_root() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        platform::home_dir().map(|h| h.join("Library").join("Application Support"))
    } else if cfg!(windows) {
        env_dir("APPDATA")
    } else {
        env_dir("XDG_CONFIG_HOME").or_else(|| platform::home_dir().map(|h| h.join(".config")))
    }
}

fn zed_db_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        platform::home_dir()?.join("Library/Application Support/Zed")
    } else if cfg!(windows) {
        env_dir("LOCALAPPDATA")?.join("Zed")
    } else {
        env_dir("XDG_DATA_HOME")
            .or_else(|| platform::home_dir().map(|h| h.join(".local/share")))?
            .join("zed")
    };
    Some(base.join("db").join("0-stable").join("db.sqlite"))
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

/// The `folderUri` values with the `file:` scheme, in order, from the value of
/// `history.recentlyOpenedPathsList`.
pub fn parse_recent_folder_uris(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    v.get("entries")
        .and_then(|e| e.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| e.get("folderUri").and_then(|u| u.as_str()))
                .filter(|u| u.starts_with("file:"))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
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

/// Parse "YYYY-MM-DD HH:MM:SS" (UTC) into Unix seconds.
pub fn parse_zed_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once([' ', 'T'])?;
    let mut d = date.split('-');
    let (y, mo, da) = (d.next()?, d.next()?, d.next()?);
    let time = time.split(['.', '+', 'Z']).next()?;
    let mut t = time.split(':');
    let (h, mi, se) = (t.next()?, t.next()?, t.next()?);
    let n = |x: &str| x.parse::<i64>().ok();
    let (y, mo, da, h, mi, se) = (n(y)?, n(mo)?, n(da)?, n(h)?, n(mi)?, n(se)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    Some(days_from_civil(y, mo, da) * 86400 + h * 3600 + mi * 60 + se)
}

/// Parse "YYYY-MM-DDTHH:MM:SS(.fff)?Z" (UTC) into Unix seconds.
pub fn parse_iso_timestamp(s: &str) -> Option<i64> {
    parse_zed_timestamp(s)
}

/// Build a `file:` URI for SQLite from a path. Special characters are percent-encoded.
pub fn sqlite_uri(path: &str, query: &str) -> String {
    let p = path.replace('\\', "/");
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

fn open_ro(path: &Path) -> Option<Connection> {
    let uri = sqlite_uri(&path.to_string_lossy(), "immutable=1");
    Connection::open_with_flags(uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI).ok()
}

fn recent_from_state_db(db_path: &Path) -> Vec<(String, i64)> {
    let Some(conn) = open_ro(db_path) else {
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
        Some(rusqlite::types::Value::Blob(b)) => String::from_utf8_lossy(&b).into_owned(),
        _ => return Vec::new(),
    };
    let base = mtime(db_path).unwrap_or_else(model::now);
    parse_recent_folder_uris(&json)
        .iter()
        .enumerate()
        .filter_map(|(i, u)| file_uri_to_path(u).map(|p| (p, base - i as i64)))
        .collect()
}

fn recent_from_workspace_storage(dir: &Path) -> Vec<(String, i64)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let sub = e.path();
        let Ok(text) = std::fs::read_to_string(sub.join("workspace.json")) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(path) = v.get("folder").and_then(|f| f.as_str()).and_then(file_uri_to_path) else {
            continue;
        };
        out.push((path, mtime(&sub).unwrap_or_else(model::now)));
    }
    out
}

/// Add folders to the database. Returns how many different folders were valid.
fn store(db: &Db, found: Vec<(String, i64)>, tag: &str, seen: &mut HashSet<String>) -> usize {
    let mut n = 0;
    for (path, ts) in found {
        if platform::ignored(&path) || !Path::new(&path).is_dir() || !seen.insert(path.clone()) {
            continue;
        }
        if db.import_dir(&path, ts, Some(tag)).is_ok() {
            n += 1;
        }
    }
    n
}

fn import_vscode_family(db: &Db, source: &str) -> Option<usize> {
    let root = config_root()?;
    let mut found_any = false;
    let mut seen = HashSet::new();
    let mut count = 0;
    for (app, tag) in vscode_apps(source) {
        let app_root = root.join(app);
        if !app_root.is_dir() {
            continue;
        }
        found_any = true;
        let mut all = recent_from_state_db(&app_root.join("User/globalStorage/state.vscdb"));
        all.extend(recent_from_workspace_storage(&app_root.join("User/workspaceStorage")));
        count += store(db, all, tag, &mut seen);
    }
    found_any.then_some(count)
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
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?))
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

fn import_zed(db: &Db) -> Option<usize> {
    let path = zed_db_path().filter(|p| p.is_file())?;
    let Some(conn) = open_ro(&path) else {
        return Some(0);
    };
    let now = model::now();
    let mut found = Vec::new();
    for (paths, ts) in zed_rows(&conn) {
        let ts = ts.as_deref().and_then(parse_zed_timestamp).unwrap_or(now);
        for p in paths.split('\n').map(str::trim).filter(|p| !p.is_empty()) {
            found.push((platform::clean_path_str(p), ts));
        }
    }
    Some(store(db, found, "zed", &mut HashSet::new()))
}

fn t3code_db_path() -> Option<PathBuf> {
    Some(platform::home_dir()?.join(".t3").join("userdata").join("statev2.sqlite"))
}

/// Live projects `(workspace_root, updated_at)`. Archived projects are kept.
fn t3code_rows(conn: &Connection) -> Vec<(String, Option<String>)> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT workspace_root, updated_at FROM projection_projects WHERE deleted_at IS NULL",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?))
    }) else {
        return Vec::new();
    };
    rows.flatten().filter_map(|(p, t)| p.map(|p| (p, t))).collect()
}

fn import_t3code(db: &Db) -> Option<usize> {
    let path = t3code_db_path().filter(|p| p.is_file())?;
    let Some(conn) = open_ro(&path) else {
        return Some(0);
    };
    let now = model::now();
    let found = t3code_rows(&conn)
        .into_iter()
        .map(|(p, t)| {
            let ts = t.as_deref().and_then(parse_iso_timestamp).unwrap_or(now);
            (platform::clean_path_str(p.trim()), ts)
        })
        .collect();
    Some(store(db, found, "t3code", &mut HashSet::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_uris() {
        assert_eq!(uri_to_path("file:///Users/me/My%20Project", false).unwrap(), "/Users/me/My Project");
        assert_eq!(uri_to_path("file:///a/b%3Ac/", false).unwrap(), "/a/b:c");
        assert_eq!(uri_to_path("file:///a/%C3%A9%E2%82%AC", false).unwrap(), "/a/\u{e9}\u{20ac}");
        assert_eq!(uri_to_path("file:///c%3A/Users/x", true).unwrap(), r"c:\Users\x");
        assert_eq!(uri_to_path("file:///c:/Users/My%20App", true).unwrap(), r"c:\Users\My App");
        assert_eq!(uri_to_path("vscode-remote://ssh/x", false), None);
        assert_eq!(uri_to_path("file://server/share", false), None);
        assert_eq!(uri_to_path("file:///bad%FF", false), None);
        assert_eq!(uri_to_path("file:///100%", false).unwrap(), "/100%");
    }

    #[test]
    fn parses_zed_timestamps() {
        assert_eq!(parse_zed_timestamp("1970-01-01 00:00:00"), Some(0));
        assert_eq!(parse_zed_timestamp("2024-03-05 12:30:45"), Some(1709641845));
        assert_eq!(parse_zed_timestamp("2000-02-29 00:00:01"), Some(951782401));
        assert_eq!(parse_zed_timestamp("garbage"), None);
        assert_eq!(parse_zed_timestamp("2024-13-01 00:00:00"), None);
    }

    #[test]
    fn parses_vscode_entries() {
        let j = r#"{"entries":[
            {"folderUri":"file:///a/b"},
            {"fileUri":"file:///a/f.txt"},
            {"workspace":{"id":"x","configPath":"file:///w.code-workspace"}},
            {"folderUri":"vscode-remote://ssh-remote%2Bhost/home/x"},
            {"folderUri":"file:///c/d%20e"}]}"#;
        assert_eq!(parse_recent_folder_uris(j), vec!["file:///a/b", "file:///c/d%20e"]);
        assert!(parse_recent_folder_uris("nope").is_empty());
        assert!(parse_recent_folder_uris("{}").is_empty());
    }

    #[test]
    fn builds_sqlite_uri() {
        assert_eq!(
            sqlite_uri("/Users/me/Library/Application Support/Code/a?b#c.db", "immutable=1"),
            "file:/Users/me/Library/Application%20Support/Code/a%3Fb%23c.db?immutable=1"
        );
        assert_eq!(sqlite_uri(r"C:\Users\x\s.db", "immutable=1"), "file:/C:/Users/x/s.db?immutable=1");
    }

    /// Build a file URI the way VS Code writes it: file:///C:/a%20b on Windows, file:///a%20b on Unix.
    fn file_uri(path: &str) -> String {
        let p = path.replace('\\', "/").replace(' ', "%20");
        if p.starts_with('/') { format!("file://{p}") } else { format!("file:///{p}") }
    }

    #[test]
    fn reads_vscode_state_db() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("my proj");
        std::fs::create_dir(&proj).unwrap();
        let dbp = d.path().join("state.vscdb");
        let c = Connection::open(&dbp).unwrap();
        c.execute("CREATE TABLE ItemTable (key TEXT UNIQUE, value BLOB)", []).unwrap();
        let json = format!(
            r#"{{"entries":[{{"folderUri":"{}"}}]}}"#,
            file_uri(&proj.to_string_lossy())
        );
        c.execute("INSERT INTO ItemTable VALUES ('history.recentlyOpenedPathsList', ?1)", [json]).unwrap();
        drop(c);
        let got = recent_from_state_db(&dbp);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, proj.to_string_lossy());
    }

    #[test]
    fn parses_iso_timestamps() {
        assert_eq!(parse_iso_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_timestamp("2024-03-05T12:30:45.960Z"), Some(1709641845));
        assert_eq!(parse_iso_timestamp("nope"), None);
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
        c.execute("INSERT INTO projection_projects VALUES ('/a', '2026-10-22T18:18:34.960Z', NULL, NULL)", []).unwrap();
        c.execute("INSERT INTO projection_projects VALUES ('/b', '2026-10-22T18:18:34.960Z', 'x', NULL)", []).unwrap();
        drop(c);
        let conn = open_ro(&dbp).unwrap();
        let rows = t3code_rows(&conn);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "/a");
    }
}
