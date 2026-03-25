# Pane

## [Ontology](../../../existence-lang/ontology/src/ontology.md)

A [pane](./pane.md) extends [Entity](../../../existence-lang/ontology/src/entity.md). It is a tmux pane managed by the router — an identity-bearing terminal surface addressed by a unique pane ID (e.g. `%42`). A pane is the atomic unit of routing: it renders one file's process output and is associated with a file path via the [registry](./registry.md). Panes are contained within a [window](./window.md), which is itself contained within a [session](./session.md).

A pane carries identity independent of its position. When displaced from the layout it moves to the [stash](./stash.md) rather than being destroyed. Identity persists across [reconcile](./reconcile.md) cycles as long as the pane process is alive.

## [Axiology](../../../existence-lang/ontology/src/axiology.md)

Panes are the reason the router exists. Every other term — [layout](./layout.md), [reconcile](./reconcile.md), [stash](./stash.md), [registry](./registry.md) — exists in service of routing the right pane to the right screen position. Preserving pane identity avoids process churn: the running shell or program is not restarted merely because the layout changed.

## [Epistemology](../../../existence-lang/ontology/src/epistemology.md)

### [Pattern](../../../existence-lang/ontology/src/pattern.md) Expression

- **In tmux**: `%42` — a numeric handle stable for the lifetime of the tmux server.
- **In the registry**: the value side of a `file_path → pane_id` entry.
- **In reconcile**: the unit moved by `join-pane`, `swap-pane`, or `break-pane`.
- **In the stash**: an alive pane parked in a hidden window, awaiting reuse.
- **Liveness check**: `pane_alive(pane_id)` — confirms the pane still exists before any routing operation.
