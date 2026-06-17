//! Hybrid tmux/PTY pane architecture helpers.
//!
//! The hybrid contract keeps tmux as the visible multiplexer while separating
//! the three jobs that were historically conflated:
//!
//! - control mode observes pane lifecycle and command events;
//! - `pipe-pane` streams live pane output without snapshot polling;
//! - owned PTYs are reserved for managed harnesses that need byte-exact input.

use crate::tmux::Tmux;
use anyhow::{Context, Result};
use std::fmt;
use std::path::{Path, PathBuf};

/// Lifecycle observation backend for a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneLifecycleBackend {
    /// Long-lived `tmux -C` control-mode client.
    ControlMode,
    /// One-shot tmux commands, retained as a fallback for minimal environments.
    TmuxCli,
}

/// Live output streaming backend for a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneOutputBackend {
    /// `tmux pipe-pane` writes live pane output to a process/file.
    PipePane,
    /// Control-mode `%output` events.
    ControlModeOutput,
    /// Snapshot fallback using `capture-pane`.
    CapturePane,
}

/// Input submission backend for a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneInputBackend {
    /// Literal text submitted through tmux with a named Enter key.
    TmuxEnter,
    /// Literal text submitted as one payload ending in a Kitty keyboard-protocol Return sequence.
    TmuxKittyReturn,
    /// Supervisor-owned child PTY byte stream.
    OwnedPty,
}

impl fmt::Display for PaneInputBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaneInputBackend::TmuxEnter => write!(f, "tmux-enter"),
            PaneInputBackend::TmuxKittyReturn => write!(f, "tmux-kitty-return"),
            PaneInputBackend::OwnedPty => write!(f, "owned-pty"),
        }
    }
}

/// The selected strategy for a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HybridPaneStrategy {
    pub lifecycle: PaneLifecycleBackend,
    pub output: PaneOutputBackend,
    pub input: PaneInputBackend,
}

/// Pick the hybrid strategy for a harness.
///
/// Managed Claude/OpenCode sessions can use an owned PTY for byte-exact input.
/// Unmanaged tmux panes still submit through tmux, with OpenCode using the
/// Kitty Return fallback because its TUI can distinguish Return from ctrl-j.
pub fn strategy_for_harness(harness: &str, managed: bool) -> HybridPaneStrategy {
    let normalized = normalize_harness(harness);
    let input = if managed && matches!(normalized, "claude" | "claude-code" | "opencode") {
        PaneInputBackend::OwnedPty
    } else if normalized == "opencode" {
        PaneInputBackend::TmuxKittyReturn
    } else {
        PaneInputBackend::TmuxEnter
    };

    HybridPaneStrategy {
        lifecycle: PaneLifecycleBackend::ControlMode,
        output: PaneOutputBackend::PipePane,
        input,
    }
}

/// Pick the tmux-backed submit fallback for a harness.
pub fn tmux_submit_backend_for_harness(harness: &str) -> PaneInputBackend {
    if normalize_harness(harness) == "opencode" {
        PaneInputBackend::TmuxKittyReturn
    } else {
        PaneInputBackend::TmuxEnter
    }
}

/// Submit one single-line command through a tmux pane using the harness-aware
/// tmux fallback. Owned-PTY callers should write to their PTY directly and use
/// this only when the pane itself is the delivery boundary.
pub fn submit_text_for_harness(
    tmux: &Tmux,
    pane_id: &str,
    text: &str,
    harness: &str,
) -> Result<()> {
    match tmux_submit_backend_for_harness(harness) {
        PaneInputBackend::TmuxEnter => tmux.send_keys(pane_id, text),
        PaneInputBackend::TmuxKittyReturn => tmux.send_keys_with_kitty_return(pane_id, text),
        PaneInputBackend::OwnedPty => unreachable!("owned PTY is not a tmux submit fallback"),
    }
}

fn normalize_harness(harness: &str) -> &str {
    match harness {
        "claude-code" => "claude-code",
        other => other,
    }
}

