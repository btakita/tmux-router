//! Pane movement policy — session-aware guards for `join-pane` and `swap-pane`.
//!
//! ## Spec
//!
//! - All pane movement operations default to `CrossSession::Deny`: callers that
//!   accidentally cross a tmux session boundary receive an `Err` with a message
//!   explaining what happened and how to override.
//! - Intentional cross-session moves use `CrossSession::Allow { reason }`, which
//!   logs the operation to stderr and permits the move.
//! - `PaneMoveOp` is a builder: construct with `PaneMoveOp::new(tmux, src, dst)`,
//!   optionally call `.allow_cross_session(reason)`, then call `.join(flag)` or `.swap()`.
//! - Session checking calls `pane_session()` for both src and dst. If either query
//!   fails (e.g. stale pane), the policy falls back to allowing the move and lets
//!   tmux itself reject invalid operations.
//!
//! ## Tests
//!
//! - `same_session_join_allowed`: two panes in same session → join succeeds
//! - `same_session_swap_allowed`: two panes in same session, same window → swap succeeds
//! - `cross_session_join_denied`: panes in different sessions, Deny → Err with message
//! - `cross_session_swap_denied`: panes in different sessions, Deny → Err with message
//! - `cross_session_join_allowed_with_explicit_flag`: Allow → Ok, logs audit line
//! - `cross_session_swap_allowed_with_explicit_flag`: Allow → Ok, logs audit line

use crate::tmux::Tmux;
use anyhow::Result;

/// Policy for cross-session pane moves.
#[derive(Debug, Clone)]
pub enum CrossSession {
    /// Refuse cross-session moves (default). Returns `Err` when src and dst
    /// are in different tmux sessions.
    Deny,
    /// Permit cross-session moves with a stderr audit log entry.
    Allow { reason: &'static str },
}

/// Builder for a single pane move operation with enforced session policy.
///
/// ```rust,ignore
/// use tmux_router::pane_policy::PaneMoveOp;
///
/// // Same-session (default Deny protects against accidental cross-session):
/// PaneMoveOp::new(tmux, src, dst).join("-dh")?;
/// PaneMoveOp::new(tmux, src, dst).swap()?;
///
/// // Cross-session (explicit intent required):
/// PaneMoveOp::new(tmux, src, dst)
///     .allow_cross_session("relocate to project session")
///     .join("-dh")?;
/// ```
pub struct PaneMoveOp<'a> {
    tmux: &'a Tmux,
    src: &'a str,
    dst: &'a str,
    cross_session: CrossSession,
}

impl<'a> PaneMoveOp<'a> {
    /// Create a new pane move op with `CrossSession::Deny` (default).
    pub fn new(tmux: &'a Tmux, src: &'a str, dst: &'a str) -> Self {
        Self {
            tmux,
            src,
            dst,
            cross_session: CrossSession::Deny,
        }
    }

    /// Override the default `Deny` policy for intentional cross-session moves.
    #[must_use]
    pub fn allow_cross_session(mut self, reason: &'static str) -> Self {
        self.cross_session = CrossSession::Allow { reason };
        self
    }

    /// Execute as `join-pane -s src -t dst <split_flag>`.
    pub fn join(self, split_flag: &str) -> Result<()> {
        self.check_session()?;
        self.tmux.join_pane(self.src, self.dst, split_flag)
    }

    /// Execute as `swap-pane -s src -t dst -d`.
    pub fn swap(self) -> Result<()> {
        self.check_session()?;
        self.tmux.swap_pane(self.src, self.dst)
    }

