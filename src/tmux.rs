//! # Module: tmux
//!
//! Tmux server handle and command abstraction. Supports isolated `-L` socket
//! servers for hermetic testing alongside the default user tmux server.
//!
//! ## Spec
//!
//! - `Tmux` wraps all tmux CLI interactions; constructed with either the
//!   default server (`Tmux::default_server()`) or a named socket for isolation.
//! - `Tmux::cmd()` emits a `Command` pre-configured with `-L <socket> -f /dev/null`
//!   when `server_socket` is set, or bare `tmux` for the default server.
//! - `pane_alive(pane_id)` — returns true iff `pane_id` still exists and `pane_dead=0`
//!   (retained dead panes from `remain-on-exit` are not considered alive).
//! - `pane_dead(pane_id)` — returns true iff tmux still retains the pane but the child
//!   process has exited (`pane_dead=1`).
//! - `running()` — returns true iff the server has at least one session.
//! - `session_exists(name)` / `session_alive(name)` — check session presence by name.
//! - `new_session(name, cwd)` — creates a detached session and returns its first pane ID.
//! - `new_window(session, cwd)` — creates a new window in an existing session and
//!   returns the pane ID; uses `session:` target syntax to avoid numeric-name ambiguity.
//! - `send_keys(pane_id, text)` — sends text literally (`-l`) then `Enter` as a
//!   separate call with a 100 ms delay between them for TUI compatibility.
//! - `send_key(pane_id, key)` — sends a single tmux key name (for example `Enter`,
//!   `Up`, `Escape`) without enabling literal mode.
//! - `send_keys_raw(pane_id, keys)` — sends keystrokes without literal mode or Enter,
//!   allowing tmux key names (`C-c`, `Escape`, etc.) to be interpreted.
//! - `select_pane(pane_id)` — focuses a pane by batching `select-window` +
//!   `select-pane`; never calls `switch-client` to avoid disrupting other clients.
//! - `split_window(target, cwd, flags)` — splits a pane with caller-specified flags
//!   (`-h`/`-v`, `-d`) and returns the new pane ID.
//! - `join_pane(src, dst, split_flag)` — moves `src` into `dst`'s window.
//! - `swap_pane(src, dst)` — atomically swaps two panes without focus change (`-d`).
//! - `break_pane(pane_id)` — breaks a pane into a new detached window.
//! - `kill_pane(pane_id)` — kills a pane; refuses (returns `Err`) if it is the sole
//!   pane in the sole window of its session to prevent accidental session destruction.
//! - `session_window_count(session)` — counts windows in a session.
//! - `pane_window(pane_id)` / `pane_session(target)` — resolve containment hierarchy.
//! - `ensure_pane_in_session(pane_id, expected_session)` — fail closed when a pane
//!   drifted into a different tmux session than the caller expects.
//! - `list_window_panes(window_id)` — lists all pane IDs in a window.
//! - `list_panes_ordered(window_id)` — same but sorted by left/top screen position.
//! - `largest_pane_in_window(window_id)` — returns the pane ID with the most rows.
//! - `resize_pane(pane_id, flag, size)` — resizes a pane to a percentage.
//! - `window_height(window_id)` / `pane_height(pane_id)` — query row counts.
//! - `select_layout(window_id, layout)` / `select_window(window_id)` — layout control.
//! - `active_pane(session)` / `active_window(session)` — query current focus.
//! - `find_stash_window(session)` — finds the first window named "stash" in a session.
//! - `find_all_stash_windows(session)` — finds ALL windows named "stash" (primary + overflow).
//! - `ensure_stash_window(session)` — idempotently creates the stash window if absent.
//! - `stash_pane(pane_id, session)` — moves a pane into the stash window; falls back
//!   to `break_pane_to_stash` on join failure; never creates orphan windows silently.
//! - `break_pane_to_stash(pane_id, session)` — breaks pane and renames new window "stash".
//! - `auto_start(session, cwd)` — creates a session if the server or session is absent,
//!   otherwise creates a new window; returns the new pane ID.
//! - `capture_pane(pane_id, lines)` — captures visible content or N scrollback lines.
//! - `enable_remain_on_exit(pane_id)` — enables `remain-on-exit on` for the pane's
//!   current window so dead panes remain inspectable until cleanup.
//! - `pane_dead_status(pane_id)` — returns tmux's retained dead-pane exit status when
//!   a pane is dead and still present.
//! - `raw_cmd(args)` — escape hatch for arbitrary tmux commands; returns trimmed stdout.
//! - `list_all_windows()` / `list_all_panes()` — global state summaries for logging.
//! - `dump_tmux_tree()` — formats a full session→window→pane tree as a string.
//! - `kill_server()` — kills the tmux server (intended for isolated test teardown).
//! - `IsolatedTmux` — RAII wrapper that kills its `-L` server on drop; dereferences
//!   to `Tmux` for all method access.
//! - `TmuxBatch` — accumulates tmux commands and fires them in a single invocation
//!   joined by `;`; supports `execute()` (status only) and `execute_output()` (stdout).
//!
//! ## Agentic Contracts
//!
//! - All methods that mutate tmux state return `Result<_>` and propagate errors;
//!   callers must handle failures explicitly — no silent swallows except in boolean
//!   probe methods (`pane_alive`, `running`, `session_exists`, `session_alive`).
//! - `pane_alive` excludes retained dead panes; callers that need dead-pane cleanup or
//!   provenance must probe `pane_dead` / `pane_dead_status` explicitly.
//! - `kill_pane` never destroys an entire session silently; it returns `Err` when
//!   the kill would be session-destructive.
//! - `stash_pane` never silently drops a pane; on all failure paths it either joins,
//!   breaks-to-stash, or returns an error.
//! - `select_pane` never disrupts other connected terminal clients (`switch-client`
//!   is intentionally omitted).
//! - `send_keys` always interprets its `text` argument literally (no tmux key expansion).
//!   Use `send_key` / `send_keys_raw` when key names like `C-c` or `Enter` are required.
//! - Isolated test servers (`IsolatedTmux`) are guaranteed to be torn down on drop
//!   even if the test panics, preventing socket leaks.
//! - `TmuxBatch::execute` is fire-and-forget at the individual command level —
//!   a failure in any batched command may not be distinguishable from others.
//! - Numeric session names are always referenced with a trailing `:` to prevent
//!   misinterpretation as window indices by tmux.
//!
//! ## Evals
//!
//! - `empty_batch_noop`: empty `TmuxBatch::execute()` → `Ok(())` without invoking tmux.
//! - `batch_tracks_count`: after two `add()` calls, `len() == 2` and `is_empty() == false`.
//! - `kill_pane_guards_last_pane`: calling `kill_pane` on the only pane in the only window
//!   → `Err` containing "refusing to kill pane".
//! - `new_window_numeric_session`: `new_window("0", cwd)` uses target `"0:"` and succeeds
//!   without "index 0 in use" error.
//! - `send_keys_literal`: text containing tmux special chars (e.g. `q`, `C-c`) is sent
//!   as-is and not interpreted as key sequences.
//! - `auto_start_creates_session`: with no server running, `auto_start` creates a session
//!   and returns a non-empty pane ID.
//! - `auto_start_creates_window`: with an existing session, `auto_start` adds a window
//!   rather than a new session.
//! - `stash_pane_fallback`: when `join_pane` fails (pane too small), `stash_pane`
//!   calls `break_pane_to_stash` and names the overflow window "stash".
//! - `isolated_tmux_cleanup`: dropping `IsolatedTmux` kills the server; subsequent
//!   `running()` on the same socket returns `false`.
//! - `pane_alive_false_for_unknown`: `pane_alive("%999")` returns `false` without panic.
//! - `list_panes_ordered_by_position`: panes with smaller `pane_left`/`pane_top` values
//!   appear earlier in the result of `list_panes_ordered`.
//! - `dump_tmux_tree_format`: output contains session name, attach state, window name,
//!   pane IDs, and running commands in a nested indented structure.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

