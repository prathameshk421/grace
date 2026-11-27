//! SQLite storage.
use crate::model::{now, Entry, Session, Stats, Status, Tag};
use anyhow::{Context, Result};
use rusqlite::{config::DbConfig, params, Connection, OpenFlags, Transaction, TransactionBehavior};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Db {
    pub conn: Connection,
}

const SCHEMA_VER: i64 = 2;

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
CREATE INDEX IF NOT EXISTS sessions_open_path_tool ON sessions(path, tool, id DESC) WHERE status != 'done';
CREATE INDEX IF NOT EXISTS sessions_shell_pid ON sessions(shell_pid);
CREATE INDEX IF NOT EXISTS sessions_status_updated ON sessions(status, updated_at);
CREATE INDEX IF NOT EXISTS sessions_updated_at ON sessions(updated_at DESC, id DESC);
CREATE TABLE IF NOT EXISTS meta(
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

const UPSERT_DIR_SQL: &str = "INSERT INTO dirs(path, visits, last_visit) VALUES(?1, 1, ?2)
 ON CONFLICT(path) DO UPDATE SET
    last_visit = max(last_visit, ?2),
    visits = CASE WHEN visits = 0 THEN 1 ELSE visits END";

const UPSERT_TAG_SQL: &str = "INSERT INTO tags(path, tool, count, last_used) VALUES(?1, ?2, 1, ?3)
 ON CONFLICT(path, tool) DO UPDATE SET count = count + 1, last_used = ?3";

/// Seconds a done session stays visible.
const DONE_VISIBLE: i64 = 30 * 60;
/// Seconds without update before a working, waiting or ready session counts as done.
const STALE_AFTER: i64 = 12 * 3600;
/// Seconds before a done row is deleted.
const PRUNE_AFTER: i64 = 7 * 86400;

/// True when an open session's shell is gone (dead PID).
pub fn session_dead(shell_pid: Option<i64>) -> bool {
    shell_pid.is_some_and(|p| !crate::platform::pid_alive(p))
}

/// True when an open session had no update for `STALE_AFTER` seconds.
pub fn session_stale(updated_at: i64, now: i64) -> bool {
    now.saturating_sub(updated_at) > STALE_AFTER
}

/// True when an open session should be shown as done: its shell died or it went stale.
/// `Db::sessions` closes such rows. Views that cache sessions use this to know when to reload.
pub fn session_expired(s: &Session, now: i64) -> bool {
    s.status != Status::Done && (session_dead(s.shell_pid) || session_stale(s.updated_at, now))
}

/// Optional metadata attached to a live session update.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionMeta<'a> {
    pub session_id: Option<&'a str>,
    pub detail: Option<&'a str>,
    pub tmux_pane: Option<&'a str>,
}

/// Directory that holds the database file.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(d) = crate::platform::env_non_empty("GRACE_DATA_DIR") {
        return Ok(PathBuf::from(d));
    }
    if cfg!(windows) {
        if let Some(d) = crate::platform::env_non_empty("LOCALAPPDATA") {
            return Ok(PathBuf::from(d).join("grace"));
        }
    }
    if let Some(d) =
        crate::platform::env_non_empty("XDG_DATA_HOME").filter(|d| Path::new(d).is_absolute())
    {
        return Ok(PathBuf::from(d).join("grace"));
    }
    let home = crate::platform::home_dir().context("HOME is not set")?;
    Ok(home.join(".local").join("share").join("grace"))
}

fn configure_defensive(conn: &Connection, timeout_ms: u64) {
    let _ = conn.busy_timeout(std::time::Duration::from_millis(timeout_ms));
    let _ = conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true);
    let _ = conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false);
}

fn restrict_db_files(path: &Path) {
    crate::platform::restrict_file_0600(path);
    let s = path.to_string_lossy();
    let wal = PathBuf::from(format!("{s}-wal"));
    if wal.exists() {
        crate::platform::restrict_file_0600(&wal);
    }
    let shm = PathBuf::from(format!("{s}-shm"));
    if shm.exists() {
        crate::platform::restrict_file_0600(&shm);
    }
}

