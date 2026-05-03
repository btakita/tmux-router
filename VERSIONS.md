# Versions

## Unreleased

- **Callers can now block ephemeral donor/spare-pane assignment for specific unresolved files.** `SyncOptions` now exposes an `allow_unresolved_pane_assignment` callback, and `sync` skips both same-column donor reuse and target-window spare-pane reuse when the caller rejects a file. This lets higher-level tools fail closed for managed-but-unresolved documents instead of silently aliasing another live pane into that column. Added a regression that blocks spare-pane reuse for a missing left-column file.
- **Session/key primitives extracted from agent-doc.** `Tmux` now owns `send_key()` and `ensure_pane_in_session()`, so sibling projects can reuse single-key dispatch and fail-closed session checks without shelling out for their own wrappers.

- **Pane-local `remain-on-exit` now survives stash/rescue moves.** `Tmux::enable_remain_on_exit()` now sets the option at pane scope (`set-option -p -t <pane>`) instead of on the original window, so a pane moved with `join-pane` into a stash window still remains inspectable if its process exits later. Added a regression test that stashes a pane first, then exits it, and proves tmux retains the dead pane plus its exit status.

## 0.3.9 (2026-04-04)

- **Cross-session pane operations**: `SessionScope::join_pane` and `SessionScope::swap_pane` now allow cross-session moves (with log warning) instead of blocking them. Registered panes drift between sessions during stash/rescue cycles; blocking left orphaned panes that could never return.
- **Last-pane guard**: Never stash the last pane in a window. Previously the stash loop could remove all panes, causing tmux to close the window entirely.
- **`--window` is authoritative for target session**: When `--window` is provided, its session takes priority over wanted panes' session. Wanted panes may have drifted to other sessions during stash/rescue; `--window` represents the user's intent.

## 0.3.8 (2026-04-01)

- **Stash guard for unresolved managed files**: When all pane columns are empty because managed files failed to resolve (dead panes, pruned registry), skip stashing to preserve existing layout. Only stash when files are truly unmanaged (no session UUIDs).

## 0.3.7 (2026-03-31)

- **SyncOptions.protect_pane**: Add callback for busy pane guard — protected panes are skipped during stash operations.

## 0.3.6 (2026-03-29)

- **No early exits**: Removed all early exits from `sync` — the full reconcile path now runs for 0, 1, or 2+ resolved panes uniformly. Previous early exits for `resolved < 2` bypassed the DETACH phase, leaving orphaned panes from previous layouts visible.
- Reconcile detail file logging to `/tmp/agent-doc-sync.log`
- Empty pane_columns bail stashes all panes via `--window`
- Test: `test_sync_single_resolved_stashes_excess` verifies 1-pane reconcile stashes excess

## 0.3.5 (2026-03-29)

- Sync trace logging at key decision points (resolution summary, exit path, full reconcile entry)
- Early-exit stash removed: preserves previous-column panes instead of stashing
- Test updated: `test_sync_early_exit_preserves_other_panes`

## 0.3.4 (2026-03-29)

- Early-exit stash now derives session from pane via `pane_session()` instead of `doc_tmux_session` (was always None = dead code)
- New test: `test_sync_early_exit_stash_derives_session_from_pane`

## 0.3.3 (2026-03-30)

- **Integration test**: Add test for early-exit excess pane stashing
- **Docs**: Update README for early-exit stash behavior

## 0.3.2 (2026-03-28)

- **alive_pane_ids**: Bulk method for O(1) pane liveness checks
- **Clippy fixes**: v0.3.1 cleanup

## 0.3.0 (2026-03-25)

- **kill_pane safety guards**: Cross-session check, prevent killing panes outside managed sessions
- **Stash orphan fix**: `stash_pane` kills pane on join failure instead of creating orphan window

## 0.2.9 (2026-03-24)

- **swap_pane**: Atomic 1:1 pane replacement via `swap-pane`
- **Focus-steal logging**: Diagnosis logging during detach/stash operations

## 0.2.8 (2026-03-18)

- **Numeric session fix**: Fix session name ambiguity in `new_window()` for numeric session names

## 0.2.7 (2026-03-18)

- **TmuxBatch**: Fire-and-forget command batching

## 0.2.6 (2026-03-17)

- **split_window**: Add `split_window` method to Tmux

## 0.2.5 (2026-03-17)

- Improve `send_keys` timing (50ms → 100ms delay) and add session logging to `focus_pane`
- Fix `first_pane_join_flag()` for layout-correct initial join

## 0.2.4 (2026-03-09)

- Assign spare window panes to unresolved files (Phase 1.75)

## 0.2.3 (2026-03-09)

- Add `file_panes` to `SyncResult`

## 0.2.2 (2026-03-07)

- Nested lock detection, `acquire_or_skip`, race condition tests

## 0.2.1 (2026-03-07)

- Add advisory file locking (flock) for registry safety

## 0.2.0 (2026-03-06)

- Add CLI binary with `send`, `capture`, and file-name addressing

## 0.1.3 (2026-03-06)

- Fix stash window creation with numeric session names

## 0.1.2 (2026-03-05)

- Fix: resize stash window before join to prevent "pane too small" errors

## 0.1.1 (2026-03-04)

- Add `prune()` function for registry cleanup

## 0.1.0 (2026-03-04)

- Initial extraction from agent-doc
- SyncResult, GitHub Actions CI/release, mdbook docs
