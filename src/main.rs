use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use tmux_router::tmux::Tmux;
use tmux_router::registry;
use tmux_router::sync::FileResolution;

/// Declarative tmux pane routing — sync editor layouts to tmux.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Registry file path
    #[arg(long, default_value = ".tmux-router/registry.json")]
    registry: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sync editor layout to tmux panes.
    Sync {
        /// Column of comma-separated files (repeat for multiple columns).
        #[arg(long = "col", required = true)]
        cols: Vec<String>,
        /// Target tmux window ID (e.g. @1).
        #[arg(long)]
        window: Option<String>,
        /// File to focus after sync.
        #[arg(long)]
        focus: Option<String>,
        /// Tmux session name.
        #[arg(long)]
        session: Option<String>,
    },
    /// Register a file to a tmux pane.
    Register {
        /// File path (used as registry key).
        file: String,
        /// Tmux pane ID (e.g. %5).
        pane: String,
    },
    /// Unregister a file from the registry.
    Unregister {
        /// File path to remove.
        file: String,
    },
    /// Show registry contents and pane health.
    Status,
    /// Prune dead panes from registry.
    Resync,
    /// Focus (select) the pane registered to a file.
    Focus {
        /// File path to focus.
        file: String,
    },
    /// Send text to a tmux pane (by pane ID or registered file name).
    Send {
        /// Target: pane ID (e.g. %5) or registered file name.
        target: String,
        /// Text to send.
        text: String,
        /// Don't press Enter after sending.
        #[arg(long)]
        no_enter: bool,
        /// Send as raw tmux keys (interpret key names like C-c, Escape).
        #[arg(long)]
        raw: bool,
    },
    /// Capture pane content (screen buffer).
    Capture {
        /// Target: pane ID (e.g. %5) or registered file name.
        target: String,
        /// Number of scrollback lines to capture.
        #[arg(long)]
        lines: Option<u32>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let tmux = Tmux::default_server();

    match cli.command {
        Command::Sync { cols, window, focus, session: _ } => {
            let resolve = |_path: &std::path::Path| -> Option<FileResolution> {
                // Standalone CLI: all files are unmanaged (no UUID mapping).
                // Users can register files first, then sync will find them.
                Some(FileResolution::Unmanaged)
            };
            let result = tmux_router::sync(
                &cols,
                window.as_deref(),
                focus.as_deref(),
                &tmux,
                &cli.registry,
                &resolve,
            )?;
            println!("synced to window {}", result.target_window);
            if let Some(session) = result.target_session {
                println!("session: {}", session);
            }
        }
        Command::Register { file, pane } => {
            let mut reg = registry::load_registry(&cli.registry)?;
            reg.insert(file.clone(), registry::RegistryEntry {
                pane: pane.clone(),
                pid: 0,
                cwd: String::new(),
                started: chrono_now(),
                file: file.clone(),
                window: tmux.pane_window(&pane).unwrap_or_default(),
            });
            registry::save_registry(&cli.registry, &reg)?;
            println!("registered {} → {}", file, pane);
        }
        Command::Unregister { file } => {
            let mut reg = registry::load_registry(&cli.registry)?;
            if reg.remove(&file).is_some() {
                registry::save_registry(&cli.registry, &reg)?;
                println!("unregistered {}", file);
            } else {
                println!("{} not found in registry", file);
            }
        }
        Command::Status => {
            let reg = registry::load_registry(&cli.registry)?;
            if reg.is_empty() {
                println!("registry is empty");
                return Ok(());
            }
            for (key, entry) in &reg {
                let alive = if tmux.pane_alive(&entry.pane) { "alive" } else { "dead" };
                println!("{} → {} [{}] window={}", key, entry.pane, alive, entry.window);
            }
        }
        Command::Resync => {
            let removed = registry::prune(&cli.registry, &tmux)?;
            println!("pruned {} dead entries", removed);
        }
        Command::Focus { file } => {
            let reg = registry::load_registry(&cli.registry)?;
            match reg.get(&file) {
                Some(entry) => {
                    tmux.select_pane(&entry.pane)?;
                    println!("focused {} (pane {})", file, entry.pane);
                }
                None => {
                    anyhow::bail!("{} not found in registry", file);
                }
            }
        }
        Command::Send { target, text, no_enter, raw } => {
            let pane = resolve_target(&cli.registry, &target)?;
            if raw {
                tmux.send_keys_raw(&pane, &text)?;
            } else if no_enter {
                let status = tmux.cmd()
                    .args(["send-keys", "-t", &pane, "-l", &text])
                    .status()?;
                if !status.success() {
                    anyhow::bail!("send-keys failed");
                }
            } else {
                tmux.send_keys(&pane, &text)?;
            }
        }
        Command::Capture { target, lines } => {
            let pane = resolve_target(&cli.registry, &target)?;
            let content = tmux.capture_pane(&pane, lines)?;
            print!("{}", content);
        }
    }
    Ok(())
}

/// Resolve a target string to a pane ID.
/// If it starts with `%` it's already a pane ID; otherwise look it up in the registry.
fn resolve_target(registry_path: &std::path::Path, target: &str) -> Result<String> {
    if target.starts_with('%') {
        return Ok(target.to_string());
    }
    let reg = registry::load_registry(registry_path)?;
    match reg.get(target) {
        Some(entry) => Ok(entry.pane.clone()),
        None => anyhow::bail!("'{}' not found in registry (use %<id> for raw pane IDs)", target),
    }
}

/// Simple timestamp for registry entries.
fn chrono_now() -> String {
    let output = std::process::Command::new("date")
        .args(["+%Y-%m-%dT%H:%M:%S"])
        .output();
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::from("unknown"),
    }
}
