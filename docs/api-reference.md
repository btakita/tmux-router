# API Reference

## Public Types

### `Tmux`

Tmux server handle. Supports both the default server and isolated test servers.

| Method | Description |
|--------|-------------|
| `default_server()` | Handle for the user's tmux |
| `cmd()` | Build a `Command` with server flags |
| `pane_alive(id)` | Check if pane exists |
| `running()` | Check if server has sessions |
| `session_exists(name)` | Check if named session exists |
| `new_session(name, cwd)` | Create session, return pane ID |
| `new_window(session, cwd)` | Create window, return pane ID |
| `send_keys(pane, text)` | Send literal text + Enter |
| `select_pane(pane)` | Focus a pane |
| `join_pane(src, dst, flag)` | Move pane to another window |
| `break_pane(pane)` | Break pane into new window |
| `stash_pane(pane, session)` | Move pane to stash window |
| `auto_start(session, cwd)` | Create session/window as needed |

### `IsolatedTmux`

RAII guard for test servers. Creates an isolated tmux via `-L`, kills on drop.

### `RegistryEntry`

A single registry entry with fields: `pane`, `pid`, `cwd`, `started`, `file`, `window`.

### `Registry`

Type alias: `HashMap<String, RegistryEntry>`

### `FileResolution`

```rust
pub enum FileResolution {
    Registered { key: String, tmux_session: Option<String> },
    Unmanaged,
}
```

### `SyncResult`

```rust
pub struct SyncResult {
    pub target_session: Option<String>,
    pub target_window: String,
}
```

## Public Functions

### `sync()`

```rust
pub fn sync(
    col_args: &[String],
    window: Option<&str>,
    focus: Option<&str>,
    tmux: &Tmux,
    registry_path: &Path,
    resolve_file: &dyn Fn(&Path) -> Option<FileResolution>,
) -> Result<SyncResult>
```

Main entry point. Syncs editor layout to tmux panes.
