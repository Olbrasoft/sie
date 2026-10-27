//! Own only the keyboard's mouse-only dial channel, never a keyboard node.
use crate::{
    macros::InputAction,
    openrazer::{PRODUCT_ID, VENDOR_ID},
};
use anyhow::{Context, Result, ensure};
use evdev::{
    AbsoluteAxisCode, AttributeSet, Device, EventType, InputEvent, RelativeAxisCode,
    UinputAbsSetup, uinput::VirtualDevice,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

pub fn supported_interface(
    vendor: u16,
    product: u16,
    path: &str,
    keys: &[u16],
    relative: &[u16],
) -> bool {
    vendor == VENDOR_ID
        && product == PRODUCT_ID
        && path.ends_with("-if02-event-mouse")
        && relative.contains(&RelativeAxisCode::REL_HWHEEL.0)
        && keys
            .iter()
            .all(|key| matches!(*key,114|115|272..=276|682..=685))
}

/// High-resolution duplicates are consumed, not counted as extra detents.
pub fn rotation(event: InputEvent) -> Option<i32> {
    (event.event_type() == EventType::RELATIVE
        && event.code() == RelativeAxisCode::REL_HWHEEL.0
        && event.value() != 0)
        .then(|| event.value().clamp(-16, 16))
}

pub fn is_dial_axis(event: InputEvent) -> bool {
    event.event_type() == EventType::RELATIVE
        && [
            RelativeAxisCode::REL_HWHEEL.0,
            RelativeAxisCode::REL_HWHEEL_HI_RES.0,
        ]
        .contains(&event.code())
}

fn discover() -> Result<(PathBuf, Device)> {
    for entry in std::fs::read_dir("/dev/input/by-id")? {
        let path = entry?.path();
        let name = path.file_name().unwrap().to_string_lossy();
        if !name.starts_with("usb-Razer_Razer_BlackWidow_V4_Pro-")
            || !name.ends_with("-if02-event-mouse")
        {
            continue;
        }
        let device = Device::open(&path)?;
        let id = device.input_id();
        let keys: Vec<_> = device
            .supported_keys()
            .map(|v| v.iter().map(|k| k.0).collect())
            .unwrap_or_default();
        let relative: Vec<_> = device
            .supported_relative_axes()
            .map(|v| v.iter().map(|k| k.0).collect())
            .unwrap_or_default();
        ensure!(
            supported_interface(id.vendor(), id.product(), &name, &keys, &relative),
            "Unsupported Command Dial input interface"
        );
        return Ok((path, device));
    }
    anyhow::bail!("Command Dial interface is unavailable")
}

fn passthrough(device: &Device) -> Result<VirtualDevice> {
    let name = "SIE Command Dial passthrough";
    let keys = device
        .supported_keys()
        .context("Missing dial control capabilities")?;
    let axes: AttributeSet<RelativeAxisCode> = device
        .supported_relative_axes()
        .context("Missing dial axes")?
        .iter()
        .filter(|axis| {
            ![
                RelativeAxisCode::REL_HWHEEL,
                RelativeAxisCode::REL_HWHEEL_HI_RES,
            ]
            .contains(axis)
        })
        .collect();
    let mut builder = VirtualDevice::builder()?
        .name(&name)
        .with_keys(keys)?
        .with_relative_axes(&axes)?;
    for (axis, info) in device.get_absinfo()? {
        ensure!(
            axis == AbsoluteAxisCode::ABS_VOLUME,
            "Unexpected dial absolute axis"
        );
        builder = builder.with_absolute_axis(&UinputAbsSetup::new(axis, info))?;
    }
    Ok(builder.build()?)
}

async fn listen(tx: &mpsc::Sender<(InputAction, Instant)>) -> Result<()> {
    let (_path, mut device) = discover()?;
    // Create the replacement before ownership changes; failure cannot steal input.
    let mut output =
        passthrough(&device).context("Command Dial requires read/write access to /dev/uinput")?;
    device
        .grab()
        .context("Cannot own the mouse-only Command Dial interface")?;
    let mut stream = device.into_event_stream()?;
    tracing::info!("Command Dial connected; keyboard nodes remain ungrabbed");
    let mut frame = Vec::new();
    loop {
        let event = stream.next_event().await?;
        if event.event_type() == EventType::SYNCHRONIZATION {
            if event.code() == 0 && !frame.is_empty() {
                output.emit(&frame)?;
                frame.clear();
            } else if event.code() != 0 {
                frame.clear();
            }
            continue;
        }
        if let Some(delta) = rotation(event) {
            let _ = tx.try_send((InputAction::DialTurn(delta), Instant::now()));
        }
        if !is_dial_axis(event)
            && matches!(
                event.event_type(),
                EventType::KEY | EventType::RELATIVE | EventType::ABSOLUTE
            )
        {
            ensure!(frame.len() < 64, "Oversized dial passthrough frame");
            frame.push(event);
        }
    }
}

pub async fn monitor(tx: mpsc::Sender<(InputAction, Instant)>) {
    let mut previous = String::new();
    while !tx.is_closed() {
        if let Err(error) = listen(&tx).await {
            let message = error.to_string();
            if message != previous {
                tracing::warn!(%message,"Command Dial input unavailable");
                previous = message;
            }
        }
        let _ = tx.try_send((InputAction::DialDisconnected, Instant::now()));
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_supported_mouse_only_interface_can_be_owned() {
        let path = "usb-Razer_Razer_BlackWidow_V4_Pro-if02-event-mouse";
        let keys = [114, 115, 272, 273, 274, 275, 276, 682, 683, 684, 685];
        let axes = [0, 1, 6, 12];
        assert!(supported_interface(
            VENDOR_ID, PRODUCT_ID, path, &keys, &axes
        ));
        for (vendor, product, name) in [
            (0x046d, PRODUCT_ID, path),
            (VENDOR_ID, 0x028e, path),
            (
                VENDOR_ID,
                PRODUCT_ID,
                "usb-Razer_Razer_BlackWidow_V4_Pro-if01-event-kbd",
            ),
        ] {
            assert!(!supported_interface(vendor, product, name, &keys, &axes));
        }
        let mut typing_capable = keys.to_vec();
        typing_capable.push(30); // KEY_A must prevent exclusive ownership.
        assert!(!supported_interface(
            VENDOR_ID,
            PRODUCT_ID,
            path,
            &typing_capable,
            &axes
        ));
        assert!(!supported_interface(
            VENDOR_ID,
            PRODUCT_ID,
            path,
            &keys,
            &[0, 1]
        ));
    }

    #[test]
    fn each_low_resolution_detent_counts_once_in_both_directions() {
        for delta in [-1, 1] {
            let detent = InputEvent::new(EventType::RELATIVE.0, 6, delta);
            let duplicate = InputEvent::new(EventType::RELATIVE.0, 12, delta * 120);
            assert_eq!(rotation(detent), Some(delta));
            assert_eq!(rotation(duplicate), None);
            assert!(is_dial_axis(detent));
            assert!(is_dial_axis(duplicate));
        }
        assert_eq!(rotation(InputEvent::new(2, 6, 0)), None);
        assert_eq!(rotation(InputEvent::new(2, 6, i32::MAX)), Some(16));
        assert_eq!(rotation(InputEvent::new(2, 6, i32::MIN)), Some(-16));
    }

    #[test]
    fn other_mouse_controls_and_volume_are_not_consumed_as_dial_events() {
        for event in [
            InputEvent::new(EventType::KEY.0, 115, 1),
            InputEvent::new(EventType::KEY.0, 114, 0),
            InputEvent::new(EventType::ABSOLUTE.0, 32, 1),
            InputEvent::new(EventType::RELATIVE.0, 0, 4),
            InputEvent::new(EventType::KEY.0, 272, 1),
        ] {
            assert_eq!(rotation(event), None);
            assert!(!is_dial_axis(event));
        }
    }
}