/// Open an arbitrary SQLite file read-only with URI fallback (`mode=ro`, then `immutable=1`).
pub(crate) fn open_readonly_uri(path: &Path, timeout_ms: u64) -> Option<Connection> {
    let raw = path.to_string_lossy();
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let conn =
        Connection::open_with_flags(crate::import_editors::sqlite_uri(&raw, "mode=ro"), flags)
            .or_else(|_| {
                Connection::open_with_flags(
                    crate::import_editors::sqlite_uri(&raw, "immutable=1"),
                    flags,
                )
            })
            .ok()?;
    configure_defensive(&conn, timeout_ms);
    Some(conn)
}

impl Db {
    /// Open (and create if needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Db> {
        let fresh = !path.exists();
        if fresh {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    crate::platform::create_dir_private(parent)?;
                }
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                let _ = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path);
            }
        }
        let conn = Connection::open(path)?;
        configure_defensive(&conn, 5000);
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA temp_store = MEMORY;")?;

        let ver: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if fresh || ver < SCHEMA_VER {
            // `PRAGMA journal_mode=WAL` can return SQLITE_BUSY immediately during concurrent
            // first-open transitions even when busy_timeout is set; retry briefly.
            let mut last_err = None;
            for attempt in 0..40u32 {
                match conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(())) {
                    Ok(()) => {
                        last_err = None;
                        break;
                    }
                    Err(rusqlite::Error::SqliteFailure(err, _))
                        if matches!(
                            err.code,
                            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                        ) && attempt < 39 =>
                    {
                        last_err = Some(rusqlite::Error::SqliteFailure(err, None));
                        std::thread::sleep(std::time::Duration::from_millis(15));
                    }
                    Err(e) => {
                        last_err = Some(e);
                        break;
                    }
                }
            }
            if let Some(e) = last_err {
                return Err(e.into());
            }
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
            } else if inner_ver < 2 {
                tx.execute_batch(
                    "CREATE INDEX IF NOT EXISTS sessions_open_path_tool ON sessions(path, tool, id DESC) WHERE status != 'done';
                     CREATE INDEX IF NOT EXISTS sessions_updated_at ON sessions(updated_at DESC, id DESC);",
                )?;
            }
            if inner_ver < SCHEMA_VER {
                tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VER};"))?;
            }
            tx.commit()?;
            restrict_db_files(path);
        }
        Ok(Db { conn })
    }

    /// Open the default database. Returns (db, fresh) where fresh = the file did not exist before.
    pub fn open_default() -> Result<(Db, bool)> {
        let file = data_dir()?.join("grace.db");
        let fresh = !file.exists();
        Ok((Db::open(&file)?, fresh))
    }

    /// Open an existing database read-only. Returns `None` if it does not exist yet.
    pub fn open_readonly(path: &Path) -> Result<Option<Db>> {
        if !path.is_file() {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure_defensive(&conn, 5000);
        let _ = conn.execute_batch("PRAGMA temp_store = MEMORY;");
        Ok(Some(Db { conn }))
    }

    /// Open the default database read-only when present. Never creates files or acquires write locks.
    pub fn open_readonly_default() -> Result<Option<Db>> {
        let file = data_dir()?.join("grace.db");
        Self::open_readonly(&file)
    }

    fn immediate_tx(&self) -> Result<Transaction<'_>> {
        Ok(Transaction::new_unchecked(
            &self.conn,
            TransactionBehavior::Immediate,
        )?)
    }

    pub fn data_version(&self) -> i64 {
        self.conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn count_dirs(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM dirs", [], |r| r.get(0))?;
        Ok(n.max(0) as usize)
    }

    /// Fast tag normalization without materializing the full Registry.
    pub fn normalize_tool_tag(&self, name: &str) -> String {
        let trimmed = name.trim();
        if let Ok(tag) = self.conn.query_row(
            "SELECT tag FROM tools WHERE command = ?1 COLLATE NOCASE LIMIT 1",
            params![trimmed],
            |r| r.get::<_, String>(0),
        ) {
            return tag.to_ascii_lowercase();
        }
        if let Some((_, tag, _, _)) = crate::tools::TOOLS
            .iter()
            .find(|(cmd, ..)| cmd.eq_ignore_ascii_case(trimmed))
        {
            return (*tag).to_string();
        }
        let low = trimmed.to_ascii_lowercase();
        if let Ok(tag) = self.conn.query_row(
            "SELECT tag FROM tools WHERE LOWER(tag) = ?1 LIMIT 1",
            params![low],
            |r| r.get::<_, String>(0),
        ) {
            return tag.to_ascii_lowercase();
        }
        if let Some((_, tag, _, _)) = crate::tools::TOOLS.iter().find(|(_, tag, ..)| *tag == low) {
            return (*tag).to_string();
        }
        low
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
        tx.execute(UPSERT_DIR_SQL, params![path, ts])?;
        tx.execute(UPSERT_TAG_SQL, params![path, tool, ts])?;
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
        Ok(self.delete_paths(&[path.to_string()])? > 0)
    }

    pub fn list_entries(&self) -> Result<Vec<Entry>> {
        let mut tags: HashMap<String, Vec<Tag>> = HashMap::new();
        let mut stmt = self
            .conn
            .prepare("SELECT path, tool, count, last_used FROM tags")?;
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
            if let Some(mut ts) = tags.remove(&e.path) {
                ts.sort_by(|a, b| {
                    b.last_used
                        .cmp(&a.last_used)
                        .then_with(|| a.tool.cmp(&b.tool))
                });
                e.tags = ts;
            }
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
    pub fn session_start(
        &self,
        path: &str,
        tool: &str,
        shell_pid: Option<i64>,
        meta: SessionMeta<'_>,
        ts: i64,
    ) -> Result<()> {
        let sid = meta.session_id.filter(|s| !s.is_empty());
        let tx = self.immediate_tx()?;
        tx.execute(UPSERT_DIR_SQL, params![path, ts])?;
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
                        detail = ?5,
                        tmux_pane = COALESCE(?6, tmux_pane)
                     WHERE id = ?1",
                    params![id, ts, shell_pid, sid, meta.detail, meta.tmux_pane],
                )?;
            }
            None => {
                tx.execute(
                    "INSERT INTO sessions(path, tool, status, shell_pid, started_at, updated_at, session_id, detail, tmux_pane)
                     VALUES(?1, ?2, 'working', ?3, ?4, ?4, ?5, ?6, ?7)",
                    params![path, tool, shell_pid, ts, sid, meta.detail, meta.tmux_pane],
                )?;
            }
        }
        // Throttle periodic session table pruning during rapid hook bursts.
        if (ts & 0xff) == 0 {
            Self::prune_sessions_tx(&tx, ts)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// An agent hook reports a new status for the matching open session.
    pub fn session_set(
        &self,
        path: &str,
        tool: &str,
        status: Status,
        meta: SessionMeta<'_>,
        ts: i64,
    ) -> Result<()> {
        let sid = meta.session_id.filter(|s| !s.is_empty());
        let tx = self.immediate_tx()?;
        let target_id: Option<i64> = if let Some(sid) = sid {
            if status == Status::Done {
                tx.query_row(
                    "SELECT id FROM sessions WHERE path = ?1 AND tool = ?2 AND status != 'done' AND session_id = ?3
                     ORDER BY id DESC LIMIT 1",
                    params![path, tool, sid],
                    |r| r.get(0),
                )
                .ok()
            } else {
                tx.query_row(
                    "SELECT id FROM sessions WHERE path = ?1 AND tool = ?2 AND status != 'done'
                     AND (session_id = ?3 OR session_id IS NULL)
                     ORDER BY CASE WHEN session_id = ?3 THEN 0 ELSE 1 END, id DESC LIMIT 1",
                    params![path, tool, sid],
                    |r| r.get(0),
                )
                .ok()
            }
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
            updated = tx.execute(
                "UPDATE sessions SET status = ?2, updated_at = ?3,
                    session_id = COALESCE(?4, session_id),
                    detail = ?5,
                    tmux_pane = COALESCE(?6, tmux_pane)
                 WHERE id = ?1",
                params![id, status.as_str(), ts, sid, meta.detail, meta.tmux_pane],
            )?;
        }
        if updated == 0 && status != Status::Done {
            tx.execute(UPSERT_DIR_SQL, params![path, ts])?;
            tx.execute(UPSERT_TAG_SQL, params![path, tool, ts])?;
            tx.execute(
                "INSERT INTO sessions(path, tool, status, shell_pid, started_at, updated_at, session_id, detail, tmux_pane)
                 VALUES(?1, ?2, ?3, NULL, ?4, ?4, ?5, ?6, ?7)",
                params![path, tool, status.as_str(), ts, sid, meta.detail, meta.tmux_pane],
            )?;
        }
        if status == Status::Done || (ts & 0xff) == 0 {
            Self::prune_sessions_tx(&tx, ts)?;
        }
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

    fn load_visible_sessions(&self, now: i64) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, tool, status, shell_pid, started_at, updated_at, session_id, detail, tmux_pane
             FROM sessions WHERE status != 'done'
             UNION ALL
             SELECT id, path, tool, status, shell_pid, started_at, updated_at, session_id, detail, tmux_pane
             FROM sessions WHERE status = 'done' AND updated_at >= ?1",
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
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Read-only inspection of `(live_active_count, dead_or_stale_count)` for `grace doctor`.
    pub fn inspect_sessions(&self, now: i64) -> Result<(usize, usize)> {
        let mut stmt = self
            .conn
            .prepare("SELECT shell_pid, updated_at FROM sessions WHERE status != 'done'")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, i64>(1)?))
        })?;
        let (mut active, mut stale_or_dead) = (0usize, 0usize);
        for row in rows {
            let (pid, updated) = row?;
            let dead = session_dead(pid);
            let stale = session_stale(updated, now);
            if dead || stale {
                stale_or_dead += 1;
            } else {
                active += 1;
            }
        }
        Ok((active, stale_or_dead))
    }

    /// Open sessions plus sessions done in the last 30 minutes.
    /// Dead shells and stale sessions are marked done first without resurrecting old ones as "now".
    pub fn sessions(&self, now: i64) -> Result<Vec<Session>> {
        let mut all = self.load_visible_sessions(now)?;
        let to_close: Vec<(i64, i64, i64)> = all
            .iter()
            .filter(|s| s.status != Status::Done)
            .filter_map(|s| {
                let dead = session_dead(s.shell_pid);
                let stale = session_stale(s.updated_at, now);
                if dead {
                    Some((s.id, s.updated_at, s.updated_at))
                } else if stale {
                    Some((
                        s.id,
                        s.updated_at.saturating_add(STALE_AFTER).min(now),
                        s.updated_at,
                    ))
                } else {
                    None
                }
            })
            .collect();
        if !to_close.is_empty() {
            let tx = self.immediate_tx()?;
            {
                let mut stmt = tx.prepare(
                    "UPDATE sessions SET status = 'done', updated_at = ?2, detail = NULL
                     WHERE id = ?1 AND status != 'done' AND updated_at = ?3",
                )?;
                for &(id, end_ts, prev_ts) in &to_close {
                    stmt.execute(params![id, end_ts, prev_ts])?;
                }
            }
            Self::prune_sessions_tx(&tx, now)?;
            tx.commit()?;

            let cutoff = now.saturating_sub(DONE_VISIBLE);
            for (id, end_ts, prev_ts) in to_close {
                if let Some(s) = all
                    .iter_mut()
                    .find(|s| s.id == id && s.status != Status::Done && s.updated_at == prev_ts)
                {
                    s.status = Status::Done;
                    s.updated_at = end_ts;
                    s.detail = None;
                }
            }
            all.retain(|s| s.status != Status::Done || s.updated_at >= cutoff);
        }
        all.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(all)
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

    /// Write multiple meta key-value pairs in a single transaction.
    pub fn save_meta_batch(&self, pairs: &[(&str, &str)]) -> Result<()> {
        if pairs.is_empty() {
            return Ok(());
        }
        let tx = self.immediate_tx()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = ?2",
            )?;
            for (k, v) in pairs {
                stmt.execute(params![k, v])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Insert a dir only if missing. Returns true if inserted.
    #[cfg(test)]
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

    /// Batch import open-editor `(path, tag)` entries in a single immediate transaction.
    pub fn import_open_editors_batch(&self, items: &[(&str, &str)], ts: i64) -> Result<usize> {
        if items.is_empty() {
            return Ok(0);
        }
        let tx = self.immediate_tx()?;
        let mut inserted = 0usize;
        {
            let mut dir_stmt = tx.prepare(
                "INSERT OR IGNORE INTO dirs(path, visits, last_visit) VALUES(?1, 1, ?2)",
            )?;
            let mut tag_stmt = tx.prepare(
                "INSERT OR IGNORE INTO tags(path, tool, count, last_used) VALUES(?1, ?2, 1, ?3)",
            )?;
            for &(path, tag) in items {
                inserted += dir_stmt.execute(params![path, ts])?;
                tag_stmt.execute(params![path, tag, ts])?;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Batch import many `(path, ts)` entries in a single immediate transaction.
    /// Returns the number of newly inserted folder entries.
    pub fn import_dirs_batch(&self, items: &[(String, i64)], tool: Option<&str>) -> Result<usize> {
        if items.is_empty() {
            return Ok(0);
        }
        let tx = self.immediate_tx()?;
        let mut added = 0usize;
        {
            let mut dir_stmt = tx.prepare(
                "INSERT OR IGNORE INTO dirs(path, visits, last_visit) VALUES(?1, 1, ?2)",
            )?;
            let mut tag_stmt = if tool.is_some() {
                Some(tx.prepare(
                    "INSERT OR IGNORE INTO tags(path, tool, count, last_used) VALUES(?1, ?2, 1, ?3)",
                )?)
            } else {
                None
            };
            for (path, ts) in items {
                added += dir_stmt.execute(params![path, ts])?;
                if let (Some(t), Some(ref mut st)) = (tool, &mut tag_stmt) {
                    st.execute(params![path, t, ts])?;
                }
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// List `(path, pinned)` directly from `dirs` without touching `tags`.
    pub fn list_dir_flags(&self) -> Result<Vec<(String, bool)>> {
        let mut stmt = self.conn.prepare("SELECT path, pinned FROM dirs")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Tracked directories that no longer exist on disk (optionally older than `older_than_days`).
    pub fn list_missing(&self, older_than_days: Option<i64>, now: i64) -> Result<Vec<Entry>> {
        let cutoff = older_than_days.map(|d| now.saturating_sub(d.saturating_mul(86400)));
        let mut out = Vec::new();
        let map_row = |r: &rusqlite::Row<'_>| {
            Ok(Entry {
                path: r.get(0)?,
                visits: r.get(1)?,
                last_visit: r.get(2)?,
                pinned: r.get::<_, i64>(3)? != 0,
                tags: Vec::new(),
            })
        };
        if let Some(c) = cutoff {
            let mut stmt = self.conn.prepare(
                "SELECT path, visits, last_visit, pinned FROM dirs WHERE last_visit <= ?1",
            )?;
            let rows = stmt.query_map(params![c], map_row)?;
            for r in rows {
                let e = r?;
                if !Path::new(&e.path).is_dir() {
                    out.push(e);
                }
            }
        } else {
            let mut stmt = self
                .conn
                .prepare("SELECT path, visits, last_visit, pinned FROM dirs")?;
            let rows = stmt.query_map([], map_row)?;
            for r in rows {
                let e = r?;
                if !Path::new(&e.path).is_dir() {
                    out.push(e);
                }
            }
        }
        Ok(out)
    }

    /// Delete multiple folder paths and their tags/sessions in one transaction.
    pub fn delete_paths(&self, paths: &[String]) -> Result<usize> {
        if paths.is_empty() {
            return Ok(0);
        }
        let tx = self.immediate_tx()?;
        let mut n = 0usize;
        {
            let mut del_tags = tx.prepare("DELETE FROM tags WHERE path = ?1")?;
            let mut del_dirs = tx.prepare("DELETE FROM dirs WHERE path = ?1")?;
            let mut del_sess = tx.prepare("DELETE FROM sessions WHERE path = ?1")?;
            for p in paths {
                del_tags.execute(params![p])?;
                n += del_dirs.execute(params![p])?;
                del_sess.execute(params![p])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Collect aggregate stats for `grace stats`.
    pub fn stats(&self, now: i64) -> Result<Stats> {
        let mut entries = self.list_entries()?;
        let total_dirs = entries.len();
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

        // Single pass of `is_dir()` before sorting top dirs.
        entries.retain(|e| Path::new(&e.path).is_dir());
        let existing_dirs = entries.len();
        let missing_dirs = total_dirs.saturating_sub(existing_dirs);
        crate::rank::sort_entries(&mut entries, now);
        entries.truncate(5);
        let (live_sessions, _) = self.inspect_sessions(now).unwrap_or((0, 0));
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

fn has_column(conn: &Connection, table: &str, col: &str) -> Result<bool> {
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
    fn import_does_not_overwrite_and_counts_new_rows() {
        let (_d, db) = temp_db();
        db.record_visit("/i", 100).unwrap();
        assert!(!db.import_dir("/i", 5, Some("claude")).unwrap());
        assert_eq!(find(&db, "/i").unwrap().last_visit, 100);
        assert!(db.import_dir("/j", 5, Some("claude")).unwrap());
        assert_eq!(find(&db, "/j").unwrap().tags.len(), 1);
        let added = db
            .import_dirs_batch(&[("/j".into(), 9), ("/k".into(), 10)], Some("claude"))
            .unwrap();
        assert_eq!(added, 1);
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
        db.session_start("/s", "claude", Some(me), SessionMeta::default(), 100)
            .unwrap();
        assert_eq!(status_of(&db, 101, "/s"), Some(Status::Working));
        assert!(find(&db, "/s").is_some());
        db.session_set("/s", "claude", Status::Waiting, SessionMeta::default(), 110)
            .unwrap();
        assert_eq!(status_of(&db, 111, "/s"), Some(Status::Waiting));
        // A second start reuses the open session.
        db.session_start("/s", "claude", None, SessionMeta::default(), 120)
            .unwrap();
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
    fn concurrent_sessions_by_session_id_and_detail_clearing() {
        let (_d, db) = temp_db();
        db.session_set(
            "/p",
            "claude",
            Status::Working,
            SessionMeta {
                session_id: Some("s1"),
                detail: Some("Bash"),
                tmux_pane: None,
            },
            100,
        )
        .unwrap();
        db.session_set(
            "/p",
            "claude",
            Status::Waiting,
            SessionMeta {
                session_id: Some("s2"),
                detail: Some("permission"),
                tmux_pane: None,
            },
            105,
        )
        .unwrap();
        // Working without detail clears prior Waiting detail.
        db.session_set(
            "/p",
            "claude",
            Status::Working,
            SessionMeta {
                session_id: Some("s2"),
                detail: None,
                tmux_pane: None,
            },
            107,
        )
        .unwrap();
        let all = db.sessions(110).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].session_id.as_deref(), Some("s2"));
        assert_eq!(all[0].detail, None);
        assert_eq!(all[1].session_id.as_deref(), Some("s1"));
        assert_eq!(all[1].detail.as_deref(), Some("Bash"));
    }

    #[test]
    fn done_with_session_id_never_closes_unrelated_session() {
        let (_d, db) = temp_db();
        let me = std::process::id() as i64;
        db.session_start("/p", "claude", Some(me), SessionMeta::default(), 100)
            .unwrap();
        // Ending unknown session "other" must NOT touch the null-session_id session.
        db.session_set(
            "/p",
            "claude",
            Status::Done,
            SessionMeta {
                session_id: Some("other"),
                detail: None,
                tmux_pane: None,
            },
            105,
        )
        .unwrap();
        assert_eq!(status_of(&db, 106, "/p"), Some(Status::Working));
    }

    #[test]
    fn session_set_without_start_and_tag() {
        let (_d, db) = temp_db();
        db.session_set("/h", "codex", Status::Waiting, SessionMeta::default(), 10)
            .unwrap();
        assert_eq!(status_of(&db, 11, "/h"), Some(Status::Waiting));
        assert!(find(&db, "/h").unwrap().has_tag("codex"));
        db.session_set("/h", "codex", Status::Done, SessionMeta::default(), 12)
            .unwrap();
        assert_eq!(status_of(&db, 13, "/h"), Some(Status::Done));
        db.session_set("/none", "codex", Status::Done, SessionMeta::default(), 14)
            .unwrap();
        assert_eq!(status_of(&db, 15, "/none"), None);
    }

    #[cfg(unix)]
    #[test]
    fn dead_pid_is_done() {
        let (_d, db) = temp_db();
        db.session_start(
            "/d",
            "claude",
            Some(999_999_999),
            SessionMeta::default(),
            100,
        )
        .unwrap();
        assert_eq!(status_of(&db, 101, "/d"), Some(Status::Done));
        db.session_start(
            "/old-dead",
            "claude",
            Some(999_999_998),
            SessionMeta::default(),
            100,
        )
        .unwrap();
        assert_eq!(status_of(&db, 3 * 86400, "/old-dead"), None);
    }

    #[test]
    fn stale_and_old_rules() {
        let (_d, db) = temp_db();
        db.session_set("/w", "claude", Status::Working, SessionMeta::default(), 0)
            .unwrap();
        assert_eq!(status_of(&db, 11 * 3600, "/w"), Some(Status::Working));
        assert_eq!(status_of(&db, 12 * 3600 + 1, "/w"), Some(Status::Done));
        assert_eq!(status_of(&db, 12 * 3600 + 33 * 60, "/w"), None);
        db.session_set("/r", "claude", Status::Ready, SessionMeta::default(), 0)
            .unwrap();
        assert_eq!(status_of(&db, 12 * 3600 + 1, "/r"), Some(Status::Done));
        db.session_set(
            "/x",
            "claude",
            Status::Waiting,
            SessionMeta::default(),
            8 * 86400,
        )
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
    fn meta_round_trip_and_integrity_and_readonly() {
        let (d, db) = temp_db();
        let db_path = d.path().join("sub/grace.db");
        assert!(db.integrity_check().unwrap());
        assert_eq!(db.get_meta("k").unwrap(), None);
        db.save_meta_batch(&[("k", "1"), ("k2", "2")]).unwrap();
        assert_eq!(db.get_meta("k").unwrap().as_deref(), Some("1"));
        assert_eq!(db.get_meta("k2").unwrap().as_deref(), Some("2"));
        assert_eq!(db.data_version(), db.data_version());
        let ro = Db::open_readonly(&db_path).unwrap().unwrap();
        assert_eq!(ro.get_meta("k").unwrap().as_deref(), Some("1"));
        assert!(Db::open_readonly(&d.path().join("missing.db"))
            .unwrap()
            .is_none());
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
        assert_eq!(db.normalize_tool_tag("KIRO"), "kiro2");
        assert!(db.remove_tool("kiro").unwrap());
        assert!(!db.remove_tool("kiro").unwrap());
        assert_eq!(db.custom_tools().unwrap().len(), 1);
    }

    #[test]
    fn concurrent_fresh_open_does_not_fail_busy() {
        let d = tempfile::tempdir().unwrap();
        let db_path = std::sync::Arc::new(d.path().join("race/grace.db"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let p = db_path.clone();
                let b = barrier.clone();
                std::thread::spawn(move || {
                    b.wait();
                    let db = Db::open(&p).expect("concurrent fresh Db::open should succeed");
                    db.record_visit(&format!("/proj/{i}"), 1000 + i as i64)
                        .expect("record_visit should succeed");
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let db = Db::open(&db_path).unwrap();
        assert_eq!(db.count_dirs().unwrap(), 8);
    }
}
