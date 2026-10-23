# System Interface Extensions

System Interface Extensions (SIE) is an MIT-licensed Rust desktop companion
for extending how input devices control a Linux computer. Version 0.3.0 is a
GNOME/Wayland proof of concept for the **Razer BlackWidow V4 Pro** (`1532:028d`).

- A top-panel tray opens a Czech menu with **O aplikaci** (the running binary's
  version) and **Ukončit**.
- **Vypnout osvětlení klávesnice** turns off the entire RGB matrix at night.
  The item then becomes **Zapnout osvětlení klávesnice**. A green check marks
  lighting on; a red cross marks lighting off. Workspace switching keeps working
  while the keyboard stays dark.
- Rust generates touch-typing finger zones and sends them directly to OpenRazer.
  The active workspace's **M1–M5 key is green** and other macro keys are white,
  including when switching with the mouse or another shortcut. The established
  red system key and green main Enter are retained.
- **M1–M5 → workspaces 1–5** (GNOME indices 0–4), using the live workspace count. Unavailable targets,
  releases, autorepeat and ordinary typing are ignored.
- On workspace 6 or higher, all five macro keys stay white. The tray uses
  workspace numbers starting at one; the diagnostic CLI explicitly uses indices.
- Existing GNOME integrations are reused; no additional Shell extension is needed
  on the development workstation. VirtualAssistant remains independent.

See [the proof of concept and architecture](docs/proof-of-concept.md) for scope,
the Rust decision, integration boundaries and the future voice roadmap.

## Prerequisites

Use Linux, Rust 1.89 or newer, a GNOME Wayland user session, OpenRazer with a
supported keyboard, and:

- A StatusNotifierItem host (the installed GNOME AppIndicator extension).
- The existing `focus-tracker@olbrasoft.cz` extension exposing
  `org.olbrasoft.Desktop.TotalWorkspaces` and `CurrentWorkspace`.
- The existing GNOME shortcuts `switch-to-workspace-1` through `-5` set to
  `<Super>KP_1` through `<Super>KP_5`. SIE checks these and does not modify them.
- Read/write access to the keyboard's input nodes, provided here by OpenRazer's
  `plugdev` rules. Run as your desktop user, not root.
- `gsettings` for shortcut inspection and `zenity` for the About dialog.

Tests additionally need `dbus-daemon` and `glib-compile-schemas` (Debian packages
`dbus-daemon` and `libglib2.0-bin`). There is no GTK development dependency.

## Build and run

```sh
cargo test --locked --all-targets
cargo run --locked -- --doctor
cargo run --locked
```

The default command runs the tray, maintains the lighting every two seconds,
and monitors the supported keyboard. While running, it maps just M1–M5 to native
Linux macro codes so GNOME does not also open Settings for M1. The original
device keymap returns on Quit or SIGTERM; ordinary keys and GNOME shortcuts are
unchanged. Avoid playing a competing Polychromatic
software effect while SIE owns the lighting. The old experimental profile is
neither loaded nor required. Quit stops monitoring and refreshing; the hardware
may retain its last frame until another controller changes it.

The lighting toggle keeps every RGB cell black, including ambient/wrist-rest
LEDs and macro highlights, during workspace changes and periodic refreshes.
Turning it back on restores the profile for the current workspace without
changing the saved device brightness. The choice applies to the current run;
restarting SIE starts with lighting enabled. In `--tray-only` mode the lighting
control is disabled.

```sh
cargo run --locked -- --tray-only            # Tray without keyboard control
cargo run --locked -- --apply-lighting       # Apply once and exit
cargo run --locked -- --switch-workspace 4   # Test M5's target, if it exists
cargo run --locked -- --version
```

Only one controlling instance may run at a time. Stop the tray before using
the one-shot control commands. `--doctor` is available while SIE is running.

## Install locally

```sh
./scripts/install.sh
systemctl --user start sie.service
journalctl --user -u sie.service -f
```

The installer builds the release binary and installs a desktop launcher and
user service. Installation does not enable automatic startup. If wanted:

```sh
systemctl --user enable sie.service
```

**Ukončit** exits successfully and the service does not restart it. To start it
again, use the desktop launcher or `systemctl --user start sie.service`.

## Development

Create issues before implementation and use GitHub's native sub-issue relations.
The [initial parent issue](https://github.com/Olbrasoft/sie/issues/1) tracks the
documentation, tray, lighting and macro-key tasks. CI runs:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

Integration tests launch a private D-Bus and mock the device and compositor.
They do not read typing, write real lighting, change desktop settings or switch
the logged-in user's workspaces. Physical M1–M5 presses and visual confirmation
of the keyboard colors are separate checks described in the design document.

## License

[MIT](LICENSE), copyright 2026 Jiří Tůma.
