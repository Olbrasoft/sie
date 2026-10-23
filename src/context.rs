//! Live state from the same GNOME focus tracker used by VirtualAssistant's monitor.
use anyhow::{Context, Result};
use zbus::Connection;

#[derive(Debug, Clone, Default)]
pub struct DesktopContext {
    pub application: String,
    pub window_title: String,
}

pub async fn snapshot(connection: &Connection) -> Result<DesktopContext> {
    let properties = zbus::fdo::PropertiesProxy::builder(connection)
        .destination("org.olbrasoft.Desktop")?
        .path("/org/olbrasoft/Desktop")?
        .build()
        .await?;
    let mut values = properties
        .get_all("org.olbrasoft.Desktop".try_into()?)
        .await?;
    Ok(DesktopContext {
        application: String::try_from(
            values
                .remove("ActiveApplication")
                .context("Missing focused application")?,
        )?,
        window_title: String::try_from(
            values
                .remove("ActiveWindow")
                .context("Missing focused window")?,
        )?,
    })
}
