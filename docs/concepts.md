# Concepts

tmux-router has three core modules:

## Tmux

The `Tmux` struct wraps tmux CLI commands. It supports both the default server and isolated test servers via the `-L` flag.

```rust
use tmux_router::Tmux;

// Default server (user's tmux)
let tmux = Tmux::default_server();

// Check if a pane is alive
assert!(tmux.pane_alive("%0"));
```

## Control Mode

Control mode is the streaming side of the tmux wrapper. `Tmux::attach_control_mode()`
starts a long-lived `tmux -C` client and parses its event lines into
`TmuxControlEvent` values. Consumers that need live pane output or lifecycle
state can wait for `%output`, `%pane-died`, `%pane-exited`, and related
notifications instead of sleeping and polling `capture-pane`.

## Registry

Persistent key-to-pane mappings stored as JSON. See [Registry](./registry.md).

## Sync

Declarative layout synchronization. See [Sync Algorithm](./sync-algorithm.md).
