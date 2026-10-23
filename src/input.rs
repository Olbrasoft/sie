use std::{collections::HashSet, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use evdev::{Device, KeyCode};
use tokio::{sync::mpsc, task::JoinSet};

use crate::{
    keymap::MacroKeymap,
    macros::{MACRO1, workspace_for_event},
    openrazer::{PRODUCT_ID, VENDOR_ID},
};

/// Inspect only stable by-id paths belonging to the supported keyboard.
pub fn discover() -> Result<Vec<(PathBuf, Device)>> {
    let mut seen = HashSet::new();
    let mut devices = Vec::new();
    for entry in std::fs::read_dir("/dev/input/by-id").context("Cannot read /dev/input/by-id")? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("usb-Razer_Razer_BlackWidow_V4_Pro-") || !name.contains("event") {
            continue;
        }
        let path = std::fs::canonicalize(entry.path())?;
        if !seen.insert(path.clone()) {
            continue;
        }
        let device = Device::open(&path).with_context(|| {
            format!(
                "Cannot read {}; check keyboard input permissions",
                path.display()
            )
        })?;
        let id = device.input_id();
        if id.vendor() != VENDOR_ID || id.product() != PRODUCT_ID {
            continue;
        }
        if device.supported_keys().is_some_and(|keys| {
            keys.contains(KeyCode::KEY_F13) || keys.contains(KeyCode::new(MACRO1))
        }) {
            devices.push((path, device));
        }
    }
    Ok(devices)
}

/// No exclusive grab, replay of ordinary typing, or storage of key events.
pub async fn monitor(tx: mpsc::Sender<u32>) {
    let mut tasks = JoinSet::new();
    let mut active = HashSet::new();
    let mut scan = tokio::time::interval(Duration::from_secs(3));
    let mut previous_error = String::new();
    loop {
        tokio::select! {
            _ = scan.tick() => {
                match discover() {
                    Ok(devices) => {
                        previous_error.clear();
                        for (path, device) in devices {
                            if active.contains(&path) { continue; }
                            let keymap = match Device::open(&path).and_then(MacroKeymap::install) {
                                Ok(keymap) => keymap,
                                Err(error) => {
                                    tracing::warn!(%error, device = %path.display(), "Cannot isolate macro keys; input control disabled for this interface");
                                    continue;
                                }
                            };
                            match device.into_event_stream() {
                                Ok(mut stream) => {
                                    active.insert(path.clone());
                                    tracing::info!(device = %path.display(), "Listening for M1–M5");
                                    let tx = tx.clone();
                                    tasks.spawn(async move {
                                        // Keep the guard until disconnect/cancellation so Quit restores the keymap.
                                        let _keymap = keymap;
                                        while let Ok(event) = stream.next_event().await {
                                            if let Some(index) = workspace_for_event(event) {
                                                // Bound pending actions; stale macro presses should not accumulate.
                                                let _ = tx.try_send(index);
                                            }
                                        }
                                        path
                                    });
                                }
                                Err(error) => tracing::warn!(%error, device = %path.display(), "Cannot monitor input"),
                            }
                        }
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        if message != previous_error { tracing::warn!("{message}"); previous_error = message; }
                    }
                }
            }
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Ok(path) = result {
                    tracing::info!(device = %path.display(), "Keyboard input disconnected");
                    active.remove(&path);
                }
            }
        }
    }
}
