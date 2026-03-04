# Sync Algorithm

The sync algorithm arranges tmux panes to match a declarative column layout.

## Input

- **Columns**: Each column is a comma-separated list of file paths
- **Window**: Optional target window ID
- **Focus**: Optional file to focus after sync
- **Resolve function**: Maps file paths to session keys

## Phases

1. **Resolve** — Map files to pane IDs via the registry
2. **Auto-register** — Unresolved files share a pane with another file in the same column (ephemeral, not persisted)
3. **Build columns** — Group resolved panes into the column structure
4. **Pick target window** — Find the window containing the most desired panes
5. **Reconcile** — Attach-first algorithm: join missing panes, then evict unwanted ones
6. **Equalize** — Resize panes for even distribution

## Reconciliation: Attach-First

```text
SNAPSHOT — query current panes in target window
FAST PATH — if layout already correct, done
ATTACH   — join missing desired panes (all with -d flag, no focus change)
SELECT   — select the focus pane
DETACH   — stash unwanted panes (focus survives, no selection jump)
REORDER  — if needed, break non-first + rejoin in correct order
VERIFY   — confirm final layout matches desired
```

The attach-first approach prevents pane selection flicker by ensuring the focus pane is in the window before any evictions occur.

## Return Value

`SyncResult` contains:
- `target_session`: The tmux session owning the target window
- `target_window`: The window ID where panes were arranged
