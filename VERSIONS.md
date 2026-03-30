# Versions

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
