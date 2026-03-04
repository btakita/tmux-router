# tmux-router

> **Alpha Software** — API may change between minor versions.

Declarative tmux pane routing — sync editor layouts to tmux pane arrangements.

## Features

- **Declarative layout sync**: Mirror your editor's split layout in tmux
- **Attach-first reconciliation**: No pane selection flicker during rearrangement
- **Session affinity**: Keep panes in a configured tmux session
- **Stash window**: Evicted panes collected in one place, not scattered
- **Column-positional focus**: Smart fallback when focused file has no pane

## Usage

### As a library

```rust
use tmux_router::{Tmux, sync, FileResolution};
use std::path::Path;

let tmux = Tmux::default_server();
let col_args = vec!["a.md,b.md".to_string(), "c.md".to_string()];
sync(
    &col_args,
    Some("@1"),
    Some("a.md"),
    &tmux,
    Path::new(".tmux-router/registry.json"),
    &|path| {
        // Your file resolution logic here
        Some(FileResolution::Registered {
            key: path.to_string_lossy().to_string(),
            tmux_session: None,
        })
    },
).unwrap();
```

### As a CLI (coming soon)

```bash
tmux-router sync --col a.md,b.md --col c.md --focus a.md
```

## License

MIT
