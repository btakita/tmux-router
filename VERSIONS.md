# Versions

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
