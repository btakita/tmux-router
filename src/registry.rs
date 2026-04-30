//! Registry — maps keys to tmux pane IDs.
//!
//! All functions accept an explicit `path` parameter rather than
//! hardcoding any particular registry location.

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::tmux::Tmux;

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
        let lock_path = registry_path.with_extension("json.lock");
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
    /// Owning agent-doc supervisor PID. Legacy entries may contain other process IDs.
    pub pid: u32,
    pub cwd: String,
    pub started: String,
    /// Document session UUID. Legacy registries used this as the map key.
    #[serde(default)]
    pub session_id: String,
    /// Relative path to the associated file (empty for legacy entries).
    #[serde(default)]
    pub file: String,
    /// Tmux window ID (e.g. `@5`) at claim time. Empty for legacy entries.
    #[serde(default)]
    pub window: String,
    /// Stable identity for the long-lived supervisor instance in the pane.
    #[serde(default)]
    pub supervisor_instance_id: String,
}

pub type Registry = HashMap<String, RegistryEntry>;

/// Load the registry from disk. Returns empty map if file doesn't exist.
pub fn load_registry(path: &Path) -> Result<Registry> {
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
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(registry)?;
    std::fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Look up the pane ID for a key in the registry.
pub fn lookup(registry_path: &Path, key: &str) -> Result<Option<String>> {
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