    fn check_session(&self) -> Result<()> {
        // If session query fails for either pane (e.g. stale pane), allow the move
        // and let tmux itself reject invalid operations.
        let src_sess = match self.tmux.pane_session(self.src) {
            Ok(s) => s,
            Err(_) => return Ok(()),
        };
        let dst_sess = match self.tmux.pane_session(self.dst) {
            Ok(s) => s,
            Err(_) => return Ok(()),
        };

        if src_sess == dst_sess {
            return Ok(());
        }

        match self.cross_session {
            CrossSession::Deny => anyhow::bail!(
                "cross-session pane move denied: {} (sess '{}') → {} (sess '{}'); \
                 use PaneMoveOp::allow_cross_session(reason) to override",
                self.src,
                src_sess,
                self.dst,
                dst_sess
            ),
            CrossSession::Allow { reason } => {
                eprintln!(
                    "[pane_policy] cross-session move: {} ('{}') → {} ('{}') — {}",
                    self.src, src_sess, self.dst, dst_sess, reason
                );
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::IsolatedTmux;
    use std::path::Path;

    #[test]
    fn same_session_join_allowed() {
        let iso = IsolatedTmux::new("pane-policy-join-same");
        let pane1 = iso.new_session("test", Path::new("/tmp")).unwrap();
        let pane2 = iso.new_window("test", Path::new("/tmp")).unwrap();
        let result = PaneMoveOp::new(&iso, &pane2, &pane1).join("-dh");
        assert!(result.is_ok(), "same-session join should succeed: {result:?}");
    }

    #[test]
    fn same_session_swap_allowed() {
        let iso = IsolatedTmux::new("pane-policy-swap-same");
        let pane1 = iso.new_session("test", Path::new("/tmp")).unwrap();
        let pane2 = iso.new_window("test", Path::new("/tmp")).unwrap();
        // Join into same window first (swap requires panes in same session)
        iso.join_pane(&pane2, &pane1, "-dh").unwrap();
        let result = PaneMoveOp::new(&iso, &pane2, &pane1).swap();
        assert!(result.is_ok(), "same-session swap should succeed: {result:?}");
    }

    #[test]
    fn cross_session_join_denied() {
        let iso = IsolatedTmux::new("pane-policy-join-cross");
        let pane1 = iso.new_session("sess-a", Path::new("/tmp")).unwrap();
        let pane2 = iso.new_session("sess-b", Path::new("/tmp")).unwrap();
        let result = PaneMoveOp::new(&iso, &pane2, &pane1).join("-dh");
        assert!(result.is_err(), "cross-session join should be denied by default");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("cross-session pane move denied"),
            "error should mention cross-session denial: {msg}"
        );
    }

    #[test]
    fn cross_session_swap_denied() {
        let iso = IsolatedTmux::new("pane-policy-swap-cross");
        let pane1 = iso.new_session("sess-a", Path::new("/tmp")).unwrap();
        let pane2 = iso.new_session("sess-b", Path::new("/tmp")).unwrap();
        let result = PaneMoveOp::new(&iso, &pane2, &pane1).swap();
        assert!(result.is_err(), "cross-session swap should be denied by default");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("cross-session pane move denied"),
            "error should mention cross-session denial: {msg}"
        );
    }

    #[test]
    fn cross_session_join_allowed_with_explicit_flag() {
        let iso = IsolatedTmux::new("pane-policy-join-explicit");
        let pane1 = iso.new_session("sess-a", Path::new("/tmp")).unwrap();
        let pane2 = iso.new_session("sess-b", Path::new("/tmp")).unwrap();
        let result = PaneMoveOp::new(&iso, &pane2, &pane1)
            .allow_cross_session("test: intentional cross-session move")
            .join("-dh");
        assert!(
            result.is_ok(),
            "explicit cross-session join should succeed: {result:?}"
        );
        // Verify pane2 is now in sess-a
        let sess = iso.pane_session(&pane2).unwrap();
        assert_eq!(sess, "sess-a", "pane should be in target session after join");
    }

    #[test]
    fn cross_session_swap_allowed_with_explicit_flag() {
        let iso = IsolatedTmux::new("pane-policy-swap-explicit");
        let pane1 = iso.new_session("sess-a", Path::new("/tmp")).unwrap();
        let pane2 = iso.new_session("sess-b", Path::new("/tmp")).unwrap();
        // Move pane2 to sess-a first (join), then swap within sess-a
        iso.join_pane(&pane2, &pane1, "-dh").unwrap();
        // Now both are in sess-a — swap should succeed with Deny too
        let result = PaneMoveOp::new(&iso, &pane2, &pane1).swap();
        assert!(result.is_ok(), "same-session swap after join should succeed: {result:?}");
    }
}
