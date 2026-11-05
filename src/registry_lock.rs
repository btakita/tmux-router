//! Advisory registry lock with a bounded, named wait.
//!
//! [`RegistryLock`] takes an exclusive advisory lock (flock on Unix,
//! `LockFileEx` on Windows, via fs2) on `<registry_path>.lock`. Every
//! acquisition — the caller's and the ones tmux-router takes inside
//! [`with_registry`](crate::registry::with_registry),
//! [`prune`](crate::registry::prune) and the other read-modify-write helpers —
//! waits at most a bound instead of blocking in `flock` forever:
//!
//! - The wait polls a non-blocking lock on the guard's own file descriptor, so
//!   the attempt that succeeds *is* the acquisition: there is no window between
//!   "seen free" and "taken" for another waiter to win.
//! - It gives up after the bound with a [`RegistryLockWaitExceeded`] error that
//!   names the lock file, what the caller was about to do, how long it waited,
//!   and the holder's pid and command line (from `/proc/locks` on Linux).
//! - While it waits it calls the wait policy's heartbeat about once a second,
//!   so a caller can report progress for a lock that is moving.
//!
//! The bound and heartbeat come from a [`RegistryLockWait`]. Callers pass one
//! explicitly ([`RegistryLock::acquire_with`]); everything else, internal
//! acquisitions included, uses the process-wide policy set with
//! [`set_registry_lock_wait`] (default: [`DEFAULT_REGISTRY_LOCK_WAIT`], no
//! heartbeat).

use anyhow::{Context, Result};
use fs2::FileExt;
use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// How long an acquisition waits for a held registry lock by default.
pub const DEFAULT_REGISTRY_LOCK_WAIT: Duration = Duration::from_secs(30);

/// The purpose named by [`RegistryLock::acquire`], which does not take one.
pub const DEFAULT_REGISTRY_LOCK_PURPOSE: &str = "update the registry";

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const FIRST_BACKOFF: Duration = Duration::from_millis(2);
const MAX_BACKOFF: Duration = Duration::from_millis(50);

/// How an acquisition waits for a held lock.
#[derive(Debug, Clone, Copy)]
pub struct RegistryLockWait {
    /// The longest an acquisition waits before failing.
    pub bound: Duration,
    /// Called about once a second while the acquisition waits (and once when
    /// it first finds the lock held). Must not take the registry lock.
    pub heartbeat: Option<fn()>,
}

impl RegistryLockWait {
    /// A wait of `bound` with no heartbeat.
    pub const fn bounded(bound: Duration) -> Self {
        Self {
            bound,
            heartbeat: None,
        }
    }
}

impl Default for RegistryLockWait {
    fn default() -> Self {
        Self::bounded(DEFAULT_REGISTRY_LOCK_WAIT)
    }
}

static PROCESS_WAIT: RwLock<RegistryLockWait> =
    RwLock::new(RegistryLockWait::bounded(DEFAULT_REGISTRY_LOCK_WAIT));

/// Set the wait policy used by every acquisition that does not pass its own,
/// including tmux-router's internal ones.
pub fn set_registry_lock_wait(wait: RegistryLockWait) {
    *PROCESS_WAIT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = wait;
}

/// The process-wide wait policy.
pub fn registry_lock_wait() -> RegistryLockWait {
    *PROCESS_WAIT
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// Thread-local flag to detect nested lock acquisition (flock is not reentrant),
// and a per-thread count of acquisitions so tests can pin how many times a code
// path takes the lock, by count and not by wall clock.
thread_local! {
    static REGISTRY_LOCK_HELD: Cell<bool> = const { Cell::new(false) };
    static ACQUISITIONS_ON_THIS_THREAD: Cell<u64> = const { Cell::new(0) };
}

/// How many registry locks this thread has taken, through any entry point.
pub fn registry_lock_acquisitions_on_this_thread() -> u64 {
    ACQUISITIONS_ON_THIS_THREAD.get()
}

/// Whether this thread currently holds a [`RegistryLock`].
pub fn registry_lock_held_on_this_thread() -> bool {
    REGISTRY_LOCK_HELD.get()
}

/// The lock file a [`RegistryLock`] takes for `registry_path`.
pub fn registry_lock_file_path(registry_path: &Path) -> PathBuf {
    if crate::registry::is_sqlite_registry_path(registry_path) {
        registry_path.with_extension("db.lock")
    } else {
        registry_path.with_extension("json.lock")
    }
}

/// An acquisition gave up waiting for a held lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryLockWaitExceeded {
    pub lock_path: PathBuf,
    pub purpose: String,
    pub waited: Duration,
    /// Who holds the lock: `pid <n> (<command line>)`, comma-separated, or
    /// `unknown (...)` when it cannot be determined.
    pub holders: String,
}

