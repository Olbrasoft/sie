//! Reveal the controlled player after a physical media-button press.
use crate::{browser::Broker, media};
use anyhow::{Result, ensure};
use serde::Deserialize;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};
use zbus::{Connection, Proxy, zvariant::OwnedValue};

#[derive(Clone, Debug)]
pub struct NativeHint {
    pub at: Instant,
    pub destination: String,
}
#[derive(Clone)]
pub struct Request {
    pub at: Instant,
    pub fallback: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Window {
    pub id: u32,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub wm_class: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub focus: bool,
}
struct Target {
    pid: u32,
    identity: String,
    desktop: String,
    title: String,
    url: String,
    can_raise: bool,
}
fn value_string(values: &std::collections::HashMap<String, OwnedValue>, key: &str) -> String {
    values
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or_default()
        .into()
}
async fn target(connection: &Connection, name: &str) -> Result<Target> {
    let daemon = zbus::fdo::DBusProxy::new(connection).await?;
    let pid = daemon
        .get_connection_unix_process_id(name.try_into()?)
        .await?;
    let properties = zbus::fdo::PropertiesProxy::builder(connection)
        .destination(name)?
        .path(media::PLAYER_PATH)?
        .build()
        .await?;
    let root = properties
        .get_all("org.mpris.MediaPlayer2".try_into()?)
        .await?;
    let mut player = properties
        .get_all(media::PLAYER_INTERFACE.try_into()?)
        .await?;
    let metadata = player
        .remove("Metadata")
        .and_then(|v| std::collections::HashMap::<String, OwnedValue>::try_from(v).ok())
        .unwrap_or_default();
    Ok(Target {
        pid,
        identity: value_string(&root, "Identity"),
        desktop: value_string(&root, "DesktopEntry"),
        title: value_string(&metadata, "xesam:title"),
        url: value_string(&metadata, "xesam:url"),
        can_raise: root
            .get("CanRaise")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false),
    })
}
async fn windows(connection: &Connection) -> Result<Vec<Window>> {
    let proxy = Proxy::new(
        connection,
        "org.gnome.Shell",
        "/org/gnome/Shell/Extensions/Windows",
        "org.gnome.Shell.Extensions.Windows",
    )
    .await?;
    Ok(serde_json::from_str(
        &proxy.call::<_, _, String>("List", &()).await?,
    )?)
}
fn title_matches(actual: &str, title: &str) -> bool {
    let actual = actual.trim();
    let title = title.trim();
    !title.is_empty()
        && (actual == title
            || [" - ", " | ", " — "]
                .iter()
                .any(|separator| actual.starts_with(&format!("{title}{separator}"))))
}
pub fn choose_window(
    windows: &[Window],
    pid: u32,
    desktop: &str,
    title: Option<&str>,
) -> Result<u32> {
    let mut candidates: Vec<_> = windows.iter().filter(|w| pid > 0 && w.pid == pid).collect();
    if candidates.is_empty() && !desktop.is_empty() {
        candidates = windows
            .iter()
            .filter(|w| {
                w.wm_class
                    .eq_ignore_ascii_case(desktop.trim_end_matches(".desktop"))
            })
            .collect();
    }
    if let Some(title) = title {
        candidates.retain(|w| title_matches(&w.title, title));
    }
    ensure!(
        candidates.len() == 1,
        "Player window is missing or ambiguous"
    );
    Ok(candidates[0].id)
}
async fn activate(connection: &Connection, id: u32) -> Result<()> {
    let proxy = Proxy::new(
        connection,
        "org.gnome.Shell",
        "/org/gnome/Shell/Extensions/Windows",
        "org.gnome.Shell.Extensions.Windows",
    )
    .await?;
    proxy.call::<_, _, ()>("Activate", &(id,)).await?;
    for _ in 0..20 {
        if windows(connection)
            .await?
            .iter()
            .any(|w| w.id == id && w.focus)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!("Player window did not become active")
}
fn browser(identity: &str, windows: &[Window], pid: u32) -> bool {
    let name = identity.to_lowercase();
    ["chrome", "chromium", "edge", "brave", "vivaldi"]
        .iter()
        .any(|s| name.contains(s))
        || windows.iter().filter(|w| w.pid == pid).any(|w| {
            ["chrome", "chromium", "edge", "brave", "vivaldi"]
                .iter()
                .any(|s| w.wm_class.to_lowercase().contains(s))
        })
}
pub async fn reveal(connection: &Connection, broker: &Broker, name: &str) -> Result<()> {
    let target = target(connection, name).await?;
    let initial = windows(connection).await?;
    if browser(&target.identity, &initial, target.pid) {
        // No bridge round trip is needed when the media tab is already visible.
        if let Ok(id) = choose_window(&initial, target.pid, &target.desktop, Some(&target.title)) {
            return activate(connection, id).await;
        }
        let tab = broker
            .activate(target.pid, &target.title, &target.url)
            .await?;
        for _ in 0..20 {
            if let Ok(id) = choose_window(
                &windows(connection).await?,
                target.pid,
                &target.desktop,
                Some(&tab.title),
            ) {
                return activate(connection, id).await;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        anyhow::bail!("Browser did not reveal the requested tab")
    }
    if target.can_raise {
        let root = Proxy::new(
            connection,
            name,
            media::PLAYER_PATH,
            "org.mpris.MediaPlayer2",
        )
        .await?;
        let _ = root.call::<_, _, ()>("Raise", &()).await;
    }
    let current = windows(connection).await?;
    let id = choose_window(&current, target.pid, &target.desktop, Some(&target.title))
        .or_else(|_| choose_window(&current, target.pid, &target.desktop, None))?;
    activate(connection, id).await
}

async fn matches_destination(connection: &Connection, state: &media::Status, name: &str) -> bool {
    let Some(target) = &state.target else {
        return false;
    };
    if target == name {
        return true;
    }
    // The native observer may report the unique owner rather than its MPRIS name.
    let Ok(daemon) = zbus::fdo::DBusProxy::new(connection).await else {
        return false;
    };
    let Ok(target) = target.as_str().try_into() else {
        return false;
    };
    daemon
        .get_name_owner(target)
        .await
        .is_ok_and(|owner| owner.as_str() == name)
}

async fn playback_started(
    connection: &Connection,
    states: &mut watch::Receiver<media::Status>,
    name: &str,
    pressed: Instant,
) -> bool {
    // The input action retains the kernel's physical press time, so a signal
    // processed before the queued action still follows the actual press.
    let earliest = pressed;
    let initial = states.borrow_and_update().clone();
    if !matches_destination(connection, &initial, name).await {
        return false;
    }
    let before = match initial.transition {
        Some(transition) if transition.at >= earliest => transition.before,
        _ => initial.playback,
    };
    if before != media::Playback::Paused {
        // Pausing a playing target must never request focus, even if its
        // PlaybackStatus update is delayed or a later Space press resumes it.
        return false;
    }
    tokio::time::timeout(Duration::from_millis(1200), async {
        loop {
            let state = states.borrow_and_update().clone();
            if matches_destination(connection, &state, name).await
                && state.playback == media::Playback::Playing
                && state.transition.is_some_and(|transition| {
                    transition.at >= earliest && transition.before == media::Playback::Paused
                })
            {
                return true;
            }
            if states.changed().await.is_err() {
                return false;
            }
        }
    })
    .await
    .unwrap_or(false)
}

pub async fn run(
    connection: Connection,
    broker: Arc<Broker>,
    mut requests: mpsc::Receiver<Request>,
    mut hints: watch::Receiver<Option<NativeHint>>,
    mut states: watch::Receiver<media::Status>,
    status: watch::Sender<Option<String>>,
) {
    while let Some(mut request) = requests.recv().await {
        while let Ok(new) = requests.try_recv() {
            request = new;
        }
        if request.at.elapsed() > Duration::from_secs(2) {
            continue;
        }
        let mut hint = hints.borrow_and_update().clone();
        let recent = |hint: &Option<NativeHint>| {
            hint.as_ref().is_some_and(|h| {
                h.at >= request
                    .at
                    .checked_sub(Duration::from_millis(80))
                    .unwrap_or(request.at)
                    && h.at.elapsed() < Duration::from_secs(1)
            })
        };
        if !recent(&hint)
            && tokio::time::timeout(Duration::from_millis(180), hints.changed())
                .await
                .is_ok()
        {
            hint = hints.borrow_and_update().clone();
        }
        let destination = if recent(&hint) {
            hint.unwrap().destination
        } else {
            request.fallback
        };
        if !playback_started(&connection, &mut states, &destination, request.at).await {
            status.send_replace(None);
            continue;
        }
        match tokio::time::timeout(
            Duration::from_secs(6),
            reveal(&connection, &broker, &destination),
        )
        .await
        {
            Ok(Ok(())) => {
                status.send_replace(None);
            }
            result => {
                if let Ok(Err(error)) = &result {
                    tracing::warn!(%error,"Could not reveal media target");
                }
                let message = if matches!(&result,Ok(Err(e)) if e.to_string().contains("extension is not connected"))
                {
                    "Přechod k videu: zapni rozšíření SIE v prohlížeči"
                } else {
                    "Přechod k videu se nezdařil"
                };
                status.send_replace(Some(message.into()));
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn window(id: u32, pid: u32, title: &str) -> Window {
        Window {
            id,
            pid,
            title: title.into(),
            wm_class: "google-chrome".into(),
            focus: false,
        }
    }
    #[test]
    fn activation_selects_only_the_matching_process_and_existing_media_title() {
        let windows = [
            window(1, 10, "Other - Google Chrome"),
            window(2, 10, "Video - YouTube - Google Chrome"),
            window(3, 99, "Video - YouTube - Google Chrome"),
        ];
        assert_eq!(
            choose_window(&windows, 10, "", Some("Video - YouTube")).unwrap(),
            2
        );
        assert!(choose_window(&windows, 10, "", Some("Missing")).is_err());
        assert!(choose_window(&windows, 10, "", None).is_err());
        assert!(choose_window(&windows, 0, "", None).is_err());
        let mut duplicate = windows[1].clone();
        duplicate.id = 4;
        duplicate.focus = true;
        assert!(choose_window(&[windows[1].clone(), duplicate], 10, "", Some("Video")).is_err());
    }
}
