//! SQLite storage.
use crate::model::{now, Entry, Tag};
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
";

/// Directory that holds the database file.
fn data_dir() -> Result<PathBuf> {
    let non_empty = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let Some(d) = non_empty("GRACE_DATA_DIR") {
        return Ok(PathBuf::from(d));
    }
    if let Some(d) = non_empty("XDG_DATA_HOME") {
        return Ok(PathBuf::from(d).join("grace"));
    }
    let home = non_empty("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/share/grace"))
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
