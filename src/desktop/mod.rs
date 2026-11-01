//! Desktop adapters with a stable facade for focus, workspaces and delivery.
mod delivery;
mod focus;
mod input;
mod workspaces;

pub use delivery::{
    DeliveryMode, PasteReceipt, paste_text, paste_text_with_mode, paste_text_with_receipt,
    press_enter, send_enter_to,
};
pub use focus::{FocusTarget, focused_window};
pub use input::Desktop;
pub use workspaces::{
    WorkspaceState, binding_matches, check_binding, current_workspace, watch_workspaces,
    workspace_count, workspace_state,
};
