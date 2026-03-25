# Registry

## [Ontology](../../../existence-lang/ontology/src/ontology.md)

The [registry](./registry.md) extends [State](../../../existence-lang/ontology/src/state.md). It is a persistent JSON file mapping file paths (keys) to tmux [pane](./pane.md) IDs (values). The registry is the durable state that allows the router to find the correct pane for a file across independent invocations and across [session](./session.md) restarts (as long as the tmux server is alive and the pane is still live).

Access to the registry is protected by an advisory file lock (`RegistryLock`) to prevent concurrent write corruption. Registry errors during [reconcile](./reconcile.md) are non-fatal — they produce log warnings and the operation continues.

## [Axiology](../../../existence-lang/ontology/src/axiology.md)

The registry is what makes tmux-router a router rather than a one-shot layout tool. Without it, every invocation would have to discover or guess which pane belongs to which file. With it, identity is stable: `open file.rs` always routes to the same pane that was previously assigned to `file.rs`, even if that pane has since been stashed, moved, or the layout has changed entirely. The registry closes the loop between [layout](./layout.md) intent and [pane](./pane.md) identity.

## [Epistemology](../../../existence-lang/ontology/src/epistemology.md)

### [Pattern](../../../existence-lang/ontology/src/pattern.md) Expression

- **Format**: JSON object `{ "path/to/file.rs": "%42", ... }` stored at a caller-specified path.
- **Lock**: `RegistryLock::acquire(registry_path)` acquires an exclusive advisory lock; the lock is released on drop. Nested acquisition on the same thread is detected and rejected (flock is not reentrant on Linux).
- **Resolution tier 1**: during [reconcile](./reconcile.md), registry lookup is the first and highest-priority resolution strategy — if the registry has a live pane for a file, it is used.
- **Update on touch**: the registry is updated for every pane touched during reconciliation, keeping it current with the latest state.
- **Liveness filter**: stale entries (pane IDs no longer alive) are filtered out during lookup via `pane_alive(pane_id)`, so the registry self-heals across server restarts.
