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

## Registry

Persistent key-to-pane mappings stored as JSON. See [Registry](./registry.md).

## Sync

Declarative layout synchronization. See [Sync Algorithm](./sync-algorithm.md).
