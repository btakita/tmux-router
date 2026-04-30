//! Declarative tmux pane routing — sync editor layouts to tmux pane arrangements.

pub mod pane_policy;
pub mod registry;
pub mod sync;
pub mod tmux;

pub use pane_policy::PaneMoveOp;
pub use registry::{
    Registry, RegistryEntry, RegistryLock, prune, with_registry, with_registry_val,
};
pub use sync::{FileResolution, SyncOptions, SyncResult, sync, sync_with_options};
pub use tmux::{IsolatedTmux, Tmux, TmuxBatch};
