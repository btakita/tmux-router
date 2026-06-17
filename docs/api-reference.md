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
| `send_keys(pane, text)` | Send literal text plus named `Enter` in one tmux invocation |
| `select_pane(pane)` | Focus a pane |
| `join_pane(src, dst, flag)` | Move pane to another window |
| `break_pane(pane)` | Break pane into new window |
| `stash_pane(pane, session)` | Move pane to stash window |
| `auto_start(session, cwd)` | Create session/window as needed |
| `attach_control_mode(target)` | Attach a long-lived `tmux -C` client for event-driven output and lifecycle notifications |

### `TmuxControlMode`

Live control-mode client returned by `Tmux::attach_control_mode()`.

| Method | Description |
|--------|-------------|
| `send_command(command)` | Send one tmux command through the control-mode client |
| `next_event_timeout(timeout)` | Read the next parsed event, or `None` when the timeout expires |
| `wait_for_event(timeout, predicate)` | Wait for a matching control-mode event |
| `wait_for_pane_output(pane, timeout, predicate)` | Wait for matching `%output` bytes from one pane without polling `capture-pane` |

### `TmuxControlEvent`

Parsed control-mode line. Notable variants:

- `Output { pane_id, bytes }` for `%output` pane data.
- `PaneLifecycle { name, pane_id, args }` for `%pane-*` notifications such as pane death or exit.
- `Begin`, `End`, and `Error` for command result boundaries.
- `Notification` for other `%name ...` events.
- `Exit` for `%exit`.

### `IsolatedTmux`

RAII guard for test servers. Creates an isolated tmux via `-L`, kills on drop.

### `RegistryEntry`

A single registry entry with fields: `pane`, `pid`, `cwd`, `started`,
`session_id`, `file`, `window`, `supervisor_instance_id`.

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
