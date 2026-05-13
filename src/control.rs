//! Tmux control-mode event stream.
//!
//! Control mode (`tmux -C`) keeps one tmux client open and emits structured
//! notifications such as pane output and pane lifecycle changes. It is the
//! event-driven counterpart to snapshot helpers such as `capture-pane`.

use crate::tmux::Tmux;
use anyhow::{Context, Result};
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// One parsed line from a tmux control-mode client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxControlEvent {
    /// `%begin <time> <number> <flags>`
    Begin {
        timestamp: String,
        number: String,
        flags: String,
    },
    /// `%end <time> <number> <flags>`
    End {
        timestamp: String,
        number: String,
        flags: String,
    },
    /// `%error <time> <number> <flags> <message>`
    Error {
        timestamp: String,
        number: String,
        flags: String,
        message: String,
    },
    /// `%output <pane-id> <escaped-bytes>`
    Output { pane_id: String, bytes: Vec<u8> },
    /// Pane lifecycle notifications such as `%pane-died` or `%pane-exited`.
    PaneLifecycle {
        name: String,
        pane_id: Option<String>,
        args: Vec<String>,
    },
    /// Other `%name ...` notifications.
    Notification { name: String, args: Vec<String> },
    /// `%exit`
    Exit,
    /// A line that did not match tmux control-mode syntax.
    Unknown(String),
}

impl fmt::Display for TmuxControlEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TmuxControlEvent::Begin {
                timestamp,
                number,
                flags,
            } => write!(f, "%begin {timestamp} {number} {flags}"),
            TmuxControlEvent::End {
                timestamp,
                number,
                flags,
            } => write!(f, "%end {timestamp} {number} {flags}"),
            TmuxControlEvent::Error {
                timestamp,
                number,
                flags,
                message,
            } => write!(f, "%error {timestamp} {number} {flags} {message}"),
            TmuxControlEvent::Output { pane_id, bytes } => {
                write!(f, "%output {pane_id} {}", String::from_utf8_lossy(bytes))
            }
            TmuxControlEvent::PaneLifecycle {
                name,
                pane_id,
                args,
            } => {
                write!(f, "%{name}")?;
                if let Some(pane_id) = pane_id
                    && !args.iter().any(|arg| arg == pane_id)
                {
                    write!(f, " {pane_id}")?;
                }
                for arg in args {
                    write!(f, " {arg}")?;
                }
                Ok(())
            }
            TmuxControlEvent::Notification { name, args } => {
                write!(f, "%{name}")?;
                for arg in args {
                    write!(f, " {arg}")?;
                }
                Ok(())
            }
            TmuxControlEvent::Exit => write!(f, "%exit"),
            TmuxControlEvent::Unknown(line) => write!(f, "{line}"),
        }
    }
}

/// Live control-mode client.
pub struct TmuxControlMode {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<TmuxControlEvent>,
    reader: Option<JoinHandle<()>>,
}

impl TmuxControlMode {
    /// Send one tmux command through the control-mode client.
    ///
    /// The command uses tmux command syntax, for example
    /// `display-message -p "#{pane_id}"`.
    pub fn send_command(&mut self, command: &str) -> Result<()> {
        writeln!(self.stdin, "{command}").context("failed to write tmux control command")?;
        self.stdin
            .flush()
            .context("failed to flush tmux control command")?;
        Ok(())
    }

    /// Read the next event, returning `Ok(None)` when the timeout expires.
    pub fn next_event_timeout(&self, timeout: Duration) -> Result<Option<TmuxControlEvent>> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("tmux control-mode reader disconnected")
            }
        }
    }

    /// Wait for a matching event without polling `capture-pane`.
    pub fn wait_for_event(
        &self,
        timeout: Duration,
        mut predicate: impl FnMut(&TmuxControlEvent) -> bool,
    ) -> Result<Option<TmuxControlEvent>> {
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let Some(event) = self.next_event_timeout(deadline - now)? else {
                return Ok(None);
            };
            if predicate(&event) {
                return Ok(Some(event));
            }
        }
    }

    /// Wait until a pane emits output matching `predicate`.
    pub fn wait_for_pane_output(
        &self,
        pane_id: &str,
        timeout: Duration,
        mut predicate: impl FnMut(&[u8]) -> bool,
    ) -> Result<Option<Vec<u8>>> {
        let Some(event) = self.wait_for_event(timeout, |event| {
            matches!(
                event,
                TmuxControlEvent::Output { pane_id: id, bytes }
                    if id == pane_id && predicate(bytes)
            )
        })?
        else {
            return Ok(None);
        };
        match event {
            TmuxControlEvent::Output { bytes, .. } => Ok(Some(bytes)),
            _ => Ok(None),
        }
    }
}

