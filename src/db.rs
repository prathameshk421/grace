//! SQLite storage.
use crate::model::{now, Entry, Session, Stats, Status, Tag};
use anyhow::{Context, Result};
use rusqlite::{config::DbConfig, params, Transaction, TransactionBehavior};
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
    updated_at INTEGER NOT NULL,
    session_id TEXT,
    detail TEXT,
    tmux_pane TEXT
);
CREATE INDEX IF NOT EXISTS sessions_path_tool ON sessions(path, tool);
CREATE INDEX IF NOT EXISTS sessions_shell_pid ON sessions(shell_pid);
CREATE INDEX IF NOT EXISTS sessions_status_updated ON sessions(status, updated_at);
CREATE TABLE IF NOT EXISTS meta(
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// Seconds a done session stays visible.
const DONE_VISIBLE: i64 = 30 * 60;
/// Seconds without update before a working, waiting or ready session counts as done.
const STALE_AFTER: i64 = 12 * 3600;
/// Seconds before a done row is deleted.
const PRUNE_AFTER: i64 = 7 * 86400;

/// Directory that holds the database file.
pub fn data_dir() -> Result<PathBuf> {
    let non_empty = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let Some(d) = non_empty("GRACE_DATA_DIR") {
        return Ok(PathBuf::from(d));
    }
    if cfg!(windows) {
        if let Some(d) = non_empty("LOCALAPPDATA") {
            return Ok(PathBuf::from(d).join("grace"));
        }
    }
    if let Some(d) = non_empty("XDG_DATA_HOME").filter(|d| Path::new(d).is_absolute()) {
        return Ok(PathBuf::from(d).join("grace"));
    }
    let home = crate::platform::home_dir().context("HOME is not set")?;
    Ok(home.join(".local").join("share").join("grace"))
}

impl Db {
    /// Open (and create if needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                crate::platform::create_dir_private(parent)?;
            }
        }
        let conn = rusqlite::Connection::open(path)?;
        crate::platform::restrict_file_0600(path);
        conn.busy_timeout(std::time::Duration::from_millis(2000))?;
        let _ = conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true);
        let _ = conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false);
        conn.execute_batch("PRAGMA trusted_schema = OFF; PRAGMA synchronous = NORMAL;")?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;

        let ver: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if ver < 1 {
            let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;
            let inner_ver: i64 = tx
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap_or(0);
            if inner_ver < 1 {
                tx.execute_batch(SCHEMA)?;
                // Existing v1.x/v2.0 databases may already have `sessions` without the new columns.
                for col in ["session_id", "detail", "tmux_pane"] {
                    if !has_column(&tx, "sessions", col)? {
                        tx.execute(&format!("ALTER TABLE sessions ADD COLUMN {col} TEXT"), [])?;
                    }
                }
                tx.execute_batch("PRAGMA user_version = 1;")?;
            }
            tx.commit()?;
        }
        Ok(Db { conn })
    }

    /// Open the default database. Returns (db, fresh) where fresh = the file did not exist before.
    pub fn open_default() -> Result<(Db, bool)> {
        let file = data_dir()?.join("grace.db");
        let fresh = !file.exists();
        Ok((Db::open(&file)?, fresh))
    }

    fn immediate_tx(&self) -> Result<Transaction<'_>> {
        Ok(Transaction::new_unchecked(
            &self.conn,
            TransactionBehavior::Immediate,
        )?)
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
        let tx = self.immediate_tx()?;
        tx.execute(
            "INSERT INTO dirs(path, visits, last_visit) VALUES(?1, 1, ?2)
             ON CONFLICT(path) DO UPDATE SET
                last_visit = max(last_visit, ?2),
                visits = CASE WHEN visits = 0 THEN 1 ELSE visits END",
            params![path, ts],
        )?;
        tx.execute(
            "INSERT INTO tags(path, tool, count, last_used) VALUES(?1, ?2, 1, ?3)
             ON CONFLICT(path, tool) DO UPDATE SET count = count + 1, last_used = ?3",
            params![path, tool, ts],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Pin or unpin a folder. `pinned == false` only updates existing rows and returns whether one existed.
    pub fn set_pinned(&self, path: &str, pinned: bool) -> Result<bool> {
        if pinned {
            let n = self.conn.execute(
                "INSERT INTO dirs(path, visits, last_visit, pinned) VALUES(?1, 0, ?2, 1)
                 ON CONFLICT(path) DO UPDATE SET pinned = 1",
                params![path, now()],
            )?;
            Ok(n > 0)
        } else {
            let n = self
                .conn
                .execute("UPDATE dirs SET pinned = 0 WHERE path = ?1", params![path])?;
            Ok(n > 0)
        }
    }

    pub fn forget(&self, path: &str) -> Result<bool> {
        let tx = self.immediate_tx()?;
        tx.execute("DELETE FROM tags WHERE path = ?1", params![path])?;
        let n = tx.execute("DELETE FROM dirs WHERE path = ?1", params![path])?;
        tx.execute("DELETE FROM sessions WHERE path = ?1", params![path])?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn list_entries(&self) -> Result<Vec<Entry>> {
        let mut tags: HashMap<String, Vec<Tag>> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT path, tool, count, last_used FROM tags ORDER BY last_used DESC, tool ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Tag {
                    tool: r.get(1)?,
                    count: r.get(2)?,
                    last_used: r.get(3)?,
                },
            ))
        })?;
        for row in rows {
            let (p, t) = row?;
            tags.entry(p).or_default().push(t);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT path, visits, last_visit, pinned FROM dirs")?;
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
        let n = self
            .conn
            .execute("DELETE FROM tools WHERE command = ?1", params![command])?;
        Ok(n > 0)
    }

    /// All custom tools as (command, tag, launch).
    pub fn custom_tools(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT command, tag, launch FROM tools ORDER BY command")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    fn prune_sessions_tx(tx: &Transaction<'_>, ts: i64) -> Result<()> {
        tx.execute(
            "DELETE FROM sessions WHERE status = 'done' AND updated_at < ?1",
            params![ts.saturating_sub(PRUNE_AFTER)],
        )?;
        Ok(())
    }

    /// The CLI starts in a folder: mark the session working (new or existing).
    #[cfg(test)]
    pub fn session_start(
        &self,
        path: &str,
        tool: &str,
        shell_pid: Option<i64>,
        ts: i64,
    ) -> Result<()> {
        self.session_start_ex(path, tool, shell_pid, None, None, None, ts)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn session_start_ex(
        &self,
        path: &str,
        tool: &str,
        shell_pid: Option<i64>,
        session_id: Option<&str>,
        detail: Option<&str>,
        tmux_pane: Option<&str>,
        ts: i64,
    ) -> Result<()> {
        let sid = session_id.filter(|s| !s.is_empty());
        let tx = self.immediate_tx()?;
        tx.execute(
            "INSERT INTO dirs(path, visits, last_visit) VALUES(?1, 1, ?2)
             ON CONFLICT(path) DO UPDATE SET
                last_visit = max(last_visit, ?2),
                visits = CASE WHEN visits = 0 THEN 1 ELSE visits END",
            params![path, ts],
        )?;
        let existing: Option<i64> = if let Some(sid) = sid {
            tx.query_row(
                "SELECT id FROM sessions WHERE path = ?1 AND tool = ?2 AND session_id = ?3 AND status != 'done'
                 ORDER BY id DESC LIMIT 1",
                params![path, tool, sid],
                |r| r.get(0),
            )
            .ok()
        } else if let Some(pid) = shell_pid {
            tx.query_row(
                "SELECT id FROM sessions WHERE path = ?1 AND tool = ?2 AND status != 'done'
                 AND (shell_pid = ?3 OR shell_pid IS NULL)
                 ORDER BY CASE WHEN shell_pid = ?3 THEN 0 ELSE 1 END, id DESC LIMIT 1",
                params![path, tool, pid],
                |r| r.get(0),
            )
            .ok()
        } else {
            tx.query_row(
                "SELECT id FROM sessions WHERE path = ?1 AND tool = ?2 AND status != 'done'
                 ORDER BY id DESC LIMIT 1",
                params![path, tool],
                |r| r.get(0),
            )
            .ok()
        };
        match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE sessions SET status = 'working', updated_at = ?2,
                        shell_pid = COALESCE(?3, shell_pid),
                        session_id = COALESCE(?4, session_id),
                        detail = COALESCE(?5, detail),
                        tmux_pane = COALESCE(?6, tmux_pane)
                     WHERE id = ?1",
                    params![id, ts, shell_pid, sid, detail, tmux_pane],
                )?;
            }
            None => {
                tx.execute(
                    "INSERT INTO sessions(path, tool, status, shell_pid, started_at, updated_at, session_id, detail, tmux_pane)
                     VALUES(?1, ?2, 'working', ?3, ?4, ?4, ?5, ?6, ?7)",
                    params![path, tool, shell_pid, ts, sid, detail, tmux_pane],
                )?;
            }
        }
        Self::prune_sessions_tx(&tx, ts)?;
        tx.commit()?;
        Ok(())
    }

    /// An agent hook reports a new status for the matching open session.
    #[cfg(test)]
    pub fn session_set(&self, path: &str, tool: &str, status: Status, ts: i64) -> Result<()> {
        self.session_set_ex(path, tool, status, None, None, None, ts)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn session_set_ex(
        &self,
        path: &str,
        tool: &str,
        status: Status,
        session_id: Option<&str>,
        detail: Option<&str>,
        tmux_pane: Option<&str>,
        ts: i64,
    ) -> Result<()> {
        let sid = session_id.filter(|s| !s.is_empty());
        let tx = self.immediate_tx()?;
        let target_id: Option<i64> = if let Some(sid) = sid {
            tx.query_row(
                "SELECT id FROM sessions WHERE path = ?1 AND tool = ?2 AND status != 'done'
                 AND (session_id = ?3 OR session_id IS NULL)
                 ORDER BY CASE WHEN session_id = ?3 THEN 0 ELSE 1 END, id DESC LIMIT 1",
                params![path, tool, sid],
                |r| r.get(0),
            )
            .ok()
        } else {
            tx.query_row(
                "SELECT id FROM sessions WHERE path = ?1 AND tool = ?2 AND status != 'done'
                 ORDER BY id DESC LIMIT 1",
                params![path, tool],
                |r| r.get(0),
            )
            .ok()
        };
        let mut updated = 0usize;
        if let Some(id) = target_id {
            let clear_detail = matches!(status, Status::Ready | Status::Done);
            updated = if clear_detail {
                tx.execute(
                    "UPDATE sessions SET status = ?2, updated_at = ?3,
                        session_id = COALESCE(?4, session_id),
                        detail = ?5,
                        tmux_pane = COALESCE(?6, tmux_pane)
                     WHERE id = ?1",
                    params![id, status.as_str(), ts, sid, detail, tmux_pane],
                )?
            } else {
                tx.execute(
                    "UPDATE sessions SET status = ?2, updated_at = ?3,
                        session_id = COALESCE(?4, session_id),
                        detail = COALESCE(?5, detail),
                        tmux_pane = COALESCE(?6, tmux_pane)
                     WHERE id = ?1",
                    params![id, status.as_str(), ts, sid, detail, tmux_pane],
                )?
            };
        }
        if updated == 0 && status != Status::Done {
            tx.execute(
                "INSERT INTO dirs(path, visits, last_visit) VALUES(?1, 1, ?2)
                 ON CONFLICT(path) DO UPDATE SET
                    last_visit = max(last_visit, ?2),
                    visits = CASE WHEN visits = 0 THEN 1 ELSE visits END",
                params![path, ts],
            )?;
            tx.execute(
                "INSERT INTO tags(path, tool, count, last_used) VALUES(?1, ?2, 1, ?3)
                 ON CONFLICT(path, tool) DO UPDATE SET count = count + 1, last_used = ?3",
                params![path, tool, ts],
            )?;
            tx.execute(
                "INSERT INTO sessions(path, tool, status, shell_pid, started_at, updated_at, session_id, detail, tmux_pane)
                 VALUES(?1, ?2, ?3, NULL, ?4, ?4, ?5, ?6, ?7)",
                params![path, tool, status.as_str(), ts, sid, detail, tmux_pane],
            )?;
        }
        Self::prune_sessions_tx(&tx, ts)?;
        tx.commit()?;
        Ok(())
    }

    /// The shell finished the command: mark its open sessions done.
    pub fn session_end_pid(&self, shell_pid: i64, ts: i64) -> Result<()> {
        let tx = self.immediate_tx()?;
        tx.execute(
            "UPDATE sessions SET status = 'done', updated_at = ?2, detail = NULL
             WHERE shell_pid = ?1 AND status != 'done'",
            params![shell_pid, ts],
        )?;
        Self::prune_sessions_tx(&tx, ts)?;
        tx.commit()?;
        Ok(())
    }

    /// Open sessions plus sessions done in the last 30 minutes.
    /// Dead shells and stale sessions are marked done first without resurrecting old ones as "now".
    pub fn sessions(&self, now: i64) -> Result<Vec<Session>> {
        let open: Vec<(i64, Option<i64>, i64)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id, shell_pid, updated_at FROM sessions WHERE status != 'done'")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let to_close: Vec<(i64, i64)> = open
            .into_iter()
            .filter_map(|(id, pid, updated)| {
                let dead = pid.is_some_and(|p| !crate::platform::pid_alive(p));
                let stale = now.saturating_sub(updated) > STALE_AFTER;
                if dead {
                    Some((id, updated))
                } else if stale {
                    Some((id, updated.saturating_add(STALE_AFTER).min(now)))
                } else {
                    None
                }
            })
            .collect();
        if !to_close.is_empty() {
            let tx = self.immediate_tx()?;
            for (id, end_ts) in to_close {
                tx.execute(
                    "UPDATE sessions SET status = 'done', updated_at = ?2, detail = NULL WHERE id = ?1",
                    params![id, end_ts],
                )?;
            }
            Self::prune_sessions_tx(&tx, now)?;
            tx.commit()?;
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, path, tool, status, shell_pid, started_at, updated_at, session_id, detail, tmux_pane
             FROM sessions
             WHERE status != 'done' OR updated_at >= ?1
             ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![now.saturating_sub(DONE_VISIBLE)], |r| {
            let st: String = r.get(3)?;
            Ok(Session {
                id: r.get(0)?,
                path: r.get(1)?,
                tool: r.get(2)?,
                status: Status::parse(&st).unwrap_or(Status::Done),
                shell_pid: r.get(4)?,
                started_at: r.get(5)?,
                updated_at: r.get(6)?,
                session_id: r.get(7)?,
                detail: r.get(8)?,
                tmux_pane: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Read a value from the `meta` table.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        Ok(match rows.next()? {
            Some(r) => Some(r.get(0)?),
            None => None,
        })
    }

    /// Write a value to the `meta` table.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    /// Insert a dir only if missing. Returns true if inserted.
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

    /// Batch import many `(path, ts)` entries in a single immediate transaction.
    /// Returns the number of distinct entries attempted.
    pub fn import_dirs_batch(&self, items: &[(String, i64)], tool: Option<&str>) -> Result<usize> {
        if items.is_empty() {
            return Ok(0);
        }
        let tx = self.immediate_tx()?;
        let mut dir_stmt =
            tx.prepare("INSERT OR IGNORE INTO dirs(path, visits, last_visit) VALUES(?1, 1, ?2)")?;
        let mut tag_stmt = if tool.is_some() {
            Some(tx.prepare(
                "INSERT OR IGNORE INTO tags(path, tool, count, last_used) VALUES(?1, ?2, 1, ?3)",
            )?)
        } else {
            None
        };
        for (path, ts) in items {
            dir_stmt.execute(params![path, ts])?;
            if let (Some(t), Some(ref mut st)) = (tool, &mut tag_stmt) {
                st.execute(params![path, t, ts])?;
            }
        }
        drop(dir_stmt);
        drop(tag_stmt);
        tx.commit()?;
        Ok(items.len())
    }

    /// Tracked directories that no longer exist on disk (optionally older than `older_than_days`).
    pub fn list_missing(&self, older_than_days: Option<i64>, now: i64) -> Result<Vec<Entry>> {
        let cutoff = older_than_days.map(|d| now.saturating_sub(d.saturating_mul(86400)));
        let entries = self.list_entries()?;
        Ok(entries
            .into_iter()
            .filter(|e| !Path::new(&e.path).is_dir())
            .filter(|e| cutoff.is_none_or(|c| e.last_visit <= c))
            .collect())
    }

    /// Delete multiple folder paths and their tags/sessions in one transaction.
    pub fn delete_paths(&self, paths: &[String]) -> Result<usize> {
        if paths.is_empty() {
            return Ok(0);
        }
        let tx = self.immediate_tx()?;
        let mut n = 0usize;
        for p in paths {
            tx.execute("DELETE FROM tags WHERE path = ?1", params![p])?;
            n += tx.execute("DELETE FROM dirs WHERE path = ?1", params![p])?;
            tx.execute("DELETE FROM sessions WHERE path = ?1", params![p])?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Collect aggregate stats for `grace stats` and `grace doctor`.
    pub fn stats(&self, now: i64) -> Result<Stats> {
        let mut entries = self.list_entries()?;
        let total_dirs = entries.len();
        let existing_dirs = entries
            .iter()
            .filter(|e| Path::new(&e.path).is_dir())
            .count();
        let missing_dirs = total_dirs.saturating_sub(existing_dirs);
        let pinned_dirs = entries.iter().filter(|e| e.pinned).count();
        let total_visits: i64 = entries
            .iter()
            .fold(0i64, |acc, e| acc.saturating_add(e.visits.max(0)));
        let mut tool_map: HashMap<String, (i64, usize)> = HashMap::new();
        let mut total_tags = 0i64;
        for e in &entries {
            for t in &e.tags {
                total_tags = total_tags.saturating_add(t.count.max(0));
                let entry = tool_map.entry(t.tool.clone()).or_insert((0, 0));
                entry.0 = entry.0.saturating_add(t.count.max(0));
                entry.1 += 1;
            }
        }
        let mut top_tools: Vec<(String, i64, usize)> = tool_map
            .into_iter()
            .map(|(k, (runs, folders))| (k, runs, folders))
            .collect();
        top_tools.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
        crate::rank::sort_entries(&mut entries, now);
        entries.retain(|e| Path::new(&e.path).is_dir());
        entries.truncate(5);
        let live_sessions = self
            .sessions(now)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.status != Status::Done)
            .count();
        Ok(Stats {
            total_dirs,
            existing_dirs,
            missing_dirs,
            pinned_dirs,
            total_visits,
            total_tags,
            live_sessions,
            top_tools,
            top_dirs: entries,
        })
    }

    /// Run `PRAGMA quick_check` on the database.
    pub fn integrity_check(&self) -> Result<bool> {
        let res: String = self
            .conn
            .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))?;
        Ok(res.eq_ignore_ascii_case("ok"))
    }
}

fn has_column(conn: &rusqlite::Connection, table: &str, col: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for r in rows {
        if r? == col {
            return Ok(true);
        }
    }
    Ok(false)
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
        // Unpin on untracked folder inserts nothing.
        assert!(!db.set_pinned("/missing", false).unwrap());
        assert!(find(&db, "/missing").is_none());
        db.set_pinned("/p", true).unwrap();
        assert!(find(&db, "/p").unwrap().pinned);
        assert!(db.set_pinned("/p", false).unwrap());
        assert!(!find(&db, "/p").unwrap().pinned);
        db.record_tool("/p", "claude", 1).unwrap();
        assert!(db.forget("/p").unwrap());
        assert!(!db.forget("/p").unwrap());
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
        db.sessions(now)
            .unwrap()
            .into_iter()
            .find(|s| s.path == path)
            .map(|s| s.status)
    }

    #[test]
    fn session_lifecycle() {
        let (_d, db) = temp_db();
        let me = std::process::id() as i64;
        db.session_start("/s", "claude", Some(me), 100).unwrap();
        assert_eq!(status_of(&db, 101, "/s"), Some(Status::Working));
        assert!(find(&db, "/s").is_some());
        db.session_set("/s", "claude", Status::Waiting, 110)
            .unwrap();
        assert_eq!(status_of(&db, 111, "/s"), Some(Status::Waiting));
        // A second start reuses the open session.
        db.session_start("/s", "claude", None, 120).unwrap();
        let all = db.sessions(121).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(
            (all[0].status, all[0].shell_pid),
            (Status::Working, Some(me))
        );
        db.session_end_pid(me, 130).unwrap();
        assert_eq!(status_of(&db, 131, "/s"), Some(Status::Done));
    }

    #[test]
    fn concurrent_sessions_by_session_id() {
        let (_d, db) = temp_db();
        db.session_set_ex(
            "/p",
            "claude",
            Status::Working,
            Some("s1"),
            Some("Bash"),
            None,
            100,
        )
        .unwrap();
        db.session_set_ex(
            "/p",
            "claude",
            Status::Waiting,
            Some("s2"),
            Some("permission"),
            None,
            105,
        )
        .unwrap();
        let all = db.sessions(110).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].session_id.as_deref(), Some("s2"));
        assert_eq!(all[0].detail.as_deref(), Some("permission"));
        assert_eq!(all[1].session_id.as_deref(), Some("s1"));
        assert_eq!(all[1].detail.as_deref(), Some("Bash"));
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
        db.session_start("/d", "claude", Some(999_999_999), 100)
            .unwrap();
        assert_eq!(status_of(&db, 101, "/d"), Some(Status::Done));
        // Dead session from days ago does NOT resurrect as "done now".
        db.session_start("/old-dead", "claude", Some(999_999_998), 100)
            .unwrap();
        assert_eq!(status_of(&db, 3 * 86400, "/old-dead"), None);
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
        db.session_set("/x", "claude", Status::Waiting, 8 * 86400)
            .unwrap();
        let n: i64 = db
            .conn
            .query_row("SELECT count(*) FROM sessions WHERE path = '/w'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn meta_round_trip_and_integrity() {
        let (_d, db) = temp_db();
        assert!(db.integrity_check().unwrap());
        assert_eq!(db.get_meta("k").unwrap(), None);
        db.set_meta("k", "1").unwrap();
        db.set_meta("k", "2").unwrap();
        assert_eq!(db.get_meta("k").unwrap().as_deref(), Some("2"));
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
        db.session_set("/p", "claude", Status::Waiting, 100)
            .unwrap();
        let e = db.list_entries().unwrap();
        assert_eq!(e.len(), 1);
        assert!(e[0].has_tag("claude"));
    }
}
