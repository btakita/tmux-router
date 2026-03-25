# Reconcile

## [Ontology](../../../existence-lang/ontology/src/ontology.md)

[Reconcile](./reconcile.md) extends [Algorithm](../../../existence-lang/ontology/src/algorithm.md). It is the attach-first sync algorithm that transforms the current tmux state into the desired [layout](./layout.md) within the target [session](./session.md) boundary. Reconcile operates in five ordered phases:

1. **ATTACH** — move missing panes into the target [window](./window.md).
2. **SELECT** — focus the desired pane.
3. **DETACH** — stash unwanted panes to the [stash](./stash.md) window.
4. **REORDER** — swap panes to match the declared column order.
5. **VERIFY** — confirm no `SessionScope` violations remain.

The attach-before-detach order is intentional: tmux auto-selects a pane when the current pane is removed; attaching first ensures the auto-selected pane is one we want, not an arbitrary one.

## [Axiology](../../../existence-lang/ontology/src/axiology.md)

Reconcile is the core intelligence of tmux-router. It closes the gap between declarative intent ([layout](./layout.md)) and live tmux state, minimising disruption: panes are reused rather than killed, focus is preserved, and a 1-in/1-out replacement uses an atomic `swap-pane` fast path to eliminate visual flicker. Errors during any phase are logged to `SyncLog` but do not abort the run, so a partially-degraded layout is always preferred over a crash.

## [Epistemology](../../../existence-lang/ontology/src/epistemology.md)

### [Pattern](../../../existence-lang/ontology/src/pattern.md) Expression

- **Resolution tiers**: (1) [registry](./registry.md) lookup by file path, (2) in-memory donor from the same column, (3) spare unassigned pane in the target window.
- **Fast path**: when exactly one pane is entering and one is leaving, `swap-pane` is used atomically — no join/break round-trip.
- **Scope guard**: every `join_pane` / `swap_pane` call passes through `SessionScope`; out-of-scope moves return `Ok(false)` and are skipped.
- **Observability**: `SyncLog` records each phase operation with pass/fail. `has_errors()` lets callers detect partial failure without panicking.
- **Post-condition**: `verify_boundary` confirms all panes in the target window belong to the target session; violations are logged as `SCOPE_VIOLATION`.
