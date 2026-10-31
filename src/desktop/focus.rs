//! Resolve the current compositor focus and enrich it with tracked context.
use anyhow::{Context, Result};
use serde::Deserialize;
use zbus::{Connection, Proxy};

#[derive(Debug, Clone, Deserialize)]
pub struct FocusTarget {
    pub id: u32,
    pub wm_class: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    focus: bool,
}

impl FocusTarget {
    pub fn is_terminal(&self) -> bool {
        let class = self.wm_class.to_lowercase();
        [
            "alacritty",
            "kitty",
            "terminator",
            "gnome-terminal",
            "konsole",
            "wezterm",
            "kgx",
            "org.gnome.console",
            "org.gnome.terminal",
            "xfce4-terminal",
            "mate-terminal",
            "tilix",
            "foot",
            "xterm",
            "urxvt",
            "terminology",
            "opencode",
        ]
        .iter()
        .any(|name| class.contains(name))
    }
}

pub async fn focused_window(connection: &Connection) -> Result<FocusTarget> {
    let proxy = Proxy::new(
        connection,
        "org.gnome.Shell",
        "/org/gnome/Shell/Extensions/Windows",
        "org.gnome.Shell.Extensions.Windows",
    )
    .await?;
    let json: String = proxy.call("List", &()).await?;
    let windows: Vec<FocusTarget> = serde_json::from_str(&json)?;
    let mut target = windows
        .into_iter()
        .find(|window| window.focus)
        .context("No focused target window")?;
    // The Shell extension continuously tracks focus; read its current snapshot
    // rather than inferring the input target from the mouse position.
    if let Ok(context) = crate::context::snapshot(connection).await
        && context
            .application
            .trim_end_matches(".desktop")
            .eq_ignore_ascii_case(&target.wm_class)
    {
        target.title = context.window_title;
    }
    Ok(target)
}
