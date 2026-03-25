# Session

## [Ontology](../../../existence-lang/ontology/src/ontology.md)

A [session](./session.md) extends [Domain](../../../existence-lang/ontology/src/domain.md). It is a tmux session — the bounded domain within which all [window](./window.md) and [pane](./pane.md) operations are scoped. A session has a name, owns one or more windows, and runs on a tmux server. The router confines all routing to a single session; cross-session pane movement is blocked by `SessionScope`.

## [Axiology](../../../existence-lang/ontology/src/axiology.md)

The session boundary is a safety invariant. Without it, a [reconcile](./reconcile.md) operation could inadvertently move panes between unrelated workspaces, corrupting other users' or projects' layouts. `SessionScope` enforces this boundary at every mutation point (`join_pane`, `swap_pane`), returning `Ok(false)` rather than an error when a scope violation is detected, so reconciliation degrades gracefully.

## [Epistemology](../../../existence-lang/ontology/src/epistemology.md)

### [Pattern](../../../existence-lang/ontology/src/pattern.md) Expression

- **Session identity**: a string name (e.g. `"main"`) passed as `target_session` to `sync`.
- **Scope enforcement**: `SessionScope::contains(pane_id)` returns `true` when the pane belongs to the session; `verify_boundary` logs `SCOPE_VIOLATION` for any outliers post-reconcile.
- **Existence check**: `session_exists(name)` / `session_alive(name)` — guards before creating or joining.
- **Auto-start**: `auto_start(session, cwd)` creates the session if absent, otherwise opens a new window — idempotent session bootstrapping.
- **Stash containment**: the [stash](./stash.md) window is always created within this session via `ensure_stash_window(session)`.
