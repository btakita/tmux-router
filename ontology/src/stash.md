# Stash

## [Ontology](../../../existence-lang/ontology/src/ontology.md)

A [stash](./stash.md) extends [State](../../../existence-lang/ontology/src/state.md). It is a hidden [window](./window.md) named `"stash"` within the [session](./session.md) that holds displaced [panes](./pane.md). The stash is the intermediate state a pane occupies when it has been removed from the active layout but not yet destroyed. Panes in the stash remain alive — their processes continue running — and can be reattached by a future [reconcile](./reconcile.md) cycle.

Multiple stash windows may exist (primary + overflow) when the primary stash fills beyond tmux's per-window pane limit.

## [Axiology](../../../existence-lang/ontology/src/axiology.md)

The stash preserves pane identity and process continuity across layout changes. Without a stash, removing a pane from the layout would require killing it and spawning a new shell for reuse — discarding any running process, shell history, and working directory. The stash makes layout transitions lossless: the cost of a layout change is a `join-pane` move, not a process restart.

## [Epistemology](../../../existence-lang/ontology/src/epistemology.md)

### [Pattern](../../../existence-lang/ontology/src/pattern.md) Expression

- **Creation**: `ensure_stash_window(session)` — idempotent; creates the stash window only if absent.
- **Deposit**: `stash_pane(pane_id, session)` — moves a pane into the stash via `join-pane`; falls back to `break_pane_to_stash` if join fails.
- **Discovery**: `find_stash_window(session)` returns the primary stash window ID; `find_all_stash_windows(session)` returns all overflow windows.
- **Reuse**: during [reconcile](./reconcile.md) ATTACH phase, the resolution tier 2 (in-memory donor) and tier 3 (spare pane) draw from panes that may have originated in the stash.
- **Overflow guard**: when a stash window exceeds capacity, `break_pane_to_stash` creates a new window named `"stash"` — the session may hold multiple stash windows simultaneously.
