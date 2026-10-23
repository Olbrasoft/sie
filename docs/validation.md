# Proof of concept validation

Validated on 2026-10-23 with GNOME Shell 48.7 on Wayland, OpenRazer 3.12.4,
the connected Razer BlackWidow V4 Pro (`1532:028d`), and Rust 1.99.0.

## Automated checks

The initial version 0.1.0 checks were:

- Formatting and Clippy with warnings denied passed.
- All 17 tests passed: eleven unit tests and six private D-Bus integration tests.
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
- The reported M1 conflict was traced to XKB's F13 → XF86Tools mapping. After
  the fix, both real keyboard interfaces expose M1–M5 as keycodes 656–660 while
  SIE runs. Stopping the service restored all ten original mappings to 183–187;
  restarting reapplied the isolation. GNOME shortcut settings were untouched.

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

## Active workspace lighting in version 0.2.0

- Formatting, Clippy and all 23 tests passed locally (15 unit tests and eight
  private D-Bus integration tests).
- The LED map was verified against the installed BlackWidow device graphic:
  M1–M5 use column 1 and rows 5 through 1 respectively.
- Tests confirm that externally emitted workspace changes move the green LED
  from M3 to M4, return the old key to white, and never send keyboard input.
  Losing tracker state clears the old highlight. All touch-typing cells retain
  their previous colors.
- Version 0.2.0 was installed and the user service restarted successfully. Live
  logs show workspace changes updating the one-based tray label and selected
  macro key.
- The user confirmed that the correct physical M key glows green and follows
  workspace changes made with the mouse or another shortcut. The earlier M1
  Settings regression was also physically confirmed fixed by the user.

## Lighting toggle in version 0.3.0

- Formatting, Clippy and all 27 tests passed locally (18 unit tests and nine
  private D-Bus integration tests). The release binary was installed and the
  user service restarted successfully.
- New tests verify that every RGB cell stays black with lighting off, including
  ambient rows and macro indicators. A complete D-Bus off/switch/refresh/on
  sequence confirms workspace switching still works and re-enabling restores
  the profile for the newly active workspace.
- The actual tray item was clicked to disable lighting. Its green check and
  off action changed to a red cross and on action. The user confirmed the
  physical keyboard, macro keys and ambient lighting were fully dark.
- An external shortcut switched the real desktop to workspace 4. After a
  periodic refresh, the tray still reported lighting off and showed the new
  workspace. The original workspace was restored.
- Clicking the on action succeeded; the menu returned to its green check/off
  action and the application reported active lighting again.
- The toggle is intentionally scoped to the current application run; no
  brightness setting or persistent preference was changed.
