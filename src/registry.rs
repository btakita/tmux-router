//! Registry — maps keys to tmux pane IDs.
//!
//! All functions accept an explicit `path` parameter rather than
//! hardcoding any particular registry location.

use anyhow::{Context, Result};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::tmux::Tmux;

const STATE_DB_FILE: &str = "state.db";
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

// Thread-local flag to detect nested lock acquisition (flock is not reentrant).
thread_local! {
    static REGISTRY_LOCK_HELD: Cell<bool> = const { Cell::new(false) };
}

// ---------------------------------------------------------------------------
// Advisory file lock (flock on Unix, LockFileEx on Windows via fs2)
// ---------------------------------------------------------------------------

/// RAII guard for exclusive advisory lock on the registry file.
///
/// Acquire via `RegistryLock::acquire(registry_path)`. The lock file is
/// `<registry_path>.lock` (sibling file). The lock is released when the
/// guard is dropped.
#[derive(Debug)]
pub struct RegistryLock {
    _file: File,
    _path: PathBuf,
}

impl RegistryLock {
    /// Acquire an exclusive advisory lock. Blocks until the lock is available.
    ///
    /// If the current thread already holds a `RegistryLock`, returns `None`
    /// with a warning instead of deadlocking (flock is not reentrant on Linux).
    /// Use `acquire_or_skip()` when the caller can tolerate a no-op.
    pub fn acquire(registry_path: &Path) -> Result<Self> {
        if REGISTRY_LOCK_HELD.get() {
            anyhow::bail!(
                "RegistryLock already held on this thread — would deadlock on {}",
                registry_path.display()
            );
        }
        let lock_path = if is_sqlite_registry_path(registry_path) {
            registry_path.with_extension("db.lock")
        } else {
            registry_path.with_extension("json.lock")
        };
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("failed to open lock file {}", lock_path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("failed to acquire lock on {}", lock_path.display()))?;
        REGISTRY_LOCK_HELD.set(true);
        Ok(Self {
            _file: file,
            _path: lock_path,
        })
    }

    /// Try to acquire the lock; if the current thread already holds it, return
    /// `Ok(None)` with a warning instead of deadlocking. Callers should treat
    /// `None` as "skip the locked operation."
    pub fn acquire_or_skip(registry_path: &Path) -> Result<Option<Self>> {
        if REGISTRY_LOCK_HELD.get() {
            eprintln!(
                "[registry] warning: RegistryLock already held on this thread, skipping nested lock for {}",
                registry_path.display()
            );
            return Ok(None);
        }
        Ok(Some(Self::acquire(registry_path)?))
    }
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        // fs2 releases the lock when the file is closed (on drop),
        // but explicit unlock is cleaner.
        let _ = self._file.unlock();
        REGISTRY_LOCK_HELD.set(false);
    }
}

/// Execute a read-modify-write operation on the registry under an exclusive lock.
///
/// The closure receives a mutable reference to the loaded registry. After the
/// closure returns, the registry is saved back to disk. The lock is held for
/// the entire duration, eliminating TOCTOU races.
pub fn with_registry<F>(path: &Path, f: F) -> Result<()>
where
    F: FnOnce(&mut Registry) -> Result<()>,
{
    let _lock = RegistryLock::acquire(path)?;
    let mut registry = load_registry(path)?;
    f(&mut registry)?;
    save_registry(path, &registry)?;
    Ok(())
}

/// Like `with_registry`, but returns a value from the closure.
pub fn with_registry_val<F, T>(path: &Path, f: F) -> Result<T>
where
    F: FnOnce(&mut Registry) -> Result<T>,
{
    let _lock = RegistryLock::acquire(path)?;
    let mut registry = load_registry(path)?;
    let val = f(&mut registry)?;
    save_registry(path, &registry)?;
    Ok(val)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub pane: String,
    /// Owning agent-doc supervisor PID. Compatibility entries may contain other process IDs.
    pub pid: u32,
    pub cwd: String,
    pub started: String,
    /// Document session UUID. Compatibility registries may use this as the map key.
    #[serde(default)]
    pub session_id: String,
    /// Relative path to the associated file.
    #[serde(default)]
    pub file: String,
    /// Tmux window ID (e.g. `@5`) at claim time.
    #[serde(default)]
    pub window: String,
    /// Stable identity for the long-lived supervisor instance in the pane.
    #[serde(default)]
    pub supervisor_instance_id: String,
}

