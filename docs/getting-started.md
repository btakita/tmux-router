# Getting Started

## Installation

Add tmux-router to your `Cargo.toml`:

```toml
[dependencies]
tmux-router = "0.2"
```

## Basic Usage

```rust
use tmux_router::{Tmux, FileResolution, sync};
use std::path::Path;

let tmux = Tmux::default_server();
let registry_path = Path::new(".tmux-router/registry.json");

let resolve_file = |path: &Path| -> Option<FileResolution> {
    // Your file resolution logic here
    Some(FileResolution::Registered {
        key: "my-session-key".to_string(),
        tmux_session: None,
    })
};

let col_args = vec!["file1.md,file2.md".to_string(), "file3.md".to_string()];

let result = sync(
    &col_args,
    None,        // window
    None,        // focus
    &tmux,
    &registry_path,
    &resolve_file,
)?;

println!("Arranged in window: {}", result.target_window);
```

## Requirements

- tmux must be installed and available on `$PATH`
- Rust 2024 edition (1.85+)
