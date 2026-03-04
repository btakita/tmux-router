//! Declarative tmux pane routing — sync editor layouts to tmux pane arrangements.

pub mod tmux;
pub mod registry;
pub mod sync;

pub use tmux::{Tmux, IsolatedTmux};
pub use registry::{RegistryEntry, Registry};
pub use sync::{FileResolution, SyncResult, sync};
