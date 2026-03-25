# tmux-router Ontology

Domain ontology for the tmux-router system, extending the [existence-lang](https://github.com/btakita/existence-lang) kernel vocabulary.

## Terms

| Term | Extends | Definition |
|------|---------|------------|
| **[Pane](./src/pane.md)** | [Entity](../../existence-lang/ontology/src/entity.md) | A tmux pane managed by the router — an identity-bearing terminal surface associated with a file path via the registry. |
| **[Window](./src/window.md)** | [System](../../existence-lang/ontology/src/system.md) | A tmux window containing an arranged set of panes — a whole of spatial relationships that forms the visible layout. |
| **[Session](./src/session.md)** | [Domain](../../existence-lang/ontology/src/domain.md) | A tmux session scoping all window and pane operations — the bounded domain within which routing is confined. |
| **[Layout](./src/layout.md)** | [Pattern](../../existence-lang/ontology/src/pattern.md) | A columnar arrangement of file paths parsed from `--col` arguments — a repeatable pattern mapping files to pane positions. |
| **[Reconcile](./src/reconcile.md)** | [Algorithm](../../existence-lang/ontology/src/algorithm.md) | The attach-first sync algorithm that transforms current tmux state into the desired layout without disrupting focus. |
| **[Stash](./src/stash.md)** | [State](../../existence-lang/ontology/src/state.md) | A hidden window holding displaced panes — a holding state that preserves pane identity across layout transitions. |
| **[Registry](./src/registry.md)** | [State](../../existence-lang/ontology/src/state.md) | A persistent JSON mapping of file paths to pane IDs — the durable state that enables routing across sessions. |

## Ontology Chain

```
Existence → Entity → System → Domain → Session
                                         └── Window (System)
                                               └── Pane (Entity)
                              ↑
                           Layout (Pattern) → Reconcile (Algorithm)
                                                    ├── Stash (State)
                                                    └── Registry (State)
```

The session narrows the domain boundary. The layout declares intent. Reconcile closes the gap between intent and tmux state, using the stash to park displaced panes and the registry to maintain identity continuity.
