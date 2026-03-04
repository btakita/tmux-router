# Introduction

**tmux-router** is a Rust library for declarative tmux pane routing. It syncs editor layouts to tmux pane arrangements, enabling IDE plugins to mirror their split views in tmux.

## Features

- **Declarative layout sync** — describe columns of files, tmux-router arranges the panes
- **Registry** — persistent key-to-pane mappings via JSON
- **Isolated testing** — full test suite using isolated tmux servers (`-L` flag)
- **Attach-first reconciliation** — join desired panes before evicting unwanted ones (prevents flicker)

## Use Cases

- IDE plugins that manage tmux panes alongside editor splits
- AI agent orchestration with multiple tmux sessions
- Any tool that needs programmatic tmux layout control
