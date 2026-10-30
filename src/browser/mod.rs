//! Local browser broker with independently testable wire and host adapters.
mod broker;
mod host;
mod wire;
pub use broker::{Broker, INTERFACE, PATH, Tab, export};
pub use host::host;
pub use wire::{read_frame, write_frame};
