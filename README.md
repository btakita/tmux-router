# tmux-router

> **Alpha Software** — API may change between minor versions.

Declarative tmux pane routing — bind files to tmux panes and manage layouts programmatically.

## Install

```bash
cargo install tmux-router
```

## CLI

### Layout Management

```bash
# Sync editor layout to tmux panes (2 columns)
tmux-router sync --col src/main.rs,src/lib.rs --col tests/test_main.rs

# Register a file to an existing pane
tmux-router register src/main.rs %5

# Unregister a file
tmux-router unregister src/main.rs

# Show all bindings and pane health
tmux-router status

# Prune dead panes from registry
tmux-router resync

# Focus (select) the pane bound to a file
tmux-router focus src/main.rs
```

### Pane Interaction

```bash
# Send a command to a pane (by file name or pane ID)
tmux-router send src/main.rs "cargo test"
tmux-router send %5 "cargo test"

# Send text without pressing Enter
tmux-router send %5 "partial input" --no-enter

# Send raw tmux keys (C-c, Escape, Enter, etc.)
tmux-router send %5 "C-c" --raw

# Capture pane screen content
tmux-router capture src/main.rs
tmux-router capture %5 --lines 50
```

## Use Cases

### Test Runner

Bind a pane to your source file. The pane runs `cargo watch` — every save triggers a test rerun.

```bash
tmux-router sync --col src/main.rs --col tests/
tmux-router send tests/ "cargo watch -x test"
# Later, check results:
tmux-router capture tests/ --lines 20 | grep "test result"
```

### REPL-Driven Development

Edit a Python file in your editor, send code blocks to an IPython REPL in a bound pane.

```bash
tmux-router register scratch.py %5
# In pane %5: ipython is running
tmux-router send scratch.py "exec(open('scratch.py').read())"
tmux-router capture scratch.py --lines 10
```

### Log Watcher

Bind a log file to a pane running `tail -f` or `lnav`.

```bash
tmux-router register app.log %7
# In pane %7: tail -f app.log
# From any script:
tmux-router capture app.log --lines 5
```

### Database Console

Bind a SQL file to a pane running `psql`. Send queries from your editor.

```bash
tmux-router register queries.sql %8
# In pane %8: psql mydb
tmux-router send queries.sql "SELECT count(*) FROM users;"
tmux-router capture queries.sql --lines 5
```

### Multi-Agent Orchestration

Run multiple Claude Code instances, each bound to a different task file.

```bash
tmux-router sync --col task-a.md --col task-b.md --col task-c.md
tmux-router send task-a.md "fix the auth bug"
tmux-router send task-b.md "add unit tests for the parser"
# Check progress:
tmux-router capture task-a.md --lines 30
```

### Scripted Workflows

Combine `send` and `capture` for automated pipelines.

```bash
#!/bin/bash
tmux-router send server.rs "cargo run"
sleep 2
tmux-router send client.rs "curl localhost:8080/health"
sleep 1
result=$(tmux-router capture client.rs --lines 1)
if echo "$result" | grep -q "ok"; then
    echo "Server healthy"
fi
```

## Library Usage

```rust
use tmux_router::{Tmux, sync, FileResolution};
use std::path::Path;

let tmux = Tmux::default_server();
let cols = vec!["a.md,b.md".into(), "c.md".into()];
sync(
    &cols,
    Some("@1"),
    Some("a.md"),
    &tmux,
    Path::new(".tmux-router/registry.json"),
    &|path| {
        Some(FileResolution::Registered {
            key: path.to_string_lossy().to_string(),
            tmux_session: None,
        })
    },
)?;

// Send text to a pane
tmux.send_keys("%5", "hello world")?;

// Capture pane content
let output = tmux.capture_pane("%5", Some(20))?;

// Send raw tmux keys
tmux.send_keys_raw("%5", "C-c")?;
```

## Architecture

- **Registry** (`.tmux-router/registry.json`) — maps file paths to pane IDs
- **Layout reconciliation** — attach-first algorithm (ATTACH → SELECT → DETACH → REORDER → VERIFY)
- **Stash window** — evicted panes collected in one place, not scattered; early-exit paths stash excess panes to prevent leftovers from previous layouts
- **Health management** — dead panes pruned, stale bindings cleaned up

## License

MIT
