# Window

## [Ontology](../../../existence-lang/ontology/src/ontology.md)

A [window](./window.md) extends [System](../../../existence-lang/ontology/src/system.md). It is a tmux window — a whole of spatial relationships among its member [panes](./pane.md). A window belongs to a [session](./session.md) and occupies one screen surface at a time. The router operates on a single target window per [reconcile](./reconcile.md) cycle; all pane arrangements expressed by a [layout](./layout.md) are realised within that window's boundaries.

A special window named `stash` serves as the [stash](./stash.md) — a system whose relationships are hidden (offscreen) rather than visible.

## [Axiology](../../../existence-lang/ontology/src/axiology.md)

The window provides the spatial container that makes a layout meaningful. Without a window boundary, pane positions have no frame of reference. The router selects the best window (`find_best_window`) — the one already containing the most wanted panes — to minimise movement cost during reconciliation. The stash window extends this value by ensuring no pane is destroyed merely to make room.

## [Epistemology](../../../existence-lang/ontology/src/epistemology.md)

### [Pattern](../../../existence-lang/ontology/src/pattern.md) Expression

- **Target window**: the window where the desired [layout](./layout.md) is materialised.
- **Stash window**: a window named `"stash"` holding displaced [panes](./pane.md); created idempotently via `ensure_stash_window`.
- **Window selection**: `find_best_window` scans the [session](./session.md) and returns the window with the highest count of already-resolved panes.
- **Pane membership**: `list_window_panes(window_id)` enumerates all panes; `list_panes_ordered` returns them sorted by screen position.
- **Layout application**: `select_layout(window_id, layout)` applies a tmux named layout string; `equalize_sizes` then redistributes space by column ratios.
