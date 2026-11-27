//! SQLite storage.
use crate::model::{now, Entry, Session, Stats, Status, Tag};
use anyhow::{Context, Result};
use rusqlite::{config::DbConfig, params, Connection, OpenFlags, Transaction, TransactionBehavior};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Db {
    pub conn: Connection,
}

const SCHEMA_VER: i64 = 4;

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
    tmux_pane TEXT,
    shell_start INTEGER,
    agent_pid INTEGER,
    agent_start INTEGER
);
CREATE INDEX IF NOT EXISTS sessions_open_path_tool ON sessions(path, tool, id DESC) WHERE status != 'done';
CREATE INDEX IF NOT EXISTS sessions_status_updated ON sessions(status, updated_at);
CREATE INDEX IF NOT EXISTS sessions_open_tool_sid ON sessions(tool, session_id, id) WHERE status != 'done' AND session_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS sessions_open_shell_pid ON sessions(shell_pid) WHERE status != 'done';
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
/// Seconds without update before a working session counts as done.
const WORKING_STALE_AFTER: i64 = 30 * 60;
/// Seconds without update before a waiting or ready session counts as done.
const STALE_AFTER: i64 = 12 * 3600;
/// Seconds before a done row is deleted.
const PRUNE_AFTER: i64 = 7 * 86400;

fn stale_limit(status: Status) -> i64 {
    if status == Status::Working {
        WORKING_STALE_AFTER
    } else {
        STALE_AFTER
    }
}

/// Optional metadata attached to a live session update.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionMeta<'a> {
    pub session_id: Option<&'a str>,
    pub detail: Option<&'a str>,
    pub tmux_pane: Option<&'a str>,
    /// `(pid, start)` of the agent CLI process, from `platform::agent_pin`.
    pub agent: Option<(i64, i64)>,
}

/// What `Db::session_set` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetOutcome {
    /// Status of the matched open row before the call. `None` when no open row matched.
    pub prev: Option<Status>,
    /// A new session row was inserted.
    pub inserted: bool,
    /// The call wrote to the database. `false` for a coalesced repeat or a no-op.
    pub wrote: bool,
}

