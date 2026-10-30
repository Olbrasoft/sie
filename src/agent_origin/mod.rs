//! Route agent questions to one live terminal using metadata and stable identities.
mod desktop;
mod model;
mod processes;
mod resolve;
mod tmux;
pub use desktop::{activate, snapshot};
pub use model::{Client, Origin, Pane, Process, Question, Snapshot};
pub use resolve::resolve;
pub use tmux::{parse_clients, parse_panes, tmux};
