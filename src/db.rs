//! SQLite storage.
use crate::model::{now, Entry, Session, Status, Tag};
use anyhow::{Context, Result};
use rusqlite::params;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Db {
    pub conn: rusqlite::Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS dirs(
    path TEXT PRIMARY KEY,
    visits INTEGER NOT NULL DEFAULT 0,
    last_visit INTEGER NOT NULL DEFAULT 0,
    pinned INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS tags(
    path TEXT NOT NULL,
    tool TEXT NOT NULL,
    count INTEGER NOT NULL DEFAULT 0,
    last_used INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(path, tool)
);
CREATE TABLE IF NOT EXISTS tools(
    command TEXT PRIMARY KEY,
    tag TEXT NOT NULL,
    launch TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions(
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL,
    tool TEXT NOT NULL,
    status TEXT NOT NULL,
    shell_pid INTEGER,
    started_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_path_tool ON sessions(path, tool);
";

/// Seconds a done session stays visible.
const DONE_VISIBLE: i64 = 30 * 60;
/// Seconds without update before a working, waiting or ready session counts as done.
const STALE_AFTER: i64 = 12 * 3600;
/// Seconds before a done row is deleted.
const PRUNE_AFTER: i64 = 7 * 86400;

/// Directory that holds the database file.
fn data_dir() -> Result<PathBuf> {
    let non_empty = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let Some(d) = non_empty("GRACE_DATA_DIR") {
        return Ok(PathBuf::from(d));
    }
    if let Some(d) = non_empty("XDG_DATA_HOME") {
        return Ok(PathBuf::from(d).join("grace"));
    }
    if cfg!(windows) {
        if let Some(d) = non_empty("LOCALAPPDATA") {
            return Ok(PathBuf::from(d).join("grace"));
        }
    }
    let home = crate::platform::home_dir().context("HOME is not set")?;
    Ok(home.join(".local").join("share").join("grace"))
}

impl Db {
    /// Open (and create if needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = rusqlite::Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_millis(2000))?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Db { conn })
    }

    /// Open the default database. Returns (db, fresh) where fresh = the file did not exist before.
    pub fn open_default() -> Result<(Db, bool)> {
        let file = data_dir()?.join("grace.db");
        let fresh = !file.exists();
        Ok((Db::open(&file)?, fresh))
    }

    pub fn record_visit(&self, path: &str, ts: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO dirs(path, visits, last_visit) VALUES(?1, 1, ?2)
             ON CONFLICT(path) DO UPDATE SET visits = visits + 1, last_visit = ?2",
            params![path, ts],
        )?;
        Ok(())
    }

    pub fn record_tool(&self, path: &str, tool: &str, ts: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO dirs(path, visits, last_visit) VALUES(?1, 1, ?2)
             ON CONFLICT(path) DO UPDATE SET
                last_visit = max(last_visit, ?2),
                visits = CASE WHEN visits = 0 THEN 1 ELSE visits END",
            params![path, ts],
        )?;
        self.conn.execute(
            "INSERT INTO tags(path, tool, count, last_used) VALUES(?1, ?2, 1, ?3)
             ON CONFLICT(path, tool) DO UPDATE SET count = count + 1, last_used = ?3",
            params![path, tool, ts],
        )?;
        Ok(())
    }

    pub fn set_pinned(&self, path: &str, pinned: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO dirs(path, visits, last_visit, pinned) VALUES(?1, 0, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET pinned = ?3",
            params![path, now(), pinned as i64],
        )?;
        Ok(())
    }

    pub fn forget(&self, path: &str) -> Result<()> {
        self.conn.execute("DELETE FROM tags WHERE path = ?1", params![path])?;
        self.conn.execute("DELETE FROM dirs WHERE path = ?1", params![path])?;
        Ok(())
    }

    pub fn list_entries(&self) -> Result<Vec<Entry>> {
        let mut tags: HashMap<String, Vec<Tag>> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT path, tool, count, last_used FROM tags ORDER BY last_used DESC, tool ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Tag { tool: r.get(1)?, count: r.get(2)?, last_used: r.get(3)? },
            ))
        })?;
        for row in rows {
            let (p, t) = row?;
            tags.entry(p).or_default().push(t);
        }
        let mut stmt = self.conn.prepare("SELECT path, visits, last_visit, pinned FROM dirs")?;
        let rows = stmt.query_map([], |r| {
            Ok(Entry {
                path: r.get(0)?,
                visits: r.get(1)?,
                last_visit: r.get(2)?,
                pinned: r.get::<_, i64>(3)? != 0,
                tags: Vec::new(),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            let mut e = row?;
            e.tags = tags.remove(&e.path).unwrap_or_default();
            out.push(e);
        }
        Ok(out)
    }

    /// Add a custom tool, or replace the one with the same command.
    pub fn add_tool(&self, command: &str, tag: &str, launch: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tools(command, tag, launch) VALUES(?1, ?2, ?3)
             ON CONFLICT(command) DO UPDATE SET tag = ?2, launch = ?3",
            params![command, tag, launch],
        )?;
        Ok(())
    }

    /// Remove a custom tool. Returns true if it existed.
    pub fn remove_tool(&self, command: &str) -> Result<bool> {
        let n = self.conn.execute("DELETE FROM tools WHERE command = ?1", params![command])?;
        Ok(n > 0)
    }

    /// All custom tools as (command, tag, launch).
    pub fn custom_tools(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt =
            self.conn.prepare("SELECT command, tag, launch FROM tools ORDER BY command")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    fn ensure_dir(&self, path: &str, ts: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO dirs(path, visits, last_visit) VALUES(?1, 1, ?2)
             ON CONFLICT(path) DO UPDATE SET
                last_visit = max(last_visit, ?2),
                visits = CASE WHEN visits = 0 THEN 1 ELSE visits END",
            params![path, ts],
        )?;
        Ok(())
    }

    fn prune_sessions(&self, ts: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM sessions WHERE status = 'done' AND updated_at < ?1",
            params![ts - PRUNE_AFTER],
        )?;
        Ok(())
    }

    /// The CLI starts in a folder: mark the session working (new or existing).
    pub fn session_start(
        &self,
        path: &str,
        tool: &str,
        shell_pid: Option<i64>,
        ts: i64,
    ) -> Result<()> {
        self.ensure_dir(path, ts)?;
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM sessions WHERE path = ?1 AND tool = ?2 AND status != 'done'
                 ORDER BY id DESC LIMIT 1",
                params![path, tool],
                |r| r.get(0),
            )
            .ok();
        match id {
            Some(id) => {
                self.conn.execute(
                    "UPDATE sessions SET status = 'working', updated_at = ?2,
                        shell_pid = COALESCE(?3, shell_pid) WHERE id = ?1",
                    params![id, ts, shell_pid],
                )?;
            }
            None => {
                self.conn.execute(
                    "INSERT INTO sessions(path, tool, status, shell_pid, started_at, updated_at)
                     VALUES(?1, ?2, 'working', ?3, ?4, ?4)",
                    params![path, tool, shell_pid, ts],
                )?;
            }
        }
        self.prune_sessions(ts)
    }

    /// An agent hook reports a new status for the newest open session of (path, tool).
    pub fn session_set(&self, path: &str, tool: &str, status: Status, ts: i64) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE sessions SET status = ?3, updated_at = ?4 WHERE id = (
                SELECT id FROM sessions WHERE path = ?1 AND tool = ?2 AND status != 'done'
                ORDER BY id DESC LIMIT 1)",
            params![path, tool, status.as_str(), ts],
        )?;
        if n == 0 && status != Status::Done {
            self.record_tool(path, tool, ts)?; // a session always tags its folder
            self.conn.execute(
                "INSERT INTO sessions(path, tool, status, shell_pid, started_at, updated_at)
                 VALUES(?1, ?2, ?3, NULL, ?4, ?4)",
                params![path, tool, status.as_str(), ts],
            )?;
        }
        self.prune_sessions(ts)
    }

    /// The shell finished the command: mark its open sessions done.
    pub fn session_end_pid(&self, shell_pid: i64, ts: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET status = 'done', updated_at = ?2
             WHERE shell_pid = ?1 AND status != 'done'",
            params![shell_pid, ts],
        )?;
        self.prune_sessions(ts)
    }

    /// Open sessions plus sessions done in the last 30 minutes.
    /// Dead shells and stale sessions are marked done first.
    pub fn sessions(&self, now: i64) -> Result<Vec<Session>> {
        let open: Vec<(i64, Option<i64>, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, shell_pid, updated_at FROM sessions WHERE status != 'done'",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for (id, pid, updated) in open {
            let dead = pid.is_some_and(|p| !crate::platform::pid_alive(p));
            if dead || now - updated > STALE_AFTER {
                self.conn.execute(
                    "UPDATE sessions SET status = 'done', updated_at = ?2 WHERE id = ?1",
                    params![id, now],
                )?;
            }
        }
        self.prune_sessions(now)?;
        let mut stmt = self.conn.prepare(
            "SELECT id, path, tool, status, shell_pid, started_at, updated_at FROM sessions
             WHERE status != 'done' OR updated_at >= ?1 ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![now - DONE_VISIBLE], |r| {
            let st: String = r.get(3)?;
            Ok(Session {
                id: r.get(0)?,
                path: r.get(1)?,
                tool: r.get(2)?,
                status: Status::parse(&st).unwrap_or(Status::Done),
                shell_pid: r.get(4)?,
                started_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Insert a dir only if missing. Returns true if inserted.
    /// With `tool`, also add a tag with count 1 (only when the dir was inserted).
    pub fn import_dir(&self, path: &str, ts: i64, tool: Option<&str>) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO dirs(path, visits, last_visit) VALUES(?1, 1, ?2)",
            params![path, ts],
        )?;
        if let Some(t) = tool {
            self.conn.execute(
                "INSERT OR IGNORE INTO tags(path, tool, count, last_used) VALUES(?1, ?2, 1, ?3)",
                params![path, t, ts],
            )?;
        }
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("sub/grace.db")).unwrap();
        (dir, db)
    }

    fn find(db: &Db, p: &str) -> Option<Entry> {
        db.list_entries().unwrap().into_iter().find(|e| e.path == p)
    }

    #[test]
    fn visits_and_tags() {
        let (_d, db) = temp_db();
        db.record_visit("/a", 10).unwrap();
        db.record_visit("/a", 20).unwrap();
        let e = find(&db, "/a").unwrap();
        assert_eq!((e.visits, e.last_visit), (2, 20));
        db.record_tool("/a", "claude", 30).unwrap();
        db.record_tool("/a", "claude", 40).unwrap();
        db.record_tool("/a", "codex", 35).unwrap();
        let e = find(&db, "/a").unwrap();
        assert_eq!(e.tags[0].tool, "claude");
        assert_eq!(e.tags[0].count, 2);
        assert_eq!(e.tags[1].tool, "codex");
        assert_eq!(e.last_visit, 40);
    }

    #[test]
    fn record_tool_creates_dir() {
        let (_d, db) = temp_db();
        db.record_tool("/b", "claude", 5).unwrap();
        let e = find(&db, "/b").unwrap();
        assert_eq!(e.visits, 1);
    }

    #[test]
    fn pin_and_forget() {
        let (_d, db) = temp_db();
        db.set_pinned("/p", true).unwrap();
        assert!(find(&db, "/p").unwrap().pinned);
        db.set_pinned("/p", false).unwrap();
        assert!(!find(&db, "/p").unwrap().pinned);
        db.record_tool("/p", "claude", 1).unwrap();
        db.forget("/p").unwrap();
        assert!(find(&db, "/p").is_none());
    }

    #[test]
    fn import_does_not_overwrite() {
        let (_d, db) = temp_db();
        db.record_visit("/i", 100).unwrap();
        assert!(!db.import_dir("/i", 5, Some("claude")).unwrap());
        assert_eq!(find(&db, "/i").unwrap().last_visit, 100);
        assert!(db.import_dir("/j", 5, Some("claude")).unwrap());
        assert_eq!(find(&db, "/j").unwrap().tags.len(), 1);
    }

    fn status_of(db: &Db, now: i64, path: &str) -> Option<Status> {
        db.sessions(now).unwrap().into_iter().find(|s| s.path == path).map(|s| s.status)
    }

    #[test]
    fn session_lifecycle() {
        let (_d, db) = temp_db();
        let me = std::process::id() as i64;
        db.session_start("/s", "claude", Some(me), 100).unwrap();
        assert_eq!(status_of(&db, 101, "/s"), Some(Status::Working));
        assert!(find(&db, "/s").is_some());
        db.session_set("/s", "claude", Status::Waiting, 110).unwrap();
        assert_eq!(status_of(&db, 111, "/s"), Some(Status::Waiting));
        // A second start reuses the open session.
        db.session_start("/s", "claude", None, 120).unwrap();
        let all = db.sessions(121).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].status, all[0].shell_pid), (Status::Working, Some(me)));
        db.session_end_pid(me, 130).unwrap();
        assert_eq!(status_of(&db, 131, "/s"), Some(Status::Done));
    }

    #[test]
    fn session_set_without_start() {
        let (_d, db) = temp_db();
        db.session_set("/h", "codex", Status::Waiting, 10).unwrap();
        assert_eq!(status_of(&db, 11, "/h"), Some(Status::Waiting));
        db.session_set("/h", "codex", Status::Done, 12).unwrap();
        assert_eq!(status_of(&db, 13, "/h"), Some(Status::Done));
        // Done for an unknown session inserts nothing.
        db.session_set("/none", "codex", Status::Done, 14).unwrap();
        assert_eq!(status_of(&db, 15, "/none"), None);
    }

    #[cfg(unix)]
    #[test]
    fn dead_pid_is_done() {
        let (_d, db) = temp_db();
        db.session_start("/d", "claude", Some(999_999_999), 100).unwrap();
        assert_eq!(status_of(&db, 101, "/d"), Some(Status::Done));
    }

    #[test]
    fn stale_and_old_rules() {
        let (_d, db) = temp_db();
        db.session_set("/w", "claude", Status::Working, 0).unwrap();
        assert_eq!(status_of(&db, 11 * 3600, "/w"), Some(Status::Working));
        // Stale after 12 h, then visible as done for 30 min only.
        assert_eq!(status_of(&db, 12 * 3600 + 1, "/w"), Some(Status::Done));
        assert_eq!(status_of(&db, 12 * 3600 + 33 * 60, "/w"), None);
        // Old done rows are pruned.
        db.session_set("/r", "claude", Status::Ready, 0).unwrap();
        assert_eq!(status_of(&db, 12 * 3600 + 1, "/r"), Some(Status::Done));
        db.session_set("/x", "claude", Status::Waiting, 8 * 86400).unwrap();
        let n: i64 = db
            .conn
            .query_row("SELECT count(*) FROM sessions WHERE path = '/w'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn tools_round_trip() {
        let (_d, db) = temp_db();
        assert!(db.custom_tools().unwrap().is_empty());
        db.add_tool("q", "amazon-q", "q chat").unwrap();
        db.add_tool("kiro", "kiro", "kiro").unwrap();
        db.add_tool("kiro", "kiro2", "kiro x").unwrap();
        let t = db.custom_tools().unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0], ("kiro".into(), "kiro2".into(), "kiro x".into()));
        assert!(db.remove_tool("kiro").unwrap());
        assert!(!db.remove_tool("kiro").unwrap());
        assert_eq!(db.custom_tools().unwrap().len(), 1);
    }
}

#[cfg(test)]
mod session_tag_tests {
    use super::*;
    use crate::model::Status;

    #[test]
    fn hook_session_tags_the_folder() {
        let d = tempfile::tempdir().unwrap();
        let db = Db::open(&d.path().join("g.db")).unwrap();
        db.session_set("/p", "claude", Status::Waiting, 100).unwrap();
        let e = db.list_entries().unwrap();
        assert_eq!(e.len(), 1);
        assert!(e[0].has_tag("claude"));
    }
}