/// Seconds a repeated identical hook report does not touch the database.
const COALESCE_SECS: i64 = 15;

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
        // Keep the WAL file small: hooks write often, and a reader can hold back checkpoints.
        conn.execute_batch(
            "PRAGMA journal_size_limit = 1048576; PRAGMA wal_autocheckpoint = 256;",
        )?;

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
                // Existing v1.x/v2.0 databases may already have `sessions` without the new
                // columns. Add them before `SCHEMA`, because its indexes use them.
                if has_table(&tx, "sessions")? {
                    for (col, ty) in [
                        ("session_id", "TEXT"),
                        ("detail", "TEXT"),
                        ("tmux_pane", "TEXT"),
                        ("shell_start", "INTEGER"),
                        ("agent_pid", "INTEGER"),
                        ("agent_start", "INTEGER"),
                    ] {
                        if !has_column(&tx, "sessions", col)? {
                            tx.execute(&format!("ALTER TABLE sessions ADD COLUMN {col} {ty}"), [])?;
                        }
                    }
                }
                tx.execute_batch(SCHEMA)?;
            } else if inner_ver < 2 {
                tx.execute_batch(
                    "CREATE INDEX IF NOT EXISTS sessions_open_path_tool ON sessions(path, tool, id DESC) WHERE status != 'done';
                     CREATE INDEX IF NOT EXISTS sessions_updated_at ON sessions(updated_at DESC, id DESC);",
                )?;
            }
            if inner_ver < 3 {
                tx.execute_batch(
                    "CREATE INDEX IF NOT EXISTS sessions_open_tool_sid ON sessions(tool, session_id, id) WHERE status != 'done' AND session_id IS NOT NULL;",
                )?;
            }
            if inner_ver < 4 {
                // v4: process identity for shells and agents, and leaner indexes.
                for col in ["shell_start", "agent_pid", "agent_start"] {
                    if !has_column(&tx, "sessions", col)? {
                        tx.execute(
                            &format!("ALTER TABLE sessions ADD COLUMN {col} INTEGER"),
                            [],
                        )?;
                    }
                }
                tx.execute_batch(
                    "DROP INDEX IF EXISTS sessions_updated_at;
                     DROP INDEX IF EXISTS sessions_path_tool;
                     DROP INDEX IF EXISTS sessions_shell_pid;
                     CREATE INDEX IF NOT EXISTS sessions_open_shell_pid ON sessions(shell_pid) WHERE status != 'done';",
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
        // Read process identity before the write lock.
        let shell_start = shell_pid.and_then(crate::platform::proc_start);
        let (agent_pid, agent_start) = meta.agent.unzip();
        let tx = self.immediate_tx()?;
        tx.execute(UPSERT_DIR_SQL, params![path, ts])?;
        let existing: Option<i64> = if let Some(sid) = sid {
            tx.query_row(
                "SELECT id FROM sessions WHERE tool = ?1 AND session_id = ?2 AND status != 'done'
                 ORDER BY id ASC LIMIT 1",
                params![tool, sid],
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
                // A new shell PID replaces the old start token, even when it is unknown.
                tx.execute(
                    "UPDATE sessions SET status = 'working', updated_at = ?2,
                        shell_pid = COALESCE(?3, shell_pid),
                        shell_start = CASE WHEN ?3 IS NOT NULL THEN ?4 ELSE shell_start END,
                        session_id = COALESCE(?5, session_id),
                        detail = ?6,
                        tmux_pane = COALESCE(?7, tmux_pane),
                        agent_pid = COALESCE(?8, agent_pid),
                        agent_start = COALESCE(?9, agent_start)
                     WHERE id = ?1",
                    params![
                        id,
                        ts,
                        shell_pid,
                        shell_start,
                        sid,
                        meta.detail,
                        meta.tmux_pane,
                        agent_pid,
                        agent_start
                    ],
                )?;
            }
            None => {
                tx.execute(
                    "INSERT INTO sessions(path, tool, status, shell_pid, started_at, updated_at, session_id, detail, tmux_pane, shell_start, agent_pid, agent_start)
                     VALUES(?1, ?2, 'working', ?3, ?4, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        path,
                        tool,
                        shell_pid,
                        ts,
                        sid,
                        meta.detail,
                        meta.tmux_pane,
                        shell_start,
                        agent_pid,
                        agent_start
                    ],
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
    ///
    /// A repeat of the same status and metadata within 15 s of the last write
    /// does not write. An `IMMEDIATE` transaction with no writes adds no WAL
    /// frames and does not bump `PRAGMA data_version`, so watchers stay idle.
    pub fn session_set(
        &self,
        path: &str,
        tool: &str,
        status: Status,
        meta: SessionMeta<'_>,
        ts: i64,
    ) -> Result<SetOutcome> {
        let sid = meta.session_id.filter(|s| !s.is_empty());
        let (agent_pid, agent_start) = meta.agent.unzip();
        let tx = self.immediate_tx()?;
        if let Some(sid) = sid {
            if status == Status::Done {
                let prev: Option<Status> = tx
                    .query_row(
                        "SELECT status FROM sessions
                         WHERE tool = ?1 AND session_id = ?2 AND status != 'done'
                         ORDER BY id DESC LIMIT 1",
                        params![tool, sid],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
                    .and_then(|s| Status::parse(&s));
                let n = tx.execute(
                    "UPDATE sessions SET status = 'done', updated_at = ?3, detail = NULL
                     WHERE tool = ?1 AND session_id = ?2 AND status != 'done'",
                    params![tool, sid, ts],
                )?;
                if n > 0 || (ts & 0xff) == 0 {
                    Self::prune_sessions_tx(&tx, ts)?;
                }
                tx.commit()?;
                return Ok(SetOutcome {
                    prev,
                    inserted: false,
                    wrote: n > 0,
                });
            }
        }
        let target_id: Option<i64> = if let Some(sid) = sid {
            tx.query_row(
                "SELECT id FROM sessions
                 WHERE tool = ?1 AND session_id = ?2 AND status != 'done'
                 ORDER BY id ASC LIMIT 1",
                params![tool, sid],
                |r| r.get(0),
            )
            .ok()
            .or_else(|| {
                tx.query_row(
                    "SELECT id FROM sessions
                     WHERE path = ?1 AND tool = ?2 AND status != 'done' AND session_id IS NULL
                     ORDER BY id DESC LIMIT 1",
                    params![path, tool],
                    |r| r.get(0),
                )
                .ok()
            })
        } else {
            tx.query_row(
                "SELECT id FROM sessions WHERE path = ?1 AND tool = ?2 AND status != 'done'
                 ORDER BY id DESC LIMIT 1",
                params![path, tool],
                |r| r.get(0),
            )
            .ok()
        };
        if let Some(id) = target_id {
            type Cur = (
                String,
                i64,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<i64>,
                Option<i64>,
            );
            let cur: Option<Cur> = tx
                .query_row(
                    "SELECT status, updated_at, session_id, detail, tmux_pane, agent_pid, agent_start
                     FROM sessions WHERE id = ?1",
                    params![id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                        ))
                    },
                )
                .ok();
            if let Some((
                cur_status,
                cur_updated,
                cur_sid,
                cur_detail,
                cur_pane,
                cur_apid,
                cur_astart,
            )) = cur
            {
                let prev = Status::parse(&cur_status);
                if status == Status::Done {
                    let n = tx.execute(
                        "UPDATE sessions SET status = 'done', updated_at = ?2, detail = NULL
                         WHERE id = ?1",
                        params![id, ts],
                    )?;
                    Self::prune_sessions_tx(&tx, ts)?;
                    tx.commit()?;
                    return Ok(SetOutcome {
                        prev,
                        inserted: false,
                        wrote: n > 0,
                    });
                }
                let same_status = prev == Some(status);
                let same_detail = cur_detail.as_deref() == meta.detail;
                let same_sid = sid.is_none() || cur_sid.as_deref() == sid;
                let same_pane = meta.tmux_pane.is_none() || cur_pane.as_deref() == meta.tmux_pane;
                let same_agent = match meta.agent {
                    None => true,
                    Some((ap, ast)) => cur_apid == Some(ap) && cur_astart == Some(ast),
                };
                let same_identity = same_status && same_sid && same_pane && same_agent;
                if same_identity && same_detail && cur_updated >= ts.saturating_sub(COALESCE_SECS) {
                    // Nothing new: end the transaction without a write.
                    tx.commit()?;
                    return Ok(SetOutcome {
                        prev,
                        inserted: false,
                        wrote: false,
                    });
                }
                if same_identity {
                    tx.execute(
                        "UPDATE sessions SET updated_at = ?2, detail = ?3 WHERE id = ?1",
                        params![id, ts, meta.detail],
                    )?;
                } else {
                    // COALESCE(new, old): a new value always replaces the old one.
                    tx.execute(
                        "UPDATE sessions SET status = ?2, updated_at = ?3,
                            session_id = COALESCE(?4, session_id),
                            detail = ?5,
                            tmux_pane = COALESCE(?6, tmux_pane),
                            agent_pid = COALESCE(?7, agent_pid),
                            agent_start = COALESCE(?8, agent_start)
                         WHERE id = ?1",
                        params![
                            id,
                            status.as_str(),
                            ts,
                            sid,
                            meta.detail,
                            meta.tmux_pane,
                            agent_pid,
                            agent_start
                        ],
                    )?;
                }
                if (ts & 0xff) == 0 {
                    Self::prune_sessions_tx(&tx, ts)?;
                }
                tx.commit()?;
                return Ok(SetOutcome {
                    prev,
                    inserted: false,
                    wrote: true,
                });
            }
        }
        if status == Status::Done {
            // No open row to close. The transaction wrote nothing.
            tx.commit()?;
            return Ok(SetOutcome {
                prev: None,
                inserted: false,
                wrote: false,
            });
        }
        tx.execute(UPSERT_DIR_SQL, params![path, ts])?;
        tx.execute(UPSERT_TAG_SQL, params![path, tool, ts])?;
        tx.execute(
            "INSERT INTO sessions(path, tool, status, shell_pid, started_at, updated_at, session_id, detail, tmux_pane, shell_start, agent_pid, agent_start)
             VALUES(?1, ?2, ?3, NULL, ?4, ?4, ?5, ?6, ?7, NULL, ?8, ?9)",
            params![
                path,
                tool,
                status.as_str(),
                ts,
                sid,
                meta.detail,
                meta.tmux_pane,
                agent_pid,
                agent_start
            ],
        )?;
        if (ts & 0xff) == 0 {
            Self::prune_sessions_tx(&tx, ts)?;
        }
        tx.commit()?;
        Ok(SetOutcome {
            prev: None,
            inserted: true,
            wrote: true,
        })
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
            "SELECT id, path, tool, status, shell_pid, started_at, updated_at, session_id, detail, tmux_pane,
                    shell_start, agent_pid, agent_start
             FROM sessions WHERE status != 'done'
             UNION ALL
             SELECT id, path, tool, status, shell_pid, started_at, updated_at, session_id, detail, tmux_pane,
                    shell_start, agent_pid, agent_start
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
                shell_start: r.get(10)?,
                agent_pid: r.get(11)?,
                agent_start: r.get(12)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Read-only inspection of `(live_active_count, dead_or_stale_count)` for `grace doctor`.
    pub fn inspect_sessions(&self, now: i64) -> Result<(usize, usize)> {
        let mut stmt = self.conn.prepare(
            "SELECT status, shell_pid, updated_at, shell_start, agent_pid, agent_start
             FROM sessions WHERE status != 'done'",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<i64>>(5)?,
            ))
        })?;
        let (mut active, mut stale_or_dead) = (0usize, 0usize);
        for row in rows {
            let (st_s, pid, updated, shell_start, agent_pid, agent_start) = row?;
            let st = Status::parse(&st_s).unwrap_or(Status::Done);
            let dead = procs_dead(pid, shell_start, agent_pid, agent_start);
            let stale = now.saturating_sub(updated) > stale_limit(st);
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
                let dead = procs_dead(s.shell_pid, s.shell_start, s.agent_pid, s.agent_start);
                let limit = stale_limit(s.status);
                let stale = now.saturating_sub(s.updated_at) > limit;
                if dead {
                    Some((s.id, s.updated_at, s.updated_at))
                } else if stale {
                    Some((
                        s.id,
                        s.updated_at.saturating_add(limit).min(now),
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

    /// Move all WAL content into the main file and truncate the WAL to 0 bytes.
    pub fn wal_checkpoint_truncate(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Earliest unix time at which one of `sessions` changes on its own:
    /// a done row leaves the list, or an open row goes stale.
    /// `i64::MAX` when the list is empty.
    pub fn next_deadline(sessions: &[Session]) -> i64 {
        sessions
            .iter()
            .map(|s| {
                if s.status == Status::Done {
                    s.updated_at.saturating_add(DONE_VISIBLE)
                } else {
                    s.updated_at.saturating_add(stale_limit(s.status))
                }
            })
            .min()
            .unwrap_or(i64::MAX)
    }

    /// Cheap fingerprint of `dirs` and `tags`:
    /// `(dir count, max last_visit, total visits, pinned count, tag count, max last_used)`.
    /// A change means `list_entries` can return something new.
    pub fn entries_signature(&self) -> Result<(i64, i64, i64, i64, i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT
                (SELECT COUNT(*) FROM dirs),
                (SELECT COALESCE(MAX(last_visit), 0) FROM dirs),
                (SELECT CAST(TOTAL(visits) AS INTEGER) FROM dirs),
                (SELECT CAST(TOTAL(pinned) AS INTEGER) FROM dirs),
                (SELECT COUNT(*) FROM tags),
                (SELECT COALESCE(MAX(last_used), 0) FROM tags)",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )?)
    }
}

/// True when the session's shell or pinned agent process is gone
/// (or its PID now belongs to a different process).
fn procs_dead(
    shell_pid: Option<i64>,
    shell_start: Option<i64>,
    agent_pid: Option<i64>,
    agent_start: Option<i64>,
) -> bool {
    let shell_dead = shell_pid.is_some_and(|p| !crate::platform::pid_alive_at(p, shell_start));
    let agent_dead = agent_pid.is_some_and(|p| !crate::platform::pid_alive_at(p, agent_start));
    shell_dead || agent_dead
}

fn has_table(conn: &Connection, table: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
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
                agent: None,
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
                agent: None,
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
                agent: None,
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
                agent: None,
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
        assert_eq!(status_of(&db, 29 * 60, "/w"), Some(Status::Working));
        assert_eq!(status_of(&db, 30 * 60 + 1, "/w"), Some(Status::Done));
        assert_eq!(status_of(&db, 30 * 60 + 33 * 60, "/w"), None);
        db.session_set("/r", "claude", Status::Ready, SessionMeta::default(), 0)
            .unwrap();
        assert_eq!(status_of(&db, 11 * 3600, "/r"), Some(Status::Ready));
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
    fn session_id_tracks_across_subdir_cwd_and_closes_all_on_done() {
        let (_d, db) = temp_db();
        db.session_set(
            "/repo",
            "claude",
            Status::Ready,
            SessionMeta {
                session_id: Some("s1"),
                detail: None,
                tmux_pane: None,
                agent: None,
            },
            100,
        )
        .unwrap();
        db.session_set(
            "/repo/ui/src",
            "claude",
            Status::Working,
            SessionMeta {
                session_id: Some("s1"),
                detail: Some("Bash"),
                tmux_pane: None,
                agent: None,
            },
            110,
        )
        .unwrap();
        let all = db.sessions(115).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].path, "/repo");
        assert_eq!(all[0].status, Status::Working);
        assert_eq!(all[0].detail.as_deref(), Some("Bash"));
        assert!(find(&db, "/repo/ui/src").is_none());

        // Legacy orphan row with same (tool, session_id) + unrelated session s2.
        db.conn
            .execute(
                "INSERT INTO sessions(path, tool, status, shell_pid, started_at, updated_at, session_id)
                 VALUES('/repo/sub', 'claude', 'working', NULL, 105, 112, 's1')",
                [],
            )
            .unwrap();
        db.session_set(
            "/repo",
            "claude",
            Status::Ready,
            SessionMeta {
                session_id: Some("s2"),
                detail: None,
                tmux_pane: None,
                agent: None,
            },
            120,
        )
        .unwrap();
        db.session_set(
            "/repo",
            "claude",
            Status::Done,
            SessionMeta {
                session_id: Some("s1"),
                detail: None,
                tmux_pane: None,
                agent: None,
            },
            130,
        )
        .unwrap();
        let after = db.sessions(135).unwrap();
        let s1_rows: Vec<_> = after
            .iter()
            .filter(|s| s.session_id.as_deref() == Some("s1"))
            .collect();
        assert_eq!(s1_rows.len(), 2);
        assert!(s1_rows.iter().all(|s| s.status == Status::Done));
        let s2_row = after
            .iter()
            .find(|s| s.session_id.as_deref() == Some("s2"))
            .unwrap();
        assert_eq!(s2_row.status, Status::Ready);
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

    fn index_names(db: &Db) -> Vec<String> {
        let mut stmt = db
            .conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'sessions'",
            )
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    fn pin_of(db: &Db, id: i64) -> (Option<i64>, Option<i64>) {
        db.conn
            .query_row(
                "SELECT agent_pid, agent_start FROM sessions WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    fn meta<'a>(
        sid: Option<&'a str>,
        detail: Option<&'a str>,
        agent: Option<(i64, i64)>,
    ) -> SessionMeta<'a> {
        SessionMeta {
            session_id: sid,
            detail,
            tmux_pane: None,
            agent,
        }
    }

    #[test]
    fn migrates_v3_to_v4() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("grace.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE dirs(path TEXT PRIMARY KEY, visits INTEGER NOT NULL DEFAULT 0,
                     last_visit INTEGER NOT NULL DEFAULT 0, pinned INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE tags(path TEXT NOT NULL, tool TEXT NOT NULL, count INTEGER NOT NULL DEFAULT 0,
                     last_used INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(path, tool));
                 CREATE TABLE tools(command TEXT PRIMARY KEY, tag TEXT NOT NULL, launch TEXT NOT NULL);
                 CREATE TABLE sessions(id INTEGER PRIMARY KEY, path TEXT NOT NULL, tool TEXT NOT NULL,
                     status TEXT NOT NULL, shell_pid INTEGER, started_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL, session_id TEXT, detail TEXT, tmux_pane TEXT);
                 CREATE INDEX sessions_path_tool ON sessions(path, tool);
                 CREATE INDEX sessions_open_path_tool ON sessions(path, tool, id DESC) WHERE status != 'done';
                 CREATE INDEX sessions_shell_pid ON sessions(shell_pid);
                 CREATE INDEX sessions_status_updated ON sessions(status, updated_at);
                 CREATE INDEX sessions_updated_at ON sessions(updated_at DESC, id DESC);
                 CREATE INDEX sessions_open_tool_sid ON sessions(tool, session_id, id)
                     WHERE status != 'done' AND session_id IS NOT NULL;
                 CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO sessions(path, tool, status, shell_pid, started_at, updated_at, session_id)
                     VALUES('/old', 'claude', 'ready', NULL, 10, 10, 's1');
                 PRAGMA user_version = 3;",
            )
            .unwrap();
        }
        let db = Db::open(&p).unwrap();
        let ver: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ver, 4);
        for col in ["shell_start", "agent_pid", "agent_start"] {
            assert!(has_column(&db.conn, "sessions", col).unwrap(), "{col}");
        }
        let idx = index_names(&db);
        for gone in [
            "sessions_updated_at",
            "sessions_path_tool",
            "sessions_shell_pid",
        ] {
            assert!(!idx.iter().any(|n| n == gone), "{gone} still exists");
        }
        for kept in [
            "sessions_open_shell_pid",
            "sessions_open_path_tool",
            "sessions_status_updated",
            "sessions_open_tool_sid",
        ] {
            assert!(idx.iter().any(|n| n == kept), "{kept} missing");
        }
        // The old row survives and reads back with empty identity columns.
        let all = db.sessions(20).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(
            (all[0].shell_start, all[0].agent_pid, all[0].agent_start),
            (None, None, None)
        );
        // A second open is a no-op.
        drop(db);
        let db = Db::open(&p).unwrap();
        assert_eq!(index_names(&db).len(), idx.len());
    }

    /// A pre-v1 database whose `sessions` table lacks the newer columns.
    #[test]
    fn migrates_pre_v1_sessions_table() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("grace.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(
                "CREATE TABLE sessions(id INTEGER PRIMARY KEY, path TEXT NOT NULL, tool TEXT NOT NULL,
                     status TEXT NOT NULL, shell_pid INTEGER, started_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL);",
            )
            .unwrap();
        }
        let db = Db::open(&p).unwrap();
        for col in [
            "session_id",
            "detail",
            "tmux_pane",
            "shell_start",
            "agent_pid",
            "agent_start",
        ] {
            assert!(has_column(&db.conn, "sessions", col).unwrap(), "{col}");
        }
        assert!(index_names(&db)
            .iter()
            .any(|n| n == "sessions_open_tool_sid"));
    }

    #[test]
    fn wal_pragmas_on_every_open() {
        let (d, db) = temp_db();
        let get = |db: &Db, pragma: &str| -> i64 {
            db.conn
                .query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(get(&db, "journal_size_limit"), 1_048_576);
        assert_eq!(get(&db, "wal_autocheckpoint"), 256);
        db.record_visit("/w", 1).unwrap();
        db.wal_checkpoint_truncate().unwrap();
        drop(db);
        // Reopen an up-to-date database: the pragmas still apply.
        let db = Db::open(&d.path().join("sub/grace.db")).unwrap();
        assert_eq!(get(&db, "journal_size_limit"), 1_048_576);
        assert_eq!(get(&db, "wal_autocheckpoint"), 256);
        let mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
    }

    #[test]
    fn dead_agent_pin_closes_session_and_new_pin_overwrites() {
        let (_d, db) = temp_db();
        let out = db
            .session_set(
                "/a",
                "claude",
                Status::Working,
                meta(Some("s1"), None, Some((999_999_999, 1))),
                100,
            )
            .unwrap();
        assert_eq!(
            out,
            SetOutcome {
                prev: None,
                inserted: true,
                wrote: true
            }
        );
        let s = &db.sessions(101).unwrap()[0];
        assert_eq!((s.agent_pid, s.agent_start), (Some(999_999_999), Some(1)));
        assert_eq!(s.status, Status::Done);
        // A dead pin closes at its last update, not "now".
        assert_eq!(s.updated_at, 100);

        // Live pin: this test process.
        let me = std::process::id() as i64;
        let Some(my_start) = crate::platform::proc_start(me) else {
            return;
        };
        db.session_set(
            "/b",
            "claude",
            Status::Working,
            meta(Some("s2"), None, Some((999_999_998, 7))),
            200,
        )
        .unwrap();
        // Read the id without `sessions()`, which would close the dead pin.
        let id: i64 = db
            .conn
            .query_row("SELECT id FROM sessions WHERE path = '/b'", [], |r| {
                r.get(0)
            })
            .unwrap();
        // Resume before the read: the new pin replaces the old one.
        let out = db
            .session_set(
                "/b",
                "claude",
                Status::Working,
                meta(Some("s2"), None, Some((me, my_start))),
                201,
            )
            .unwrap();
        assert_eq!(
            out,
            SetOutcome {
                prev: Some(Status::Working),
                inserted: false,
                wrote: true
            }
        );
        assert_eq!(pin_of(&db, id), (Some(me), Some(my_start)));
        // A report without a pin keeps the pin.
        db.session_set(
            "/b",
            "claude",
            Status::Ready,
            meta(Some("s2"), None, None),
            202,
        )
        .unwrap();
        assert_eq!(pin_of(&db, id), (Some(me), Some(my_start)));
        assert_eq!(status_of(&db, 203, "/b"), Some(Status::Ready));
        // Same PID with a different start time: a reused PID, so the session closes.
        db.conn
            .execute(
                "UPDATE sessions SET agent_start = ?2 WHERE id = ?1",
                params![id, my_start + 1],
            )
            .unwrap();
        assert_eq!(status_of(&db, 204, "/b"), Some(Status::Done));
    }

    #[test]
    fn shell_start_recorded_and_reused_shell_pid_is_dead() {
        let (_d, db) = temp_db();
        let me = std::process::id() as i64;
        let Some(my_start) = crate::platform::proc_start(me) else {
            return;
        };
        let m = meta(Some("x"), None, None);
        db.session_start("/s", "claude", Some(me), m, 100).unwrap();
        // A restart without a shell PID keeps the shell identity.
        db.session_start("/s", "claude", None, m, 110).unwrap();
        let s = &db.sessions(111).unwrap()[0];
        assert_eq!((s.shell_pid, s.shell_start), (Some(me), Some(my_start)));
        assert_eq!(s.status, Status::Working);
        // A restart with a dead shell PID replaces the start token.
        db.session_start("/s", "claude", Some(999_999_999), m, 120)
            .unwrap();
        let (n, pid, start): (i64, Option<i64>, Option<i64>) = db
            .conn
            .query_row(
                "SELECT COUNT(*), MAX(shell_pid), MAX(shell_start) FROM sessions WHERE path = '/s'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!((pid, start), (Some(999_999_999), None));

        // Our PID with a wrong start time counts as dead.
        db.session_start("/t", "claude", Some(me), SessionMeta::default(), 130)
            .unwrap();
        db.conn
            .execute(
                "UPDATE sessions SET shell_start = shell_start + 1 WHERE path = '/t'",
                [],
            )
            .unwrap();
        assert_eq!(status_of(&db, 131, "/t"), Some(Status::Done));
    }

    #[test]
    fn repeated_hook_reports_coalesce_for_15s() {
        let (_d, db) = temp_db();
        let set = |status: Status, detail: Option<&str>, ts: i64| {
            db.session_set("/c", "codex", status, meta(Some("s1"), detail, None), ts)
                .unwrap()
        };
        let out = |prev, inserted, wrote| SetOutcome {
            prev,
            inserted,
            wrote,
        };
        assert_eq!(
            set(Status::Working, Some("Bash"), 100),
            out(None, true, true)
        );
        // Same report within 15 s: no write.
        assert_eq!(
            set(Status::Working, Some("Bash"), 110),
            out(Some(Status::Working), false, false)
        );
        assert_eq!(
            set(Status::Working, Some("Bash"), 115),
            out(Some(Status::Working), false, false)
        );
        let s = &db.sessions(115).unwrap()[0];
        assert_eq!(s.updated_at, 100);
        // A new detail writes.
        assert_eq!(
            set(Status::Working, Some("Edit"), 116),
            out(Some(Status::Working), false, true)
        );
        // The same report after 15 s refreshes `updated_at`.
        assert_eq!(
            set(Status::Working, Some("Edit"), 132),
            out(Some(Status::Working), false, true)
        );
        let s = &db.sessions(132).unwrap()[0];
        assert_eq!((s.updated_at, s.detail.as_deref()), (132, Some("Edit")));
        // A new status writes.
        assert_eq!(
            set(Status::Waiting, Some("permission"), 133),
            out(Some(Status::Working), false, true)
        );
        // Done with a session id closes the row and reports the old status.
        assert_eq!(
            set(Status::Done, None, 134),
            out(Some(Status::Waiting), false, true)
        );
        // Nothing left to close.
        assert_eq!(set(Status::Done, None, 135), out(None, false, false));
        assert_eq!(
            db.session_set("/none", "codex", Status::Done, SessionMeta::default(), 136)
                .unwrap(),
            out(None, false, false)
        );
    }

    /// A coalesced report from another connection does not bump `data_version`.
    #[test]
    fn coalesced_report_leaves_data_version() {
        let (d, db) = temp_db();
        let other = Db::open(&d.path().join("sub/grace.db")).unwrap();
        let m = meta(Some("s1"), Some("Bash"), None);
        other
            .session_set("/v", "claude", Status::Working, m, 100)
            .unwrap();
        let v1 = db.data_version();
        let out = other
            .session_set("/v", "claude", Status::Working, m, 105)
            .unwrap();
        assert!(!out.wrote);
        assert_eq!(db.data_version(), v1);
        other
            .session_set("/v", "claude", Status::Ready, m, 106)
            .unwrap();
        assert_ne!(db.data_version(), v1);
    }

    fn sess(status: Status, updated_at: i64) -> Session {
        Session {
            id: 1,
            path: "/x".into(),
            tool: "claude".into(),
            status,
            shell_pid: None,
            started_at: 0,
            updated_at,
            session_id: None,
            detail: None,
            tmux_pane: None,
            shell_start: None,
            agent_pid: None,
            agent_start: None,
        }
    }

    #[test]
    fn next_deadline_picks_earliest_change() {
        assert_eq!(Db::next_deadline(&[]), i64::MAX);
        assert_eq!(
            Db::next_deadline(&[sess(Status::Working, 100)]),
            100 + WORKING_STALE_AFTER
        );
        assert_eq!(
            Db::next_deadline(&[sess(Status::Ready, 100), sess(Status::Waiting, 50)]),
            50 + STALE_AFTER
        );
        assert_eq!(
            Db::next_deadline(&[sess(Status::Ready, 100), sess(Status::Done, 90)]),
            90 + DONE_VISIBLE
        );
        assert_eq!(Db::next_deadline(&[sess(Status::Done, i64::MAX)]), i64::MAX);
    }

    #[test]
    fn entries_signature_tracks_dirs_and_tags() {
        let (_d, db) = temp_db();
        assert_eq!(db.entries_signature().unwrap(), (0, 0, 0, 0, 0, 0));
        db.record_visit("/a", 10).unwrap();
        assert_eq!(db.entries_signature().unwrap(), (1, 10, 1, 0, 0, 0));
        db.record_visit("/a", 20).unwrap();
        assert_eq!(db.entries_signature().unwrap(), (1, 20, 2, 0, 0, 0));
        db.set_pinned("/a", true).unwrap();
        assert_eq!(db.entries_signature().unwrap().3, 1);
        db.record_tool("/b", "claude", 30).unwrap();
        assert_eq!(db.entries_signature().unwrap(), (2, 30, 3, 1, 1, 30));
        let before = db.entries_signature().unwrap();
        db.set_meta("k", "v").unwrap();
        assert_eq!(db.entries_signature().unwrap(), before);
    }
}
