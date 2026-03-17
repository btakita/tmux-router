//! Tmux server handle — supports isolated `-L` servers for testing.

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

    /// Check if a tmux pane is alive.
    pub fn pane_alive(&self, pane_id: &str) -> bool {
        let output = self
            .cmd()
            .args(["list-panes", "-a", "-F", "#{pane_id}"])
            .output();
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                stdout.lines().any(|line| line.trim() == pane_id)
            }
            Err(_) => false,
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
        let output = self
            .cmd()
            .args([
                "new-window",
                "-a",
                "-t",
                session,
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

    /// Select (focus) a tmux pane.
    pub fn select_pane(&self, pane_id: &str) -> Result<()> {
        // Switch to the window containing the pane first (select-pane alone
        // doesn't change the active window).
        let status = self
            .cmd()
            .args(["select-window", "-t", pane_id])
            .status()
            .context("failed to run tmux select-window")?;
        if !status.success() {
            anyhow::bail!("tmux select-window failed for {}", pane_id);
        }
        let status = self
            .cmd()
            .args(["select-pane", "-t", pane_id])
            .status()
            .context("failed to run tmux select-pane")?;
        if !status.success() {
            anyhow::bail!("tmux select-pane failed for {}", pane_id);
        }

        // Log the session for debugging. select-window + select-pane already
        // switched the active pane within the session. We do NOT switch-client
        // because that would force ALL terminal clients to jump sessions,
        // disrupting the user's layout.
        if let Ok(output) = self
            .cmd()
            .args(["display-message", "-t", pane_id, "-p", "#{session_name}:#{window_index}"])
            .output()
        {
            let info = String::from_utf8_lossy(&output.stdout).trim().to_string();
            eprintln!("[tmux] select_pane {} → session:window {}", pane_id, info);
        }
        Ok(())
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

    /// Kill a tmux pane.
    pub fn kill_pane(&self, pane_id: &str) -> Result<()> {
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
                session_name,
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

    /// Find a window named "stash" in the given tmux session.
    pub fn find_stash_window(&self, session_name: &str) -> Option<String> {
        let output = self
            .cmd()
            .args([
                "list-windows",
                "-t",
                session_name,
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
        if let Some(target) = stash_panes.first() {
            // Resize stash window tall enough to accept another pane.
            // tmux enforces a minimum per-pane height that varies by content,
            // so use a generous fixed size. The stash window is never displayed.
            let _ = self.raw_cmd(&[
                "resize-window", "-t", &stash_window, "-y", "200",
            ]);
            // Use -dv: -d prevents changing the active pane, -v stacks vertically.
            // Fall back to break_pane if join still fails.
            match self.join_pane(pane_id, target, "-dv") {
                Ok(()) => Ok(()),
                Err(_) => self.break_pane(pane_id),
            }
        } else {
            // Empty stash window shouldn't happen (new-window creates a shell pane),
            // but fall back to break_pane just in case.
            self.break_pane(pane_id)
        }
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