impl std::fmt::Display for RegistryLockWaitExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "registry lock {} is still held after waiting {:.1}s to {}; holder: {}. \
             Retry once that process finishes; if it is stuck, inspect it before stopping it",
            self.lock_path.display(),
            self.waited.as_secs_f64(),
            self.purpose,
            self.holders,
        )
    }
}

impl std::error::Error for RegistryLockWaitExceeded {}

/// RAII guard for the exclusive advisory lock on the registry file.
///
/// The lock file is `<registry_path>.lock` (sibling file,
/// [`registry_lock_file_path`]). The lock is released when the guard is
/// dropped.
#[derive(Debug)]
#[must_use = "the registry lock is held only while the guard is alive"]
pub struct RegistryLock {
    file: File,
    path: PathBuf,
}

impl RegistryLock {
    /// Acquire the lock under the process-wide wait policy
    /// ([`registry_lock_wait`]).
    ///
    /// Fails instead of deadlocking if the current thread already holds a
    /// `RegistryLock` (flock is not reentrant); use [`Self::acquire_or_skip`]
    /// when the caller can tolerate a no-op. Fails with
    /// [`RegistryLockWaitExceeded`] when the lock stays held past the bound.
    pub fn acquire(registry_path: &Path) -> Result<Self> {
        Self::acquire_for(registry_path, DEFAULT_REGISTRY_LOCK_PURPOSE)
    }

    /// [`Self::acquire`] naming what the caller is about to do; `purpose`
    /// appears in the error when the wait gives up ("... waiting 30.0s to
    /// <purpose>").
    pub fn acquire_for(registry_path: &Path, purpose: &str) -> Result<Self> {
        Self::acquire_with(registry_path, purpose, registry_lock_wait())
    }

    /// [`Self::acquire_for`] with an explicit wait policy.
    pub fn acquire_with(
        registry_path: &Path,
        purpose: &str,
        wait: RegistryLockWait,
    ) -> Result<Self> {
        if REGISTRY_LOCK_HELD.get() {
            anyhow::bail!(
                "RegistryLock already held on this thread — would deadlock on {}; refusing a nested acquisition to {purpose}",
                registry_path.display()
            );
        }
        let lock_path = registry_lock_file_path(registry_path);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("failed to open lock file {}", lock_path.display()))?;
        lock_within(&file, &lock_path, purpose, wait)?;
        REGISTRY_LOCK_HELD.set(true);
        ACQUISITIONS_ON_THIS_THREAD.set(ACQUISITIONS_ON_THIS_THREAD.get() + 1);
        Ok(Self {
            file,
            path: lock_path,
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

    /// The lock file this guard holds.
    pub fn lock_path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        // fs2 releases the lock when the file is closed (on drop),
        // but explicit unlock is cleaner.
        let _ = self.file.unlock();
        REGISTRY_LOCK_HELD.set(false);
    }
}

/// Lock `file` exclusively, polling a non-blocking attempt on that same file
/// until it succeeds or `wait.bound` passes. The successful attempt is the
/// acquisition, so no other waiter can take the lock between a probe and it.
fn lock_within(file: &File, lock_path: &Path, purpose: &str, wait: RegistryLockWait) -> Result<()> {
    let started = Instant::now();
    let mut last_heartbeat: Option<Instant> = None;
    let mut backoff = FIRST_BACKOFF;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == fs2::lock_contended_error().kind() => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to acquire lock on {}", lock_path.display()));
            }
        }
        let waited = started.elapsed();
        if waited >= wait.bound {
            return Err(RegistryLockWaitExceeded {
                lock_path: lock_path.to_path_buf(),
                purpose: purpose.to_string(),
                waited,
                holders: describe_holders(lock_path),
            }
            .into());
        }
        if let Some(heartbeat) = wait.heartbeat
            && last_heartbeat.is_none_or(|at| at.elapsed() >= HEARTBEAT_INTERVAL)
        {
            heartbeat();
            last_heartbeat = Some(Instant::now());
        }
        std::thread::sleep(backoff.min(wait.bound.saturating_sub(waited)));
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