/// Active `pipe-pane` stream. Dropping the guard disables the pane pipe.
pub struct TmuxPipePane {
    tmux: Tmux,
    pane_id: String,
    output_path: PathBuf,
}

impl TmuxPipePane {
    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }

    pub fn output_path(&self) -> &Path {
        &self.output_path
    }
}

impl Drop for TmuxPipePane {
    fn drop(&mut self) {
        let _ = self.tmux.stop_pipe_pane(&self.pane_id);
    }
}

impl Tmux {
    /// Stream future pane output into `output_path` with `tmux pipe-pane`.
    pub fn start_pipe_pane_to_file(
        &self,
        pane_id: &str,
        output_path: &Path,
        append: bool,
    ) -> Result<TmuxPipePane> {
        let args = pipe_pane_file_args(pane_id, output_path, append);
        let status = self
            .cmd()
            .args(args.iter().map(String::as_str))
            .status()
            .context("failed to run tmux pipe-pane")?;
        if !status.success() {
            anyhow::bail!("tmux pipe-pane failed for pane {}", pane_id);
        }
        Ok(TmuxPipePane {
            tmux: self.clone(),
            pane_id: pane_id.to_string(),
            output_path: output_path.to_path_buf(),
        })
    }

    /// Disable any active `pipe-pane` command for `pane_id`.
    pub fn stop_pipe_pane(&self, pane_id: &str) -> Result<()> {
        let status = self
            .cmd()
            .args(["pipe-pane", "-t", pane_id])
            .status()
            .context("failed to stop tmux pipe-pane")?;
        if !status.success() {
            anyhow::bail!("tmux pipe-pane stop failed for pane {}", pane_id);
        }
        Ok(())
    }
}

fn pipe_pane_file_args(pane_id: &str, output_path: &Path, append: bool) -> Vec<String> {
    let redirect = if append { ">>" } else { ">" };
    vec![
        "pipe-pane".to_string(),
        "-t".to_string(),
        pane_id.to_string(),
        format!("cat {redirect} {}", shell_quote_path(output_path)),
    ]
}

fn shell_quote_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    format!("'{}'", raw.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::IsolatedTmux;
    use std::time::{Duration, Instant};

    #[test]
    fn hybrid_strategy_scopes_owned_pty_to_managed_input() {
        assert_eq!(
            strategy_for_harness("opencode", true).input,
            PaneInputBackend::OwnedPty
        );
        assert_eq!(
            strategy_for_harness("claude", true).input,
            PaneInputBackend::OwnedPty
        );
        assert_eq!(
            strategy_for_harness("codex", true).input,
            PaneInputBackend::TmuxEnter
        );
        assert_eq!(
            strategy_for_harness("opencode", false).input,
            PaneInputBackend::TmuxKittyReturn
        );
    }

    #[test]
    fn pipe_pane_file_args_quote_paths() {
        let args = pipe_pane_file_args("%7", Path::new("/tmp/agent doc/o'hai.log"), true);
        assert_eq!(
            args,
            vec![
                "pipe-pane",
                "-t",
                "%7",
                "cat >> '/tmp/agent doc/o'\\''hai.log'"
            ]
        );
    }

    #[test]
    fn pipe_pane_streams_output_without_capture_polling() {
        let tmp = tempfile::tempdir().unwrap();
        let iso = IsolatedTmux::new("tmux-pipe-pane-output");
        let pane = iso.new_session("sess-pipe", tmp.path()).unwrap();
        let output = tmp.path().join("pane.log");
        let _pipe = iso.start_pipe_pane_to_file(&pane, &output, false).unwrap();

        iso.send_keys(&pane, "printf pipe-pane-ready").unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut content = String::new();
        while Instant::now() < deadline {
            content = std::fs::read_to_string(&output).unwrap_or_default();
            if content.contains("pipe-pane-ready") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(
            content.contains("pipe-pane-ready"),
            "pipe-pane should stream live pane output, got: {content:?}"
        );
    }
}
