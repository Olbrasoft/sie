# Proof of concept validation

Validated on 2026-10-23 with GNOME Shell 48.7 on Wayland, OpenRazer 3.12.4,
the connected Razer BlackWidow V4 Pro (`1532:028d`), and Rust 1.99.0.

## Automated checks

- Formatting and Clippy with warnings denied passed.
- All 14 tests passed: eight unit tests and six private D-Bus integration tests.
- The release build, shell installer syntax, systemd unit and desktop launcher
  validation passed.
- The private bus tests confirm that M5 emits no input when only four workspaces
  exist, all five available targets emit the expected keycodes, a failed key
  press still releases both keys, and a duplicate process cannot replace SIE.

## Live desktop checks

- `sie --doctor` found the supported matrix, two readable keyboard interfaces,
  the live workspace tracker, all five configured shortcuts, the tray host,
  Mutter input API and Zenity.
- All five one-shot targets activated real GNOME workspace indices 0–4; each
  result was confirmed by reading the tracker. The original workspace was
  restored after the probe.
- The native 576-byte lighting frame was accepted by OpenRazer. The running
  application reports active lighting and monitors both Razer keyboard nodes.
- Clicking the tray opened the menu. Clicking About displayed the application
  name, version 0.1.0 and MIT license; the dialog contents were also read through
  desktop accessibility.
- Activating the exposed menu's Quit action stopped the service with success
  and without an automatic restart. Starting the service again worked.
- A second controlling process failed while the first retained its process ID
  and D-Bus ownership.
- Runtime logs recorded actual Razer macro input activating workspace indices
  1 and 0 while the service was running.

## Physical checks still required

Visually confirm the LEDs on the keyboard, exercise every physical M1–M5 key,
and unplug/reconnect the device. The four-workspace boundary was tested on a
private D-Bus; the user's occupied real workspaces were not removed to reproduce
that configuration. Automatic login startup was left disabled.

## Integration finding

Mutter's `NotifyKeyboardKeysym` did not reliably activate the fifth target on
this desktop. `NotifyKeyboardKeycode` with Linux evdev codes worked for every
target. The implementation uses keycodes and confirms actual activation before
stopping each short input session. No new GNOME extension was installed.