impl Drop for TmuxControlMode {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Tmux {
    /// Attach a tmux control-mode client.
    ///
    /// When `target` is provided it is passed to `attach-session -t <target>`.
    /// The returned reader receives `%output` and lifecycle notifications from
    /// tmux without repeatedly invoking `capture-pane`.
    pub fn attach_control_mode(&self, target: Option<&str>) -> Result<TmuxControlMode> {
        let mut command = self.cmd();
        command.arg("-C");
        if let Some(target) = target {
            command.args(["attach-session", "-t", target]);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = command
            .spawn()
            .context("failed to start tmux control mode")?;
        let stdin = child
            .stdin
            .take()
            .context("tmux control mode stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("tmux control mode stdout unavailable")?;

        let (tx, rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else {
                    break;
                };
                if tx.send(parse_control_event(&line)).is_err() {
                    break;
                }
            }
        });

        Ok(TmuxControlMode {
            child,
            stdin,
            events: rx,
            reader: Some(reader),
        })
    }
}

/// Parse one control-mode line.
pub fn parse_control_event(line: &str) -> TmuxControlEvent {
    let line = line.trim_end_matches(['\r', '\n']);
    if line == "%exit" {
        return TmuxControlEvent::Exit;
    }
    let Some(rest) = line.strip_prefix('%') else {
        return TmuxControlEvent::Unknown(line.to_string());
    };
    let mut parts = rest.splitn(2, ' ');
    let name = parts.next().unwrap_or_default();
    let remainder = parts.next().unwrap_or_default();

    match name {
        "begin" => parse_command_boundary(remainder, |timestamp, number, flags| {
            TmuxControlEvent::Begin {
                timestamp,
                number,
                flags,
            }
        }),
        "end" => parse_command_boundary(remainder, |timestamp, number, flags| {
            TmuxControlEvent::End {
                timestamp,
                number,
                flags,
            }
        }),
        "error" => parse_error(remainder),
        "output" => parse_output(remainder),
        name if name.starts_with("pane-") => parse_pane_lifecycle(name, remainder),
        name => TmuxControlEvent::Notification {
            name: name.to_string(),
            args: split_args(remainder),
        },
    }
}

fn parse_command_boundary(
    remainder: &str,
    build: impl FnOnce(String, String, String) -> TmuxControlEvent,
) -> TmuxControlEvent {
    let mut fields = remainder.split_whitespace();
    let timestamp = fields.next().unwrap_or_default().to_string();
    let number = fields.next().unwrap_or_default().to_string();
    let flags = fields.next().unwrap_or_default().to_string();
    build(timestamp, number, flags)
}

fn parse_error(remainder: &str) -> TmuxControlEvent {
    let mut fields = remainder.splitn(4, ' ');
    let timestamp = fields.next().unwrap_or_default().to_string();
    let number = fields.next().unwrap_or_default().to_string();
    let flags = fields.next().unwrap_or_default().to_string();
    let message = fields.next().unwrap_or_default().to_string();
    TmuxControlEvent::Error {
        timestamp,
        number,
        flags,
        message,
    }
}

fn parse_output(remainder: &str) -> TmuxControlEvent {
    let mut fields = remainder.splitn(2, ' ');
    let pane_id = fields.next().unwrap_or_default().to_string();
    let payload = fields.next().unwrap_or_default();
    TmuxControlEvent::Output {
        pane_id,
        bytes: decode_control_payload(payload),
    }
}

fn parse_pane_lifecycle(name: &str, remainder: &str) -> TmuxControlEvent {
    let args = split_args(remainder);
    let pane_id = args.iter().find(|arg| arg.starts_with('%')).cloned();
    TmuxControlEvent::PaneLifecycle {
        name: name.to_string(),
        pane_id,
        args,
    }
}

fn split_args(remainder: &str) -> Vec<String> {
    remainder
        .split_whitespace()
        .map(ToString::to_string)
        .collect()
}

/// Decode the escaped payload used by `%output` events.
pub fn decode_control_payload(payload: &str) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(payload.len());
    let bytes = payload.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            decoded.push(bytes[i]);
            i += 1;
            continue;
        }

        if i + 3 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
            && bytes[i + 3].is_ascii_digit()
        {
            let value =
                (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + bytes[i + 3] - b'0';
            decoded.push(value);
            i += 4;
            continue;
        }

        match bytes[i + 1] {
            b'\\' => decoded.push(b'\\'),
            b'n' => decoded.push(b'\n'),
            b'r' => decoded.push(b'\r'),
            b't' => decoded.push(b'\t'),
            other => decoded.push(other),
        }
        i += 2;
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::IsolatedTmux;
    use std::path::Path;

    #[test]
    fn parses_control_output_payload() {
        assert_eq!(
            parse_control_event(r"%output %7 hello\015\012world"),
            TmuxControlEvent::Output {
                pane_id: "%7".to_string(),
                bytes: b"hello\r\nworld".to_vec(),
            }
        );
    }

    #[test]
    fn parses_pane_lifecycle_notification() {
        assert_eq!(
            parse_control_event("%pane-died %12 1"),
            TmuxControlEvent::PaneLifecycle {
                name: "pane-died".to_string(),
                pane_id: Some("%12".to_string()),
                args: vec!["%12".to_string(), "1".to_string()],
            }
        );
    }

    #[test]
    fn parses_command_boundaries() {
        assert_eq!(
            parse_control_event("%begin 1710000000 3 1"),
            TmuxControlEvent::Begin {
                timestamp: "1710000000".to_string(),
                number: "3".to_string(),
                flags: "1".to_string(),
            }
        );
        assert_eq!(
            parse_control_event("%end 1710000000 3 1"),
            TmuxControlEvent::End {
                timestamp: "1710000000".to_string(),
                number: "3".to_string(),
                flags: "1".to_string(),
            }
        );
    }

    #[test]
    fn control_mode_receives_pane_output_without_capture_polling() {
        let iso = IsolatedTmux::new("tmux-control-output");
        let pane = iso.new_session("sess-control", Path::new("/tmp")).unwrap();
        let control = iso.attach_control_mode(Some("sess-control")).unwrap();

        iso.send_keys(&pane, "printf control-mode-ready").unwrap();
        let output = control
            .wait_for_pane_output(&pane, Duration::from_secs(3), |bytes| {
                String::from_utf8_lossy(bytes).contains("control-mode-ready")
            })
            .unwrap();

        assert!(
            output.is_some(),
            "control mode should observe pane output without capture-pane polling"
        );
    }
}