pub type Registry = HashMap<String, RegistryEntry>;

pub fn sqlite_registry_path_in(base_dir: &Path) -> PathBuf {
    base_dir.join(".agent-doc").join(STATE_DB_FILE)
}

fn is_sqlite_registry_path(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some(STATE_DB_FILE)
        || matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("db" | "sqlite" | "sqlite3")
        )
}

pub fn canonical_registry_key_in(base_dir: &Path, file: &str) -> String {
    let path = Path::new(file);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    };
    canonicalize_or_normalize(&joined)
        .to_string_lossy()
        .to_string()
}

fn canonicalize_or_normalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize_path_components(path))
}

fn normalize_path_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Return the stable session identifier for a registry entry.
///
/// Compatibility entries used the map key as the session ID and left `session_id`
/// empty in the value.
pub fn entry_session_id<'a>(registry_key: &'a str, entry: &'a RegistryEntry) -> &'a str {
    if entry.session_id.is_empty() {
        registry_key
    } else {
        entry.session_id.as_str()
    }
}

fn choose_preferred_entry(left: &RegistryEntry, right: &RegistryEntry) -> bool {
    if right.session_id.is_empty() != left.session_id.is_empty() {
        return !right.session_id.is_empty();
    }
    right.started >= left.started
}

/// Normalize registry keys to canonical file paths and backfill compatibility session IDs.
pub fn normalize_registry(base_dir: &Path, registry: Registry) -> Registry {
    let mut normalized = Registry::new();
    for (registry_key, mut entry) in registry {
        if entry.session_id.is_empty() {
            entry.session_id = registry_key.clone();
        }
        let file_hint = if !entry.file.is_empty() {
            entry.file.clone()
        } else {
            registry_key.clone()
        };
        let normalized_key = canonical_registry_key_in(base_dir, &file_hint);
        if let Some(existing) = normalized.get(&normalized_key)
            && !choose_preferred_entry(existing, &entry)
        {
            continue;
        }
        normalized.insert(normalized_key, entry);
    }
    normalized
}

/// Find the map key for a session ID, including compatibility key-as-session entries.
pub fn find_registry_key_by_session_id(registry: &Registry, session_id: &str) -> Option<String> {
    registry
        .iter()
        .find_map(|(key, entry)| (entry_session_id(key, entry) == session_id).then(|| key.clone()))
}