/// Tmux server handle — supports isolated `-L` servers for testing.
#[derive(Debug, Clone, Default)]
pub struct Tmux {
    /// If set, uses `-L <socket> -f /dev/null` for an isolated tmux server.
    pub server_socket: Option<String>,
}

impl Tmux {
    /// Create a Tmux handle that targets the default server (user's tmux).
    pub fn default_server() -> Self {
        Tmux::default()
    }

    /// Build a tmux command with the appropriate `-L` and `-f` flags.
    pub fn cmd(&self) -> Command {
        let mut cmd = Command::new("tmux");
        if let Some(ref socket) = self.server_socket {
            cmd.args(["-L", socket, "-f", "/dev/null"]);
        }
        cmd
    }

    fn pane_dead_flag(&self, pane_id: &str) -> Option<bool> {
        let output = self
            .cmd()
            .args(["list-panes", "-a", "-F", "#{pane_id}\t#{pane_dead}"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut parts = line.splitn(2, '\t');
            let Some(id) = parts.next() else {
                continue;
            };
            if id.trim() != pane_id {
                continue;
            }
            let dead = parts.next().unwrap_or("0").trim();
            return Some(dead == "1");
        }
        None
    }

    /// Check if a tmux pane is alive.
    pub fn pane_alive(&self, pane_id: &str) -> bool {
        matches!(self.pane_dead_flag(pane_id), Some(false))
    }

    /// Check if a tmux pane still exists but its child has already exited.
    pub fn pane_dead(&self, pane_id: &str) -> bool {
        matches!(self.pane_dead_flag(pane_id), Some(true))
    }

    /// Get all alive pane IDs in a single subprocess call.
    /// Returns a HashSet for O(1) lookup. Use this instead of calling
    /// `pane_alive()` per entry to avoid N subprocess calls.
    pub fn alive_pane_ids(&self) -> std::collections::HashSet<String> {
        let output = self
            .cmd()
            .args(["list-panes", "-a", "-F", "#{pane_id}\t#{pane_dead}"])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter_map(|line| {
                        let mut parts = line.splitn(2, '\t');
                        let pane_id = parts.next()?.trim();
                        let pane_dead = parts.next().unwrap_or("0").trim();
                        if pane_id.is_empty() || pane_dead == "1" {
                            None
                        } else {
                            Some(pane_id.to_string())
                        }
                    })
                    .collect()
            }
            _ => std::collections::HashSet::new(),
        }
    }

    /// Check if a tmux server is running (has any sessions).
    pub fn running(&self) -> bool {
        self.cmd()
            .args(["has-session"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Check if a named tmux session exists.
    pub fn session_exists(&self, name: &str) -> bool {
        self.cmd()
            .args(["has-session", "-t", name])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Create a new tmux session and return the pane ID of the first pane.
    pub fn new_session(&self, name: &str, cwd: &Path) -> Result<String> {
        let output = self
            .cmd()
            .args([
                "new-session",
                "-d",
                "-s",
                name,
                "-c",
                &cwd.to_string_lossy(),
                "-P",
                "-F",
                "#{pane_id}",
            ])
            .output()
            .context("failed to create tmux session")?;
        if !output.status.success() {
            anyhow::bail!(
                "tmux new-session failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Create a new window in an existing tmux session and return the pane ID.
    pub fn new_window(&self, session: &str, cwd: &Path) -> Result<String> {
        // Append ":" to force session-name interpretation.
        // Without it, numeric names like "0" are parsed as window indices.
        let target = format!("{}:", session);
        let output = self
            .cmd()
            .args([
                "new-window",
                "-t",
                &target,
                "-c",
                &cwd.to_string_lossy(),
                "-P",
                "-F",
                "#{pane_id}",
            ])
            .output()
            .context("failed to create tmux window")?;
        if !output.status.success() {
            anyhow::bail!(
                "tmux new-window failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Send keys to a tmux pane.
    ///
    /// Uses `-l` for literal text (no special key interpretation), then sends
    /// Enter separately. A small delay between text and Enter ensures the TUI
    /// (e.g., Claude Code) processes the input before the submit.
    pub fn send_keys(&self, pane_id: &str, text: &str) -> Result<()> {
        // Send text + Enter in a single tmux command to avoid timing issues.
        // Using two separate args: first the literal text (-l), then Enter as
        // a separate non-literal key. tmux processes them atomically in one call.
        let status = self
            .cmd()
            .args(["send-keys", "-t", pane_id, "-l", text])
            .status()
            .context("failed to run tmux send-keys (text)")?;
        if !status.success() {
            anyhow::bail!("tmux send-keys failed (text)");
        }

        // Brief pause for TUI to process literal text before Enter.
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Send Enter separately
        let status = self
            .cmd()
            .args(["send-keys", "-t", pane_id, "Enter"])
            .status()
            .context("failed to run tmux send-keys (enter)")?;
        if !status.success() {
            anyhow::bail!("tmux send-keys failed (enter)");
        }
        Ok(())
    }

    /// Send a single tmux key name to a pane without literal mode.
    pub fn send_key(&self, pane_id: &str, key: &str) -> Result<()> {
        let status = self
            .cmd()
            .args(["send-keys", "-t", pane_id, key])
            .status()
            .context("failed to run tmux send-keys (single key)")?;
        if !status.success() {
            anyhow::bail!("tmux send-keys failed for pane {}", pane_id);
        }
        Ok(())
    }

    /// Select (focus) a tmux pane.
    ///
    /// Uses TmuxBatch to combine select-window + select-pane in a single
    /// tmux invocation, reducing flicker.
    pub fn select_pane(&self, pane_id: &str) -> Result<()> {
        // Batch select-window + select-pane into one invocation.
        // select-pane alone doesn't change the active window.
        let mut batch = TmuxBatch::new(self);
        batch.add(&["select-window", "-t", pane_id]);
        batch.add(&["select-pane", "-t", pane_id]);
        batch.execute()
            .with_context(|| format!("failed to select pane {}", pane_id))?;

        Ok(())
    }


    /// Split an existing pane, creating a new pane in the same window.
    ///
    /// Returns the pane ID of the newly created pane.
    /// `target_pane` is the pane to split. `cwd` sets the working directory.
    /// `flags` controls split direction: `-h` for horizontal (side-by-side),
    /// `-v` for vertical (stacked). Can include `-d` (don't focus new pane).
    pub fn split_window(&self, target_pane: &str, cwd: &Path, flags: &str) -> Result<String> {
        let output = self
            .cmd()
            .args([
                "split-window",
                "-t",
                target_pane,
                flags,
                "-c",
                &cwd.to_string_lossy(),
                "-P",
                "-F",
                "#{pane_id}",
            ])
            .output()
            .context("failed to run tmux split-window")?;
        if !output.status.success() {
            anyhow::bail!(
                "tmux split-window failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Move a pane into another pane's window with the given split direction.
    ///
    /// `split_flag` is `-h` for horizontal (side-by-side) or `-v` for vertical (stacked).
    /// Can include `-d` (don't change active pane) and `-b` (place before target).
    pub fn join_pane(&self, src_pane: &str, dst_pane: &str, split_flag: &str) -> Result<()> {
        let status = self
            .cmd()
            .args(["join-pane", "-s", src_pane, "-t", dst_pane, split_flag])
            .status()
            .context("failed to run tmux join-pane")?;
        if !status.success() {
            anyhow::bail!("tmux join-pane failed: {} → {}", src_pane, dst_pane);
        }
        Ok(())
    }

    /// Atomically swap two panes (even across windows).
    /// `-d` prevents focus change. The src pane moves to dst's position and vice versa.
    pub fn swap_pane(&self, src: &str, dst: &str) -> Result<()> {
        let status = self
            .cmd()
            .args(["swap-pane", "-s", src, "-t", dst, "-d"])
            .status()
            .context("failed to run tmux swap-pane")?;
        if !status.success() {
            anyhow::bail!("tmux swap-pane failed: {} ↔ {}", src, dst);
        }
        Ok(())
    }

    /// Break a pane out of its window into a new window.
    pub fn break_pane(&self, pane_id: &str) -> Result<()> {
        let status = self
            .cmd()
            .args(["break-pane", "-s", pane_id, "-d"])
            .status()
            .context("failed to run tmux break-pane")?;
        if !status.success() {
            anyhow::bail!("tmux break-pane failed for {}", pane_id);
        }
        Ok(())
    }

    /// Kill a tmux pane with safety guards.
    ///
    /// Refuses to kill if:
    /// - The pane is the only pane in its window AND the window is the last in its session
    ///   (killing would destroy the entire session)
    ///
    /// Returns an error instead of risking session destruction.
    pub fn kill_pane(&self, pane_id: &str) -> Result<()> {
        // Guard: check if killing this pane would destroy the session
        if let Ok(window_id) = self.pane_window(pane_id) {
            let panes = self.list_window_panes(&window_id).unwrap_or_default();
            if panes.len() <= 1 {
                // This is the last pane in its window — killing it destroys the window.
                // Check if this is the last window in the session.
                if let Ok(session_name) = self.pane_session(pane_id) {
                    let window_count = self.session_window_count(&session_name);
                    if window_count <= 1 {
                        anyhow::bail!(
                            "refusing to kill pane {} — it is the last pane in the last window of session '{}'; \
                             killing would destroy the entire session",
                            pane_id, session_name
                        );
                    }
                }
            }
        }

        let status = self
            .cmd()
            .args(["kill-pane", "-t", pane_id])
            .status()
            .context("failed to kill tmux pane")?;
        if !status.success() {
            anyhow::bail!("tmux kill-pane failed for {}", pane_id);
        }
        Ok(())
    }

    /// Count the number of windows in a session.
    pub fn session_window_count(&self, session_name: &str) -> usize {
        let output = match self
            .cmd()
            .args([
                "list-windows",
                "-t",
                &format!("{}:", session_name),
                "-F",
                "#{window_id}",
            ])
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return 0,
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .count()
    }

    /// Get the window ID that contains a pane.
    pub fn pane_window(&self, pane_id: &str) -> Result<String> {
        let output = self
            .cmd()
            .args([
                "display-message",
                "-t",
                pane_id,
                "-p",
                "#{window_id}",
            ])
            .output()
            .context("failed to run tmux display-message")?;
        if !output.status.success() {
            anyhow::bail!(
                "tmux display-message failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Get the tmux session name that contains a pane or window.
    pub fn pane_session(&self, target: &str) -> Result<String> {
        let output = self
            .cmd()
            .args([
                "display-message",
                "-t",
                target,
                "-p",
                "#{session_name}",
            ])
            .output()
            .context("failed to query tmux session name")?;
        if !output.status.success() {
            anyhow::bail!(
                "tmux display-message failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Require that a pane belongs to the expected tmux session.
    pub fn ensure_pane_in_session(&self, pane_id: &str, expected_session: &str) -> Result<()> {
        let pane_session = self.pane_session(pane_id)?;
        if pane_session != expected_session {
            anyhow::bail!(
                "pane {} is in session '{}', expected '{}' — refusing cross-session move",
                pane_id,
                pane_session,
                expected_session
            );
        }
        Ok(())
    }

    /// List all pane IDs in a given window.
    pub fn list_window_panes(&self, window_id: &str) -> Result<Vec<String>> {
        let output = self
            .cmd()
            .args([
                "list-panes",
                "-t",
                window_id,
                "-F",
                "#{pane_id}",
            ])
            .output()
            .context("failed to run tmux list-panes")?;
        if !output.status.success() {
            anyhow::bail!("tmux list-panes failed for window {}", window_id);
        }
        let panes = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        Ok(panes)
    }

    /// List panes in a window sorted by position (left-to-right, top-to-bottom).
    pub fn list_panes_ordered(&self, window_id: &str) -> Result<Vec<String>> {
        let output = self
            .cmd()
            .args([
                "list-panes",
                "-t",
                window_id,
                "-F",
                "#{pane_id} #{pane_left} #{pane_top}",
            ])
            .output()
            .context("failed to run tmux list-panes")?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        let mut panes: Vec<(String, u32, u32)> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 {
                    Some((
                        parts[0].to_string(),
                        parts[1].parse::<u32>().unwrap_or(0),
                        parts[2].parse::<u32>().unwrap_or(0),
                    ))
                } else {
                    None
                }
            })
            .collect();
        panes.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)));
        Ok(panes.into_iter().map(|(id, _, _)| id).collect())
    }

    /// Resize a pane to a specific percentage of the window.
    pub fn resize_pane(&self, pane_id: &str, flag: &str, size: u32) -> Result<()> {
        let status = self
            .cmd()
            .args([
                "resize-pane",
                "-t",
                pane_id,
                flag,
                &format!("{}%", size),
            ])
            .status()
            .context("failed to run tmux resize-pane")?;
        if !status.success() {
            anyhow::bail!("tmux resize-pane failed for pane {}", pane_id);
        }
        Ok(())
    }

    /// Query the height (rows) of a tmux window.
    pub fn window_height(&self, window_id: &str) -> Result<usize> {
        let output = self
            .cmd()
            .args(["display-message", "-t", window_id, "-p", "#{window_height}"])
            .output()
            .context("failed to run tmux display-message")?;
        if !output.status.success() {
            anyhow::bail!("tmux display-message failed for window {}", window_id);
        }
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        s.parse::<usize>().with_context(|| format!("invalid window height: {:?}", s))
    }

    /// Query the height (rows) of a tmux pane.
    pub fn pane_height(&self, pane_id: &str) -> Result<usize> {
        let output = self
            .cmd()
            .args(["display-message", "-t", pane_id, "-p", "#{pane_height}"])
            .output()
            .context("failed to run tmux display-message")?;
        if !output.status.success() {
            anyhow::bail!("tmux display-message failed for pane {}", pane_id);
        }
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        s.parse::<usize>().with_context(|| format!("invalid pane height: {:?}", s))
    }

    /// Apply a named layout to a window (e.g., "even-horizontal", "tiled").
    pub fn select_layout(&self, window_id: &str, layout: &str) -> Result<()> {
        let status = self
            .cmd()
            .args(["select-layout", "-t", window_id, layout])
            .status()
            .context("failed to run tmux select-layout")?;
        if !status.success() {
            anyhow::bail!("tmux select-layout failed for window {}", window_id);
        }
        Ok(())
    }

    /// Select a tmux window (make it the active window in its session).
    pub fn select_window(&self, window_id: &str) -> Result<()> {
        let status = self
            .cmd()
            .args(["select-window", "-t", window_id])
            .status()
            .context("failed to run tmux select-window")?;
        if !status.success() {
            anyhow::bail!("tmux select-window failed for {}", window_id);
        }
        Ok(())
    }

    /// Check if a tmux session with the given name exists.
    pub fn session_alive(&self, name: &str) -> bool {
        self.cmd()
            .args(["has-session", "-t", name])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Get the currently active pane in a session.
    pub fn active_pane(&self, session_name: &str) -> Option<String> {
        let output = self
            .cmd()
            .args([
                "display-message",
                "-t",
                &format!("{}:", session_name),
                "-p",
                "#{pane_id}",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let pane = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if pane.is_empty() { None } else { Some(pane) }
    }

    /// Get the currently active window ID for a session.
    pub fn active_window(&self, session_name: &str) -> Option<String> {
        let output = self
            .cmd()
            .args([
                "display-message",
                "-t",
                &format!("{}:", session_name),
                "-p",
                "#{window_id}",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let win = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if win.is_empty() { None } else { Some(win) }
    }

    /// Find a window named "stash" in the given tmux session.
    pub fn find_stash_window(&self, session_name: &str) -> Option<String> {
        let output = self
            .cmd()
            .args([
                "list-windows",
                "-t",
                &format!("{}:", session_name),
                "-F",
                "#{window_id} #{window_name}",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let mut parts = line.splitn(2, ' ');
            let window_id = parts.next()?;
            let window_name = parts.next().unwrap_or("");
            if window_name == "stash" {
                return Some(window_id.to_string());
            }
        }
        None
    }

    /// Ensure a stash window exists in the session. Creates if missing.
    pub fn ensure_stash_window(&self, session_name: &str) -> Result<String> {
        if let Some(w) = self.find_stash_window(session_name) {
            return Ok(w);
        }
        // Append ":" to force session-name interpretation.
        // Without it, a numeric session name like "0" is parsed as a window index,
        // causing "index 0 in use" errors.
        let target = format!("{}:", session_name);
        // Create a new detached window named "stash"
        let output = self
            .cmd()
            .args([
                "new-window",
                "-t",
                &target,
                "-n",
                "stash",
                "-d",
                "-P",
                "-F",
                "#{window_id}",
            ])
            .output()
            .context("failed to create stash window")?;
        if !output.status.success() {
            anyhow::bail!(
                "tmux new-window (stash) failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Move a pane to the stash window instead of scattering via break_pane.
    /// Falls back to break_pane if stash creation fails.
    pub fn stash_pane(&self, pane_id: &str, session_name: &str) -> Result<()> {
        let stash_window = match self.ensure_stash_window(session_name) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("warning: stash window failed ({}), falling back to break_pane", e);
                return self.break_pane(pane_id);
            }
        };
        let stash_panes = self.list_window_panes(&stash_window).unwrap_or_default();
        if !stash_panes.is_empty() {
            // Resize stash window tall enough to accept another pane.
            // Use a very large size to prevent "pane too small" errors.
            // The stash window is never displayed, so size doesn't matter visually.
            let _ = self.raw_cmd(&[
                "resize-window", "-t", &stash_window, "-y", "1000",
            ]);
            // Target the LARGEST pane in the stash to avoid "pane too small" errors.
            // tmux join-pane splits the target pane — if it's only 1 row, the join fails.
            let target = self.largest_pane_in_window(&stash_window)
                .unwrap_or_else(|| stash_panes[0].clone());
            // Use -dv: -d prevents changing the active pane, -v stacks vertically.
            // On failure: kill the pane instead of creating an orphan stash window.
            // Creating orphan windows (via break_pane) causes stash proliferation.
            match self.join_pane(pane_id, &target, "-dv") {
                Ok(()) => Ok(()),
                Err(e) => {
                    eprintln!("[stash] join-pane {} → {} failed ({}), breaking to overflow stash", pane_id, target, e);
                    self.break_pane_to_stash(pane_id, session_name)
                }
            }
        } else {
            // Empty stash window shouldn't happen (new-window creates a shell pane),
            // but create a stash overflow window just in case.
            self.break_pane_to_stash(pane_id, session_name)
        }
    }

    /// Find the largest pane (by height) in a window.
    /// Returns the pane ID with the most rows, suitable as a join target.
    pub fn largest_pane_in_window(&self, window_id: &str) -> Option<String> {
        let output = self
            .cmd()
            .args([
                "list-panes",
                "-t",
                window_id,
                "-F",
                "#{pane_id} #{pane_height}",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let id = parts.next()?.to_string();
                let height: usize = parts.next()?.parse().ok()?;
                Some((id, height))
            })
            .max_by_key(|(_, h)| *h)
            .map(|(id, _)| id)
    }

    /// Break a pane out and name the new window "stash" so it's tracked
    /// as part of the stash inventory. Used as fallback when join-pane
    /// to the primary stash window fails (pane too small).
    pub fn break_pane_to_stash(&self, pane_id: &str, _session_name: &str) -> Result<()> {
        // break-pane -d creates a new detached window
        self.break_pane(pane_id)?;
        // Find the window that now contains this pane and rename it to "stash"
        if let Ok(window_id) = self.pane_window(pane_id) {
            let _ = self.raw_cmd(&[
                "rename-window", "-t", &window_id, "stash",
            ]);
            eprintln!("[stash] created overflow stash window {} for pane {}", window_id, pane_id);
        }
        Ok(())
    }

    /// Find ALL windows named "stash" in the session (primary + overflow).
    pub fn find_all_stash_windows(&self, session_name: &str) -> Vec<String> {
        let output = match self
            .cmd()
            .args([
                "list-windows",
                "-t",
                &format!("{}:", session_name),
                "-F",
                "#{window_id} #{window_name}",
            ])
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return Vec::new(),
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(2, ' ');
                let window_id = parts.next()?;
                let window_name = parts.next().unwrap_or("");
                if window_name == "stash" {
                    Some(window_id.to_string())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Run a raw tmux command and return stdout.
    pub fn raw_cmd(&self, args: &[&str]) -> Result<String> {
        let output = self.cmd().args(args).output().context("tmux raw_cmd")?;
        if !output.status.success() {
            anyhow::bail!(
                "tmux {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// List all windows across all sessions (for global state logging).
    pub fn list_all_windows(&self) -> Result<String> {
        let output = self
            .cmd()
            .args([
                "list-windows",
                "-a",
                "-F",
                "#{window_id} #{session_name}:#{window_name} (#{window_panes} panes)",
            ])
            .output()
            .context("failed to list all tmux windows")?;
        if !output.status.success() {
            anyhow::bail!("tmux list-windows -a failed");
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// List all panes across all sessions (for global state logging).
    pub fn list_all_panes(&self) -> Result<String> {
        let output = self
            .cmd()
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{pane_id} #{window_id} #{session_name}:#{window_name}",
            ])
            .output()
            .context("failed to list all tmux panes")?;
        if !output.status.success() {
            anyhow::bail!("tmux list-panes -a failed");
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Dump the full tmux tree: sessions -> windows -> panes.
    /// Returns a formatted string for logging.
    pub fn dump_tmux_tree(&self) -> Result<String> {
        use std::collections::BTreeMap;

        // Get sessions with attach state and last activity
        let sess_out = self
            .cmd()
            .args([
                "list-sessions",
                "-F",
                "#{session_name}\t#{?session_attached,attached,detached}\t#{session_activity}",
            ])
            .output()
            .context("failed to list tmux sessions")?;
        if !sess_out.status.success() {
            anyhow::bail!("tmux list-sessions failed");
        }

        // Get all panes with full context
        let panes_out = self
            .cmd()
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{session_name}\t#{window_id}\t#{window_name}\t#{pane_id}\t#{pane_current_command}",
            ])
            .output()
            .context("failed to list tmux panes")?;
        if !panes_out.status.success() {
            anyhow::bail!("tmux list-panes -a failed");
        }

        // Parse sessions
        type WindowMap = BTreeMap<String, (String, Vec<(String, String)>)>;
        let mut sessions: BTreeMap<String, (String, String, WindowMap)> = BTreeMap::new();
        for line in String::from_utf8_lossy(&sess_out.stdout).lines() {
            let parts: Vec<&str> = line.splitn(3, '\t').collect();
            if parts.len() >= 3 {
                sessions
                    .entry(parts[0].to_string())
                    .or_insert_with(|| {
                        (parts[1].to_string(), parts[2].to_string(), BTreeMap::new())
                    });
            }
        }

        // Parse panes into tree
        for line in String::from_utf8_lossy(&panes_out.stdout).lines() {
            let parts: Vec<&str> = line.splitn(5, '\t').collect();
            if parts.len() >= 5
                && let Some((_, _, windows)) = sessions.get_mut(parts[0]) {
                    windows
                        .entry(parts[1].to_string())
                        .or_insert_with(|| (parts[2].to_string(), Vec::new()))
                        .1
                        .push((parts[3].to_string(), parts[4].to_string()));
                }
        }

        // Format tree
        let mut output = String::from("tmux tree:\n");
        for (name, (attach, activity, windows)) in &sessions {
            output.push_str(&format!("  {} ({}, activity={})\n", name, attach, activity));
            for (win_id, (win_name, panes)) in windows {
                let pane_list: Vec<String> = panes
                    .iter()
                    .map(|(id, cmd)| format!("{}({})", id, cmd))
                    .collect();
                output.push_str(&format!(
                    "    {} \"{}\" [{}]\n",
                    win_id,
                    win_name,
                    pane_list.join(", ")
                ));
            }
        }

        Ok(output)
    }

    /// Auto-start cascade: create session/window as needed, return pane ID.
    ///
    /// 1. Server not running -> create session
    /// 2. Session missing -> create session
    /// 3. Session exists -> create new window
    pub fn auto_start(&self, session_name: &str, cwd: &Path) -> Result<String> {
        if !self.running() || !self.session_exists(session_name) {
            self.new_session(session_name, cwd)
        } else {
            self.new_window(session_name, cwd)
        }
    }

    /// Capture pane content (what's visible on screen + scrollback).
    ///
    /// `lines` limits output to the last N lines of scrollback.
    /// If None, captures the visible pane content.
    pub fn capture_pane(&self, pane_id: &str, lines: Option<u32>) -> Result<String> {
        let mut args = vec!["capture-pane", "-t", pane_id, "-p"];
        let start_line;
        if let Some(n) = lines {
            start_line = format!("-{}", n);
            args.extend(["-S", &start_line]);
        }
        let output = self.cmd().args(&args).output()
            .context("failed to run tmux capture-pane")?;
        if !output.status.success() {
            anyhow::bail!(
                "tmux capture-pane failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Enable dead-pane retention on the pane's current window.
    pub fn enable_remain_on_exit(&self, pane_id: &str) -> Result<()> {
        let window_id = self.pane_window(pane_id)?;
        let status = self
            .cmd()
            .args(["set-option", "-t", &window_id, "remain-on-exit", "on"])
            .status()
            .context("failed to run tmux set-option remain-on-exit")?;
        if !status.success() {
            anyhow::bail!("tmux set-option remain-on-exit failed for {}", window_id);
        }
        Ok(())
    }

    /// Return the retained dead-pane exit status when available.
    pub fn pane_dead_status(&self, pane_id: &str) -> Result<Option<String>> {
        if !self.pane_dead(pane_id) {
            return Ok(None);
        }
        let output = self
            .cmd()
            .args(["display-message", "-t", pane_id, "-p", "#{pane_dead_status}"])
            .output()
            .context("failed to run tmux display-message for pane_dead_status")?;
        if !output.status.success() {
            anyhow::bail!(
                "tmux display-message failed for pane_dead_status on {}: {}",
                pane_id,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if status.is_empty() {
            Ok(None)
        } else {
            Ok(Some(status))
        }
    }

    /// Send raw keys to a pane without pressing Enter.
    ///
    /// Unlike `send_keys()`, this sends keystrokes without appending Enter
    /// and without `-l` (literal mode), so tmux key names like `C-c`, `Enter`,
    /// `Escape` are interpreted.
    pub fn send_keys_raw(&self, pane_id: &str, keys: &str) -> Result<()> {
        let status = self
            .cmd()
            .args(["send-keys", "-t", pane_id, keys])
            .status()
            .context("failed to run tmux send-keys (raw)")?;
        if !status.success() {
            anyhow::bail!("tmux send-keys (raw) failed");
        }
        Ok(())
    }

    /// Kill the tmux server (only useful for isolated test servers).
    pub fn kill_server(&self) -> Result<()> {
        self.cmd()
            .args(["kill-server"])
            .status()
            .context("failed to kill tmux server")?;
        Ok(())
    }
}

/// RAII guard that kills the isolated tmux server on drop.
pub struct IsolatedTmux {
    tmux: Tmux,
}

impl IsolatedTmux {
    pub fn new(name: &str) -> Self {
        IsolatedTmux {
            tmux: Tmux {
                server_socket: Some(name.to_string()),
            },
        }
    }
}

impl Drop for IsolatedTmux {
    fn drop(&mut self) {
        let _ = self.tmux.kill_server();
    }
}

impl std::ops::Deref for IsolatedTmux {
    type Target = Tmux;
    fn deref(&self) -> &Tmux {
        &self.tmux
    }
}

// ---------------------------------------------------------------------------
// TmuxBatch — fire-and-forget command batching via `\;` separator
// ---------------------------------------------------------------------------

/// Batch multiple tmux commands into a single invocation using `\;` separator.
///
/// This reduces visual flicker by executing all commands in a single tmux
/// server tick instead of spawning separate processes for each operation.
///
/// # Usage
///
/// ```no_run
/// # use tmux_router::tmux::{Tmux, TmuxBatch};
/// let tmux = Tmux::default_server();
/// let mut batch = TmuxBatch::new(&tmux);
/// batch.add(&["select-window", "-t", "%5"]);
/// batch.add(&["select-pane", "-t", "%5"]);
/// batch.add(&["send-keys", "-t", "%5", "hello", "Enter"]);
/// batch.execute().unwrap();
/// ```
pub struct TmuxBatch<'a> {
    tmux: &'a Tmux,
    commands: Vec<Vec<String>>,
}

impl<'a> TmuxBatch<'a> {
    /// Create a new empty batch.
    pub fn new(tmux: &'a Tmux) -> Self {
        Self {
            tmux,
            commands: Vec::new(),
        }
    }

    /// Add a tmux command (as argument slices) to the batch.
    pub fn add(&mut self, args: &[&str]) -> &mut Self {
        self.commands.push(args.iter().map(|s| s.to_string()).collect());
        self
    }

    /// True if no commands have been added.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Number of commands in the batch.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Execute all batched commands in a single tmux invocation.
    ///
    /// Commands are joined with `\;` (tmux command separator).
    /// Returns Ok(()) if the combined invocation succeeds.
    /// This is fire-and-forget — individual command failures are not
    /// distinguishable from the batch exit code.
    pub fn execute(&self) -> Result<()> {
        if self.commands.is_empty() {
            return Ok(());
        }

        let mut cmd = self.tmux.cmd();
        for (i, args) in self.commands.iter().enumerate() {
            if i > 0 {
                cmd.arg(";");
            }
            cmd.args(args);
        }

        let status = cmd
            .status()
            .context("failed to execute tmux batch")?;

        if !status.success() {
            anyhow::bail!("tmux batch failed (exit code {:?})", status.code());
        }
        Ok(())
    }

    /// Execute and return stdout (useful for batches ending with a display-message).
    pub fn execute_output(&self) -> Result<String> {
        if self.commands.is_empty() {
            return Ok(String::new());
        }

        let mut cmd = self.tmux.cmd();
        for (i, args) in self.commands.iter().enumerate() {
            if i > 0 {
                cmd.arg(";");
            }
            cmd.args(args);
        }

        let output = cmd
            .output()
            .context("failed to execute tmux batch")?;

        if !output.status.success() {
            anyhow::bail!("tmux batch failed (exit code {:?})", output.status.code());
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    #[test]
    fn empty_batch_is_noop() {
        let tmux = Tmux::default_server();
        let batch = TmuxBatch::new(&tmux);
        assert!(batch.is_empty());
        assert_eq!(batch.len(), 0);
        // Execute should succeed without running anything
        batch.execute().unwrap();
    }

    #[test]
    fn batch_tracks_command_count() {
        let tmux = Tmux::default_server();
        let mut batch = TmuxBatch::new(&tmux);
        batch.add(&["list-sessions"]);
        batch.add(&["list-windows"]);
        assert!(!batch.is_empty());
        assert_eq!(batch.len(), 2);
    }

}

#[cfg(test)]
mod tmux_tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    fn wait_for<F>(timeout: Duration, mut predicate: F) -> bool
    where
        F: FnMut() -> bool,
    {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        predicate()
    }

    #[test]
    fn ensure_pane_in_session_accepts_matching_session() {
        let iso = IsolatedTmux::new("tmux-ensure-session-ok");
        let pane = iso.new_session("sess-a", Path::new("/tmp")).unwrap();
        iso.ensure_pane_in_session(&pane, "sess-a").unwrap();
    }

    #[test]
    fn ensure_pane_in_session_rejects_mismatched_session() {
        let iso = IsolatedTmux::new("tmux-ensure-session-mismatch");
        let pane = iso.new_session("sess-b", Path::new("/tmp")).unwrap();
        let err = iso
            .ensure_pane_in_session(&pane, "sess-a")
            .expect_err("mismatched session should fail");
        assert!(
            err.to_string().contains("expected 'sess-a'"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn send_key_submits_buffered_input() {
        let iso = IsolatedTmux::new("tmux-send-key");
        let pane = iso.new_session("sess-a", Path::new("/tmp")).unwrap();
        iso.send_keys(&pane, "cat").unwrap();
        iso.send_keys_raw(&pane, "hello").unwrap();
        iso.send_key(&pane, "Enter").unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let content = iso.capture_pane(&pane, Some(20)).unwrap();
        assert!(
            content.contains("hello"),
            "pane should contain submitted input after send_key Enter: {content}"
        );
    }

    #[test]
    fn pane_alive_excludes_retained_dead_panes() {
        let iso = IsolatedTmux::new("tmux-pane-dead-retained");
        let pane = iso.new_session("sess-dead", Path::new("/tmp")).unwrap();
        iso.enable_remain_on_exit(&pane).unwrap();
        iso.send_keys(&pane, "exit 7").unwrap();

        assert!(
            wait_for(Duration::from_secs(3), || iso.pane_dead(&pane)),
            "pane should be retained as dead after exit"
        );
        assert!(
            !iso.pane_alive(&pane),
            "retained dead pane should not be treated as alive"
        );
        assert_eq!(iso.pane_dead_status(&pane).unwrap().as_deref(), Some("7"));
    }
}
