# Registry

The registry maps string keys to tmux pane IDs. It's stored as a JSON file on disk.

## Entry Structure

Each entry tracks:

| Field | Type | Description |
|-------|------|-------------|
| `pane` | String | Tmux pane ID (e.g. `%42`) |
| `pid` | u32 | Foreground process PID |
| `cwd` | String | Working directory at registration |
| `started` | String | UTC timestamp |
| `session_id` | String | Document session UUID; defaults to empty for legacy entries |
| `file` | String | Associated file path |
| `window` | String | Tmux window ID (e.g. `@5`) |
| `supervisor_instance_id` | String | Stable long-lived supervisor identity; defaults to empty for legacy entries |

## Operations

```rust
use tmux_router::registry;
use std::path::Path;

let path = Path::new("registry.json");

// Load (returns empty map if file doesn't exist)
let registry = registry::load_registry(path)?;

// Look up a pane by key
let pane = registry::lookup(path, "my-session")?;

// Prune dead panes
let tmux = tmux_router::Tmux::default_server();
let pruned = registry::prune_dead(&registry, &tmux);
```

## Locking

Read-modify-write helpers (`with_registry`, `with_registry_val`,
`update_window_for_entry`, `prune`) hold an exclusive advisory lock on
`<registry>.lock` for the whole operation. Take it yourself with
`RegistryLock`:

```rust
use tmux_router::registry::{RegistryLock, RegistryLockWait, set_registry_lock_wait};
use std::time::Duration;

// Process-wide policy for every acquisition that does not pass its own,
// including the helpers above. Default: 30s, no heartbeat.
set_registry_lock_wait(RegistryLockWait {
    bound: Duration::from_secs(10),
    heartbeat: Some(|| eprintln!("still waiting for the registry lock")),
});

let _lock = RegistryLock::acquire_for(path, "rename a session")?;
```

An acquisition never waits past its bound. It polls a non-blocking lock on the
guard's own file, so the attempt that succeeds is the acquisition, and it fails
with `RegistryLockWaitExceeded` naming the lock file, the purpose, the time
waited and the holder (pid and command line, on Linux). The lock is not
reentrant: a nested acquisition on the same thread fails instead of
deadlocking (`acquire_or_skip` returns `None` instead).
