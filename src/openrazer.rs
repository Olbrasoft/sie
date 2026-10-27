use anyhow::{Context, Result, ensure};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use zbus::{Connection, Proxy};

use crate::lighting::{ANIMATION_INTERVAL, COLS, LightingMode, ROWS, State, encode_frame};

pub const VENDOR_ID: u16 = 0x1532;
pub const PRODUCT_ID: u16 = 0x028D;

pub fn supported_device(ids: &[i32], dimensions: &[i32]) -> bool {
    ids == [i32::from(VENDOR_ID), i32::from(PRODUCT_ID)] && dimensions == [ROWS as i32, COLS as i32]
}

pub async fn devices(connection: &Connection) -> Result<Vec<String>> {
    let manager = Proxy::new(connection, "org.razer", "/org/razer", "razer.devices").await?;
    let serials: Vec<String> = manager.call("getDevices", &()).await?;
    let mut paths = Vec::new();
    for serial in serials {
        let path = format!("/org/razer/device/{serial}");
        let misc = Proxy::new(connection, "org.razer", path.as_str(), "razer.device.misc").await?;
        let ids: Vec<i32> = misc.call("getVidPid", &()).await?;
        // Ask for dimensions only on the known keyboard, not on other Razer devices.
        if ids != [i32::from(VENDOR_ID), i32::from(PRODUCT_ID)] {
            continue;
        }
        let dimensions: Vec<i32> = misc.call("getMatrixDimensions", &()).await?;
        ensure!(
            supported_device(&ids, &dimensions),
            "Unsupported keyboard matrix: {dimensions:?}"
        );
        paths.push(path);
    }
    Ok(paths)
}

pub async fn apply(connection: &Connection, active_workspace: Option<i32>) -> Result<usize> {
    apply_mode(connection, active_workspace, LightingMode::On).await
}

pub async fn apply_mode(
    connection: &Connection,
    active_workspace: Option<i32>,
    mode: LightingMode,
) -> Result<usize> {
    apply_status(
        connection,
        active_workspace,
        mode,
        crate::media::Playback::Unavailable,
    )
    .await
}

pub async fn apply_status(
    connection: &Connection,
    active_workspace: Option<i32>,
    mode: LightingMode,
    playback: crate::media::Playback,
) -> Result<usize> {
    apply_recording(connection, active_workspace, mode, playback, None).await
}

pub async fn apply_recording(
    connection: &Connection,
    active_workspace: Option<i32>,
    mode: LightingMode,
    playback: crate::media::Playback,
    elapsed: Option<Duration>,
) -> Result<usize> {
    apply_state(
        connection,
        State {
            workspace: active_workspace,
            mode,
            playback,
            recording: elapsed.is_some(),
            ..Default::default()
        },
        elapsed,
    )
    .await
}

async fn apply_state(
    connection: &Connection,
    state: State,
    elapsed: Option<Duration>,
) -> Result<usize> {
    let paths = devices(connection)
        .await
        .context("OpenRazer discovery failed")?;
    // A complete black custom frame also covers macro and ambient LEDs without
    // changing the user's saved brightness or disabling keyboard input.
    let payload = encode_frame(&state.mode.frame_with_dial(
        state.workspace,
        state.playback,
        elapsed,
        state.dial,
    ));
    for path in &paths {
        let lighting = Proxy::new(
            connection,
            "org.razer",
            path.as_str(),
            "razer.device.lighting.chroma",
        )
        .await?;
        lighting.call::<_, _, ()>("setKeyRow", &(&payload,)).await?;
        lighting.call::<_, _, ()>("setCustom", &()).await?;
    }
    Ok(paths.len())
}

/// One asynchronous writer owns every lighting frame, keeping USB latency out
/// of the input/dictation loop. Watch updates and missed ticks never queue frames.
pub async fn run(
    connection: Connection,
    mut requests: watch::Receiver<State>,
    status: watch::Sender<String>,
) {
    let mut refresh = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(2),
        Duration::from_secs(2),
    );
    let mut animation = tokio::time::interval_at(
        tokio::time::Instant::now() + ANIMATION_INTERVAL,
        ANIMATION_INTERVAL,
    );
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut started: Option<Instant> = None;
    let mut last_error = String::new();
    loop {
        let state = *requests.borrow_and_update();
        let elapsed = if state.recording {
            Some(started.get_or_insert_with(Instant::now).elapsed())
        } else {
            started = None;
            None
        };
        let label = match apply_state(&connection, state, elapsed).await {
            Ok(count) => {
                last_error.clear();
                if count == 0 {
                    "Klávesnice není připojena"
                } else if state.mode == LightingMode::Off {
                    "Osvětlení vypnuté"
                } else if state.recording {
                    "Podsvícení aktivní · nahrávání"
                } else {
                    "Podsvícení aktivní"
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                if message != last_error {
                    tracing::warn!(%message, "Lighting unavailable");
                    last_error = message;
                }
                "Podsvícení nedostupné"
            }
        };
        status.send_if_modified(|old| {
            if old == label {
                return false;
            }
            *old = label.into();
            true
        });
        tokio::select! {
            changed = requests.changed() => if changed.is_err() { break; },
            _ = refresh.tick(), if !state.recording || state.mode == LightingMode::Off => {},
            _ = animation.tick(), if state.recording && state.mode == LightingMode::On => {},
        }
    }
    // Normal Quit/SIGTERM must not leave a frozen recording rainbow behind.
    // Use the latest mode even if it changed while a USB write was in flight.
    let state = *requests.borrow();
    if let Err(error) = apply_recording(
        &connection,
        state.workspace,
        state.mode,
        state.playback,
        None,
    )
    .await
    {
        tracing::warn!(%error, "Could not restore idle lighting on shutdown");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_other_models_and_unexpected_matrix_dimensions() {
        assert!(supported_device(&[0x1532, 0x028D], &[8, 23]));
        assert!(!supported_device(&[0x1532, 0x0293], &[8, 23]));
        assert!(!supported_device(&[0x1532, 0x028D], &[23, 8]));
        assert!(!supported_device(&[0x1532, 0x028D], &[6, 22]));
        assert!(!supported_device(&[], &[]));
    }
}
