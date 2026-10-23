//! Linux desktop adapters and independently testable keyboard behavior.

pub mod audio;
pub mod browser;
pub mod config;
pub mod context;
pub mod corrections;
pub mod desktop;
pub mod dictation;
pub mod input;
mod keymap;
pub mod lighting;
pub mod macros;
pub mod media;
pub mod media_focus;
pub mod openrazer;
mod sounds;
pub mod speech;
mod speech_lock;
pub mod terminal;
pub mod tray;

pub const APP_NAME: &str = "System Interface Extensions";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUS_NAME: &str = "cz.olbrasoft.SystemInterfaceExtensions";

pub async fn claim_instance(connection: &zbus::Connection) -> anyhow::Result<()> {
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    let reply = connection
        .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
        .await?;
    anyhow::ensure!(
        matches!(
            reply,
            RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner
        ),
        "SIE is already running"
    );
    Ok(())
}