fn describe_holders(lock_path: &Path) -> String {
    let pids = lock_file_inode(lock_path)
        .and_then(|inode| {
            std::fs::read_to_string("/proc/locks")
                .ok()
                .map(|locks| flock_holder_pids(&locks, inode))
        })
        .unwrap_or_default();
    if pids.is_empty() {
        return "unknown (no /proc/locks entry)".to_string();
    }
    pids.iter()
        .map(|pid| match process_command_line(*pid) {
            Some(command) => format!("pid {pid} ({command})"),
            None => format!("pid {pid}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(unix)]
fn lock_file_inode(lock_path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(lock_path).ok().map(|meta| meta.ino())
}

#[cfg(not(unix))]
fn lock_file_inode(_lock_path: &Path) -> Option<u64> {
    None
}

/// Pids holding (not waiting for) a `FLOCK` on `inode`, from `/proc/locks`.
///
/// A line is `N: FLOCK ADVISORY WRITE <pid> <maj>:<min>:<inode> 0 EOF`; a
/// blocked waiter's line carries `->` after the ordinal and is skipped.
pub fn flock_holder_pids(proc_locks: &str, inode: u64) -> Vec<u32> {
    let mut pids = proc_locks
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            fields.next()?;
            let kind = fields.next()?;
            if kind != "FLOCK" {
                return None;
            }
            let _advisory = fields.next()?;
            let _mode = fields.next()?;
            let pid = fields.next()?.parse::<u32>().ok()?;
            let file_inode = fields.next()?.rsplit(':').next()?.parse::<u64>().ok()?;
            (file_inode == inode).then_some(pid)
        })
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    pids
}

fn process_command_line(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let command = raw
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    if command.is_empty() {
        return None;
    }
    const LIMIT: usize = 160;
    Some(if command.chars().count() > LIMIT {
        format!("{}…", command.chars().take(LIMIT).collect::<String>())
    } else {
        command
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    fn registry_in(dir: &tempfile::TempDir) -> PathBuf {
        let registry = dir.path().join(".agent-doc/state.db");
        std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
        registry
    }

    /// Hold the lock on another thread (the reentrancy check is per thread)
    /// while `body` runs on this one.
    fn while_held_elsewhere<T>(registry: &Path, body: impl FnOnce() -> T) -> T {
        std::thread::scope(|scope| {
            let (locked_tx, locked_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            scope.spawn(move || {
                let _lock = RegistryLock::acquire(registry).unwrap();
                locked_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            locked_rx.recv().unwrap();
            let value = body();
            release_tx.send(()).unwrap();
            value
        })
    }

    #[test]
    fn lock_file_is_a_sibling_of_the_registry() {
        assert_eq!(
            registry_lock_file_path(Path::new("/p/.agent-doc/state.db")),
            PathBuf::from("/p/.agent-doc/state.db.lock")
        );
        assert_eq!(
            registry_lock_file_path(Path::new("/p/sessions.json")),
            PathBuf::from("/p/sessions.json.lock")
        );
    }

    #[test]
    fn proc_locks_names_holders_not_waiters() {
        let locks = "\
1: FLOCK  ADVISORY  WRITE 4242 103:02:777 0 EOF
1: -> FLOCK  ADVISORY  WRITE 5151 103:02:777 0 EOF
2: POSIX  ADVISORY  WRITE 6000 103:02:777 0 EOF
3: FLOCK  ADVISORY  WRITE 7000 103:02:888 0 EOF
";
        assert_eq!(flock_holder_pids(locks, 777), vec![4242]);
        assert_eq!(flock_holder_pids(locks, 999), Vec::<u32>::new());
    }

    /// A held lock is not waited on forever: the wait gives up at its bound
    /// with a typed error that names the lock, the purpose and this process
    /// as the holder, and nothing is counted as acquired.
    #[test]
    fn a_held_lock_gives_up_at_the_bound_and_names_the_holder() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry_in(&dir);
        let error = while_held_elsewhere(&registry, || {
            let before = registry_lock_acquisitions_on_this_thread();
            let error = RegistryLock::acquire_with(
                &registry,
                "test the bound",
                RegistryLockWait::bounded(Duration::from_millis(50)),
            )
            .unwrap_err();
            assert_eq!(registry_lock_acquisitions_on_this_thread(), before);
            assert!(!registry_lock_held_on_this_thread());
            error
        });
        let exceeded = error
            .downcast_ref::<RegistryLockWaitExceeded>()
            .expect("a typed wait-exceeded error");
        assert_eq!(exceeded.lock_path, registry_lock_file_path(&registry));
        assert_eq!(exceeded.purpose, "test the bound");
        assert!(exceeded.waited >= Duration::from_millis(50));
        let message = error.to_string();
        assert!(message.contains("state.db.lock"), "{message}");
        assert!(message.contains("to test the bound"), "{message}");
        #[cfg(target_os = "linux")]
        assert!(
            message.contains(&format!("pid {}", std::process::id())),
            "the holder is this test process: {message}"
        );

        // Released: the next acquisition succeeds and is counted.
        let before = registry_lock_acquisitions_on_this_thread();
        let lock = RegistryLock::acquire_for(&registry, "after release").unwrap();
        assert_eq!(registry_lock_acquisitions_on_this_thread(), before + 1);
        assert_eq!(lock.lock_path(), registry_lock_file_path(&registry));
        let nested = RegistryLock::acquire_for(&registry, "nest")
            .unwrap_err()
            .to_string();
        assert!(nested.contains("already held on this thread"), "{nested}");
        assert!(nested.contains("to nest"), "{nested}");
    }

    /// The internal acquisitions tmux-router takes (here `with_registry`) are
    /// bounded too, by the process-wide policy.
    #[test]
    fn internal_acquisitions_use_the_process_policy_bound() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("sessions.json");
        // Only this test changes the process policy. Every other test either
        // passes its own policy or takes an uncontended lock, which the bound
        // does not affect.
        set_registry_lock_wait(RegistryLockWait::bounded(Duration::from_millis(30)));
        let error = while_held_elsewhere(&registry, || {
            crate::registry::with_registry(&registry, |_| Ok(())).unwrap_err()
        });
        set_registry_lock_wait(RegistryLockWait::default());
        let exceeded = error
            .downcast_ref::<RegistryLockWaitExceeded>()
            .unwrap_or_else(|| panic!("a typed wait-exceeded error: {error:#}"));
        assert_eq!(exceeded.purpose, DEFAULT_REGISTRY_LOCK_PURPOSE);
    }

    static HEARTBEATS: AtomicUsize = AtomicUsize::new(0);
    fn count_heartbeat() {
        HEARTBEATS.fetch_add(1, Ordering::SeqCst);
    }

    /// A waiter reports a heartbeat while the lock is held, and takes the
    /// lock as soon as the holder lets go, well inside the bound.
    #[test]
    fn a_waiter_heartbeats_and_takes_the_lock_when_released() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry_in(&dir);
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder_registry = registry.clone();
        let holder = std::thread::spawn(move || {
            let _lock = RegistryLock::acquire(&holder_registry).unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        locked_rx.recv().unwrap();
        let waiter_registry = registry.clone();
        let waiter = std::thread::spawn(move || {
            RegistryLock::acquire_with(
                &waiter_registry,
                "wait for release",
                RegistryLockWait {
                    bound: Duration::from_secs(60),
                    heartbeat: Some(count_heartbeat),
                },
            )
            .map(drop)
        });
        // The first heartbeat fires when the waiter first finds the lock held.
        while HEARTBEATS.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        waiter.join().unwrap().unwrap();
    }
}
