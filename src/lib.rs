//! Declarative tmux pane routing — sync editor layouts to tmux pane arrangements.

pub mod tmux;
pub mod registry;
pub mod sync;

pub use tmux::{Tmux, IsolatedTmux, TmuxBatch};
pub use registry::{RegistryEntry, Registry, RegistryLock, prune, with_registry, with_registry_val};
pub use sync::{FileResolution, SyncResult, sync};
