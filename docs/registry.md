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