/// Load the registry from disk. Returns empty map if file doesn't exist.
pub fn load_registry(path: &Path) -> Result<Registry> {
    if is_sqlite_registry_path(path) {
        return load_sqlite_registry(path);
    }
    if !path.exists() {
        return Ok(Registry::new());
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let registry: Registry = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(registry)
}

/// Save the registry to disk.
pub fn save_registry(path: &Path, registry: &Registry) -> Result<()> {
    if is_sqlite_registry_path(path) {
        return save_sqlite_registry(path, registry);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(registry)?;
    std::fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Look up the pane ID for a key in the registry.
pub fn lookup(registry_path: &Path, key: &str) -> Result<Option<String>> {
    if is_sqlite_registry_path(registry_path) {
        return lookup_sqlite_registry(registry_path, key);
    }
    let registry = load_registry(registry_path)?;
    Ok(registry.get(key).map(|e| e.pane.clone()))
}

/// Update the window field for all entries whose pane matches the given pane_id.
/// Called after break_pane or join_pane moves a pane to a different window.
///
/// Acquires an exclusive advisory lock for the duration of the read-modify-write.
pub fn update_window_for_entry(
    registry_path: &Path,
    pane_id: &str,
    new_window: &str,
) -> Result<()> {
    if is_sqlite_registry_path(registry_path) {
        return update_sqlite_window_for_entry(registry_path, pane_id, new_window);
    }
    let _lock = RegistryLock::acquire(registry_path)?;
    let mut registry = load_registry(registry_path)?;
    let mut changed = false;
    for entry in registry.values_mut() {
        if entry.pane == pane_id && entry.window != new_window {
            entry.window = new_window.to_string();
            changed = true;
        }
    }
    if changed {
        save_registry(registry_path, &registry)?;
    }
    Ok(())
}

/// Update non-authoritative registry metadata for a document binding.
///
/// For SQLite-backed registries, actor ownership lives in the `documents` table;
/// this only preserves compatibility fields that older registry consumers expose
/// (`cwd`, relative file spelling, supervisor PID, and supervisor instance ID).
pub fn upsert_entry_metadata(
    registry_path: &Path,
    document_id: &str,
    entry: &RegistryEntry,
) -> Result<()> {
    if is_sqlite_registry_path(registry_path) {
        let conn = open_sqlite_registry(registry_path)?;
        conn.execute(
            r#"
            INSERT INTO registry_entries (
                document_id,
                pane,
                pid,
                cwd,
                started,
                session_id,
                file,
                window,
                supervisor_instance_id
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(document_id) DO UPDATE SET
                pane = excluded.pane,
                pid = excluded.pid,
                cwd = excluded.cwd,
                started = excluded.started,
                session_id = excluded.session_id,
                file = excluded.file,
                window = excluded.window,
                supervisor_instance_id = excluded.supervisor_instance_id
            "#,
            params![
                document_id,
                entry.pane,
                entry.pid,
                entry.cwd,
                entry.started,
                entry.session_id,
                entry.file,
                entry.window,
                entry.supervisor_instance_id
            ],
        )?;
        return Ok(());
    }

    with_registry(registry_path, |registry| {
        registry
            .entry(document_id.to_string())
            .and_modify(|existing| {
                existing.pid = entry.pid;
                existing.cwd = entry.cwd.clone();
                existing.started = entry.started.clone();
                existing.file = entry.file.clone();
                existing.supervisor_instance_id = entry.supervisor_instance_id.clone();
            })
            .or_insert_with(|| entry.clone());
        Ok(())
    })
}

/// Remove entries whose panes are no longer alive.
pub fn prune_dead(registry: &Registry, tmux: &Tmux) -> Registry {
    registry
        .iter()
        .filter(|(_, entry)| tmux.pane_alive(&entry.pane))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Prune dead panes and deduplicate entries from the registry file.
///
/// 1. Removes entries whose tmux panes are no longer alive.
/// 2. Deduplicates entries pointing to the same pane (keeps most recent by `started` timestamp).
/// 3. Saves if anything changed.
///
/// Acquires an exclusive advisory lock for the duration of the read-modify-write.
///
/// Returns the number of entries removed.
pub fn prune(registry_path: &Path, tmux: &Tmux) -> Result<usize> {
    let Some(_lock) = RegistryLock::acquire_or_skip(registry_path)? else {
        eprintln!("[registry] prune skipped — lock already held on this thread");
        return Ok(0);
    };
    let mut registry = load_registry(registry_path)?;
    let before = registry.len();

    // Remove dead panes — single subprocess call instead of N
    let alive = tmux.alive_pane_ids();
    registry.retain(|_key, entry| alive.contains(&entry.pane));
    let dead_removed = before - registry.len();

    // Deduplicate: if multiple keys point to the same pane, keep most recent
    let mut pane_to_keys: std::collections::HashMap<String, Vec<(String, String)>> =
        std::collections::HashMap::new();
    for (key, entry) in &registry {
        pane_to_keys
            .entry(entry.pane.clone())
            .or_default()
            .push((key.clone(), entry.started.clone()));
    }
    let mut dedup_removed = 0usize;
    for (_pane, mut keys) in pane_to_keys {
        if keys.len() <= 1 {
            continue;
        }
        // Sort by started timestamp descending — keep the newest
        keys.sort_by(|a, b| b.1.cmp(&a.1));
        for (key, _) in &keys[1..] {
            registry.remove(key);
            dedup_removed += 1;
        }
    }

    let total = dead_removed + dedup_removed;
    if total > 0 {
        save_registry(registry_path, &registry)?;
    }
    Ok(total)
}

fn timestamp_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn open_sqlite_registry(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn =
        Connection::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    initialize_sqlite_registry(&conn)?;
    Ok(conn)
}

fn initialize_sqlite_registry(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA busy_timeout = 30000;

CREATE TABLE IF NOT EXISTS documents (
    document_id TEXT PRIMARY KEY,
    canonical_path TEXT NOT NULL,
    session_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    pane_id TEXT NOT NULL,
    window_id TEXT NOT NULL,
    harness_id TEXT NOT NULL,
    actor_state TEXT NOT NULL,
    launch_mode TEXT,
    controller_epoch INTEGER,
    last_transition_id INTEGER
);

CREATE TABLE IF NOT EXISTS actor_transitions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    document_id TEXT NOT NULL,
    prior_generation INTEGER NOT NULL,
    new_generation INTEGER NOT NULL,
    caller TEXT NOT NULL,
    reason TEXT NOT NULL,
    old_pane TEXT,
    new_pane TEXT NOT NULL,
    old_window TEXT,
    new_window TEXT,
    timestamp INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS supervisor_leases (
    document_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    supervisor_pid INTEGER,
    supervisor_socket TEXT,
    last_heartbeat INTEGER,
    runtime_state TEXT,
    PRIMARY KEY (document_id, generation)
);

CREATE TABLE IF NOT EXISTS registry_entries (
    document_id TEXT PRIMARY KEY,
    pane TEXT,
    pid INTEGER,
    cwd TEXT,
    started TEXT,
    session_id TEXT,
    file TEXT,
    window TEXT,
    supervisor_instance_id TEXT
);
"#,
    )?;
    ensure_registry_entries_columns(conn)?;
    Ok(())
}

fn ensure_registry_entries_columns(conn: &Connection) -> Result<()> {
    for (column, definition) in [("pane", "TEXT"), ("session_id", "TEXT"), ("window", "TEXT")] {
        if registry_entries_has_column(conn, column)? {
            continue;
        }
        conn.execute(
            &format!("ALTER TABLE registry_entries ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn registry_entries_has_column(conn: &Connection, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(registry_entries)")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get("name")?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn registry_entry_from_sql_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, RegistryEntry)> {
    let document_id: String = row.get("document_id")?;
    let canonical_path: Option<String> = row.get("canonical_path")?;
    let actor_session_id: Option<String> = row.get("actor_session_id")?;
    let actor_pane_id: Option<String> = row.get("actor_pane_id")?;
    let actor_window_id: Option<String> = row.get("actor_window_id")?;
    let metadata_pid: Option<i64> = row.get("metadata_pid")?;
    let pid: Option<i64> = row.get("supervisor_pid")?;
    let timestamp: Option<i64> = row.get("timestamp")?;
    let cwd: Option<String> = row.get("cwd")?;
    let file: Option<String> = row.get("file")?;
    let started: Option<String> = row.get("started")?;
    let metadata_session_id: Option<String> = row.get("metadata_session_id")?;
    let metadata_pane: Option<String> = row.get("metadata_pane")?;
    let metadata_window: Option<String> = row.get("metadata_window")?;
    let supervisor_instance_id: Option<String> = row.get("supervisor_instance_id")?;
    let canonical_path = canonical_path
        .or_else(|| file.clone())
        .unwrap_or_else(|| document_id.clone());
    let fallback_cwd = Path::new(&canonical_path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok((
        document_id,
        RegistryEntry {
            pane: actor_pane_id
                .filter(|pane| !pane.is_empty())
                .or(metadata_pane)
                .unwrap_or_default(),
            pid: metadata_pid
                .or(pid)
                .unwrap_or_default()
                .try_into()
                .unwrap_or_default(),
            cwd: cwd.unwrap_or(fallback_cwd),
            started: started.unwrap_or_else(|| timestamp.unwrap_or_default().to_string()),
            session_id: actor_session_id.or(metadata_session_id).unwrap_or_default(),
            file: file.unwrap_or(canonical_path),
            window: actor_window_id.or(metadata_window).unwrap_or_default(),
            supervisor_instance_id: supervisor_instance_id.unwrap_or_default(),
        },
    ))
}

fn load_sqlite_registry(path: &Path) -> Result<Registry> {
    let conn = open_sqlite_registry(path)?;
    let mut stmt = conn.prepare(
        r#"
        SELECT
            d.document_id,
            d.canonical_path,
            d.session_id AS actor_session_id,
            d.pane_id AS actor_pane_id,
            d.window_id AS actor_window_id,
            s.supervisor_pid,
            t.timestamp,
            m.pid AS metadata_pid,
            m.cwd,
            m.started,
            m.file,
            m.session_id AS metadata_session_id,
            m.pane AS metadata_pane,
            m.window AS metadata_window,
            m.supervisor_instance_id
        FROM documents d
        LEFT JOIN supervisor_leases s
          ON s.document_id = d.document_id
         AND s.generation = d.generation
        LEFT JOIN actor_transitions t
          ON t.id = d.last_transition_id
        LEFT JOIN registry_entries m
          ON m.document_id = d.document_id
        WHERE d.actor_state <> 'closed'
          AND d.pane_id <> ''
        "#,
    )?;
    let mut registry = Registry::new();
    for row in stmt.query_map([], registry_entry_from_sql_row)? {
        let (key, entry) = row?;
        registry.insert(key, entry);
    }
    drop(stmt);

    let mut metadata_stmt = conn.prepare(
        r#"
        SELECT
            m.document_id,
            m.file AS canonical_path,
            NULL AS actor_session_id,
            NULL AS actor_pane_id,
            NULL AS actor_window_id,
            NULL AS supervisor_pid,
            NULL AS timestamp,
            m.pid AS metadata_pid,
            m.cwd,
            m.started,
            m.file,
            m.session_id AS metadata_session_id,
            m.pane AS metadata_pane,
            m.window AS metadata_window,
            m.supervisor_instance_id
        FROM registry_entries m
        LEFT JOIN documents d
          ON d.document_id = m.document_id
        WHERE d.document_id IS NULL
          AND COALESCE(m.pane, '') <> ''
        "#,
    )?;
    for row in metadata_stmt.query_map([], registry_entry_from_sql_row)? {
        let (key, entry) = row?;
        registry.insert(key, entry);
    }
    Ok(registry)
}

fn lookup_sqlite_registry(path: &Path, key: &str) -> Result<Option<String>> {
    let conn = open_sqlite_registry(path)?;
    let actor_pane = conn
        .query_row(
            r#"
        SELECT pane_id
        FROM documents
        WHERE (document_id = ?1 OR canonical_path = ?1 OR session_id = ?1)
          AND actor_state <> 'closed'
          AND pane_id <> ''
        "#,
            params![key],
            |row| row.get(0),
        )
        .optional()
        .with_context(|| format!("failed to look up sqlite actor registry key {key}"))?;
    if actor_pane.is_some() {
        return Ok(actor_pane);
    }

    conn.query_row(
        r#"
        SELECT m.pane
        FROM registry_entries m
        LEFT JOIN documents d
          ON d.document_id = m.document_id
        WHERE d.document_id IS NULL
          AND (m.document_id = ?1 OR m.file = ?1 OR m.session_id = ?1)
          AND COALESCE(m.pane, '') <> ''
        "#,
        params![key],
        |row| row.get(0),
    )
    .optional()
    .with_context(|| format!("failed to look up sqlite registry metadata key {key}"))
}

#[derive(Debug)]
struct ExistingSqliteDocument {
    generation: i64,
    pane_id: String,
    window_id: String,
}

fn load_existing_sqlite_document(
    conn: &Connection,
    document_id: &str,
) -> Result<Option<ExistingSqliteDocument>> {
    conn.query_row(
        r#"
        SELECT generation, pane_id, window_id
        FROM documents
        WHERE document_id = ?1
        "#,
        params![document_id],
        |row| {
            Ok(ExistingSqliteDocument {
                generation: row.get("generation")?,
                pane_id: row.get("pane_id")?,
                window_id: row.get("window_id")?,
            })
        },
    )
    .optional()
    .context("failed to load sqlite registry document")
}

fn insert_sqlite_transition(
    conn: &Connection,
    document_id: &str,
    existing: Option<&ExistingSqliteDocument>,
    new_generation: i64,
    pane_id: &str,
    window_id: &str,
    reason: &str,
) -> Result<i64> {
    let prior_generation = existing.map(|existing| existing.generation).unwrap_or(0);
    let old_pane = existing.map(|existing| existing.pane_id.as_str());
    let old_window = existing.map(|existing| existing.window_id.as_str());
    conn.execute(
        r#"
        INSERT INTO actor_transitions (
            document_id,
            prior_generation,
            new_generation,
            caller,
            reason,
            old_pane,
            new_pane,
            old_window,
            new_window,
            timestamp
        )
        VALUES (?1, ?2, ?3, 'tmux-router', ?4, ?5, ?6, ?7, ?8, ?9)
        "#,
        params![
            document_id,
            prior_generation,
            new_generation,
            reason,
            old_pane,
            pane_id,
            old_window,
            window_id,
            timestamp_secs()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn save_sqlite_registry(path: &Path, registry: &Registry) -> Result<()> {
    let mut conn = open_sqlite_registry(path)?;
    let tx = conn.transaction()?;
    let mut metadata_stmt = tx.prepare("SELECT document_id FROM registry_entries")?;
    let existing_metadata_keys: Vec<String> = metadata_stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(metadata_stmt);
    for document_id in existing_metadata_keys {
        if !registry.contains_key(&document_id) {
            tx.execute(
                "DELETE FROM registry_entries WHERE document_id = ?1",
                params![document_id],
            )?;
        }
    }

    let mut stmt = tx.prepare("SELECT document_id FROM documents")?;
    let existing_keys: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    for document_id in existing_keys {
        if registry.contains_key(&document_id) {
            continue;
        }
        let existing = load_existing_sqlite_document(&tx, &document_id)?;
        let Some(existing) = existing else {
            continue;
        };
        let transition_id = insert_sqlite_transition(
            &tx,
            &document_id,
            Some(&existing),
            existing.generation,
            "",
            "",
            "registry_prune",
        )?;
        tx.execute(
            r#"
            UPDATE documents
            SET actor_state = 'closed',
                pane_id = '',
                window_id = '',
                last_transition_id = ?2
            WHERE document_id = ?1
            "#,
            params![document_id, transition_id],
        )?;
        tx.execute(
            "DELETE FROM registry_entries WHERE document_id = ?1",
            params![document_id],
        )?;
    }

    for (document_id, entry) in registry {
        tx.execute(
            r#"
            INSERT INTO registry_entries (
                document_id,
                pane,
                pid,
                cwd,
                started,
                session_id,
                file,
                window,
                supervisor_instance_id
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(document_id) DO UPDATE SET
                pane = excluded.pane,
                pid = excluded.pid,
                cwd = excluded.cwd,
                started = excluded.started,
                session_id = excluded.session_id,
                file = excluded.file,
                window = excluded.window,
                supervisor_instance_id = excluded.supervisor_instance_id
            "#,
            params![
                document_id,
                entry.pane,
                entry.pid,
                entry.cwd,
                entry.started,
                entry.session_id,
                entry.file,
                entry.window,
                entry.supervisor_instance_id
            ],
        )?;
    }

    tx.commit()?;
    Ok(())
}

fn update_sqlite_window_for_entry(
    registry_path: &Path,
    pane_id: &str,
    new_window: &str,
) -> Result<()> {
    let conn = open_sqlite_registry(registry_path)?;
    conn.execute(
        r#"
        UPDATE documents
        SET window_id = ?2
        WHERE pane_id = ?1
          AND actor_state <> 'closed'
          AND window_id <> ?2
        "#,
        params![pane_id, new_window],
    )?;
    conn.execute(
        r#"
        UPDATE registry_entries
        SET window = ?2
        WHERE pane = ?1
          AND COALESCE(window, '') <> ?2
        "#,
        params![pane_id, new_window],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let reg_path = dir.path().join("registry.json");
        (dir, reg_path)
    }

    fn registry_entry(started: &str, session_id: &str, file: &str) -> RegistryEntry {
        RegistryEntry {
            pane: "%1".to_string(),
            pid: 1234,
            cwd: "/tmp".to_string(),
            started: started.to_string(),
            session_id: session_id.to_string(),
            file: file.to_string(),
            window: "@1".to_string(),
            supervisor_instance_id: String::new(),
        }
    }

    #[test]
    fn canonical_registry_key_normalizes_missing_relative_paths() {
        let dir = TempDir::new().unwrap();

        let key = canonical_registry_key_in(dir.path(), "./nested/../doc.md");

        assert_eq!(key, dir.path().join("doc.md").to_string_lossy());
    }

    #[test]
    fn normalize_registry_uses_file_hint_and_prefers_latest_duplicate() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::new();
        registry.insert(
            "old-session".to_string(),
            registry_entry("2026-01-01T00:00:00Z", "", "nested/../plan.md"),
        );
        registry.insert(
            "modern-key".to_string(),
            registry_entry("2026-01-01T00:01:00Z", "modern-session", "plan.md"),
        );

        let normalized = normalize_registry(dir.path(), registry);
        let key = canonical_registry_key_in(dir.path(), "plan.md");

        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[&key].session_id, "modern-session");
    }

    #[test]
    fn normalize_registry_backfills_compatibility_session_id_from_key() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::new();
        registry.insert(
            "compat-session".to_string(),
            registry_entry("2026-01-01T00:00:00Z", "", ""),
        );

        let normalized = normalize_registry(dir.path(), registry);
        let key = canonical_registry_key_in(dir.path(), "compat-session");

        assert_eq!(normalized[&key].session_id, "compat-session");
    }

    #[test]
    fn find_registry_key_by_session_id_accepts_key_fallback() {
        let mut registry = Registry::new();
        registry.insert(
            "compat-session".to_string(),
            registry_entry("2026-01-01T00:00:00Z", "", ""),
        );

        assert_eq!(
            find_registry_key_by_session_id(&registry, "compat-session").as_deref(),
            Some("compat-session")
        );
    }

    #[test]
    fn acquire_lock_succeeds() {
        let (_dir, reg_path) = setup();
        let lock = RegistryLock::acquire(&reg_path);
        assert!(lock.is_ok());
    }

    #[test]
    fn lock_released_on_drop() {
        let (_dir, reg_path) = setup();
        {
            let _lock = RegistryLock::acquire(&reg_path).unwrap();
        }
        // After drop, a second acquire should succeed
        let lock2 = RegistryLock::acquire(&reg_path);
        assert!(lock2.is_ok());
    }

    #[test]
    fn nested_acquire_returns_error() {
        let (_dir, reg_path) = setup();
        let _lock = RegistryLock::acquire(&reg_path).unwrap();
        let result = RegistryLock::acquire(&reg_path);
        let err = result.err().expect("should fail on nested acquire");
        assert!(
            err.to_string().contains("already held"),
            "error should mention 'already held', got: {}",
            err
        );
    }

    #[test]
    fn acquire_or_skip_returns_none_on_reentrant() {
        let (_dir, reg_path) = setup();
        let _lock = RegistryLock::acquire(&reg_path).unwrap();
        let result = RegistryLock::acquire_or_skip(&reg_path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn acquire_or_skip_returns_some_when_free() {
        let (_dir, reg_path) = setup();
        let result = RegistryLock::acquire_or_skip(&reg_path).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn with_registry_read_modify_write() {
        let (_dir, reg_path) = setup();
        save_registry(&reg_path, &Registry::new()).unwrap();

        with_registry(&reg_path, |reg| {
            reg.insert(
                "test-key".to_string(),
                RegistryEntry {
                    pane: "%1".to_string(),
                    pid: 1234,
                    cwd: "/tmp".to_string(),
                    started: "2026-01-01T00:00:00Z".to_string(),
                    session_id: "session-1".to_string(),
                    file: "test.md".to_string(),
                    window: "@1".to_string(),
                    supervisor_instance_id: String::new(),
                },
            );
            Ok(())
        })
        .unwrap();

        let loaded = load_registry(&reg_path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["test-key"].pane, "%1");
    }

    #[test]
    fn registry_entry_round_trips_session_and_supervisor_fields() {
        let (_dir, reg_path) = setup();
        let mut registry = Registry::new();
        registry.insert(
            "test-key".to_string(),
            RegistryEntry {
                pane: "%7".to_string(),
                pid: 4321,
                cwd: "/workspace".to_string(),
                started: "2026-05-07T00:00:00Z".to_string(),
                session_id: "session-7".to_string(),
                file: "tasks/software/tmux-router.md".to_string(),
                window: "@3".to_string(),
                supervisor_instance_id: "supervisor-7".to_string(),
            },
        );

        save_registry(&reg_path, &registry).unwrap();

        let loaded = load_registry(&reg_path).unwrap();
        let entry = loaded.get("test-key").unwrap();
        assert_eq!(entry.session_id, "session-7");
        assert_eq!(entry.supervisor_instance_id, "supervisor-7");
    }

    #[test]
    fn registry_entry_defaults_optional_session_and_supervisor_fields_for_json() {
        let (_dir, reg_path) = setup();
        std::fs::write(
            &reg_path,
            r#"{
  "compat-key": {
    "pane": "%1",
    "pid": 1234,
    "cwd": "/tmp",
    "started": "2026-01-01T00:00:00Z",
    "file": "compat.md",
    "window": "@1"
  }
}"#,
        )
        .unwrap();

        let loaded = load_registry(&reg_path).unwrap();
        let entry = loaded.get("compat-key").unwrap();
        assert_eq!(entry.session_id, "");
        assert_eq!(entry.supervisor_instance_id, "");
    }

    #[test]
    fn sqlite_registry_round_trips_metadata_entries() {
        let dir = TempDir::new().unwrap();
        let reg_path = dir.path().join("state.db");
        let mut registry = Registry::new();
        registry.insert(
            "doc-1".to_string(),
            RegistryEntry {
                pane: "%7".to_string(),
                pid: 4321,
                cwd: dir.path().to_string_lossy().to_string(),
                started: "2026-05-07T00:00:00Z".to_string(),
                session_id: "session-7".to_string(),
                file: "tasks/doc.md".to_string(),
                window: "@3".to_string(),
                supervisor_instance_id: "supervisor-7".to_string(),
            },
        );

        save_registry(&reg_path, &registry).unwrap();

        let loaded = load_registry(&reg_path).unwrap();
        let entry = loaded.get("doc-1").unwrap();
        assert_eq!(entry.pane, "%7");
        assert_eq!(entry.pid, 4321);
        assert_eq!(entry.session_id, "session-7");
        assert_eq!(entry.window, "@3");
        assert_eq!(entry.supervisor_instance_id, "supervisor-7");
        assert_eq!(
            lookup(&reg_path, "session-7").unwrap().as_deref(),
            Some("%7")
        );
    }

    #[test]
    fn sqlite_registry_update_window_updates_metadata_entry() {
        let dir = TempDir::new().unwrap();
        let reg_path = dir.path().join("state.db");
        let mut registry = Registry::new();
        registry.insert(
            "doc-1".to_string(),
            registry_entry("2026-01-01", "session-1", "doc.md"),
        );
        save_registry(&reg_path, &registry).unwrap();

        update_window_for_entry(&reg_path, "%1", "@9").unwrap();

        let loaded = load_registry(&reg_path).unwrap();
        assert_eq!(loaded["doc-1"].window, "@9");
    }

    #[test]
    fn sqlite_registry_save_does_not_create_actor_documents() {
        let dir = TempDir::new().unwrap();
        let reg_path = dir.path().join("state.db");
        let mut registry = Registry::new();
        registry.insert(
            "doc-1".to_string(),
            registry_entry("2026-01-01", "session-1", "doc.md"),
        );

        save_registry(&reg_path, &registry).unwrap();

        let conn = Connection::open(&reg_path).unwrap();
        let actor_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
            .unwrap();
        let metadata_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM registry_entries", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(actor_rows, 0);
        assert_eq!(metadata_rows, 1);
    }

    #[test]
    fn with_registry_val_returns_value() {
        let (_dir, reg_path) = setup();
        save_registry(&reg_path, &Registry::new()).unwrap();

        let count = with_registry_val(&reg_path, |reg| {
            reg.insert(
                "a".to_string(),
                RegistryEntry {
                    pane: "%1".to_string(),
                    pid: 1,
                    cwd: "/".to_string(),
                    started: "".to_string(),
                    session_id: "session-a".to_string(),
                    file: "".to_string(),
                    window: "".to_string(),
                    supervisor_instance_id: String::new(),
                },
            );
            Ok(reg.len())
        })
        .unwrap();

        assert_eq!(count, 1);
    }

    #[test]
    fn concurrent_with_registry_serializes_writes() {
        let dir = TempDir::new().unwrap();
        let reg_path = dir.path().join("registry.json");
        save_registry(&reg_path, &Registry::new()).unwrap();

        let n = 10;
        let barrier = Arc::new(Barrier::new(n));
        let mut handles = Vec::new();

        for i in 0..n {
            let path = reg_path.clone();
            let bar = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                bar.wait();
                with_registry(&path, |reg| {
                    reg.insert(
                        format!("key-{}", i),
                        RegistryEntry {
                            pane: format!("%{}", i),
                            pid: i as u32,
                            cwd: "/".to_string(),
                            started: "".to_string(),
                            session_id: format!("session-{}", i),
                            file: "".to_string(),
                            window: "".to_string(),
                            supervisor_instance_id: String::new(),
                        },
                    );
                    Ok(())
                })
                .unwrap();
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let final_reg = load_registry(&reg_path).unwrap();
        assert_eq!(final_reg.len(), n);
    }
}
