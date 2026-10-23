//! Manual Unicode paste check; only runs in the dedicated GTK fixture window.
use std::sync::{Arc, atomic::AtomicBool};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let connection = zbus::Connection::session().await?;
    let target = sie::desktop::focused_window(&connection).await?;
    anyhow::ensure!(
        ["cz.olbrasoft.SieDictationProbe", "AlacrittySIEProbe"].contains(&target.wm_class.as_str()),
        "Focus a dedicated SIE dictation/PRIMARY probe first"
    );
    let mode = if std::env::args().any(|arg| arg == "--submit") {
        sie::desktop::DeliveryMode::Submit
    } else {
        sie::desktop::DeliveryMode::PasteOnly
    };
    sie::desktop::paste_text_with_mode(
        &connection,
        &target,
        "Příliš žluťoučký kůň úpěl ďábelské ódy.",
        mode,
        Arc::new(AtomicBool::new(false)),
    )
    .await?;
    println!("Unicode paste sent to the dedicated probe.");
    Ok(())
}
