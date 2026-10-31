# System Interface Extensions

System Interface Extensions (SIE) is an MIT-licensed Rust desktop companion
for extending how input devices control a Linux computer. Version 0.17.1 is a
GNOME/Wayland proof of concept for the **Razer BlackWidow V4 Pro** (`1532:028d`).

- A top-panel tray opens a Czech menu with **O aplikaci** (the running binary's
  version) and **Ukončit**.
- **Vypnout osvětlení klávesnice a myši** turns off the entire keyboard RGB matrix
  and the connected Logitech G203 LIGHTSYNC (`046d:c092`) at night.
  The item then becomes **Zapnout osvětlení klávesnice a myši**. A green check marks
  lighting on; a red cross marks lighting off. Workspace switching keeps working
  while both devices stay dark. Enabling restores the keyboard profile and the
  mouse's saved lighting effect. See [mouse support and local access](docs/mouse-lighting.md).
- The **Play/Pause media key** is green while the globally controlled player is
  playing, red while paused/stopped and white without a controllable target.
  It follows Space, mouse and external playback changes across applications and
  workspaces. Lighting off overrides this indicator too.
- Starting/resuming playback with the physical **Play/Pause button** reveals
  the controlled player's existing window and workspace. Pausing keeps the
  current workspace and focus. A small local browser bridge selects
  the existing video tab in Chrome when another tab is active.
- During microphone recording, a moving rainbow lights only the ambient strip
  around the keyboard and wrist rest. It returns to white when recording stops;
  lighting off keeps everything black. See [recording indication and the separate
  voice-intensity assessment](docs/recording-lighting.md).
- Rust generates touch-typing finger zones and sends them directly to OpenRazer.
  The active workspace's **M1–M5 key is green** and other macro keys are white,
  including when switching with the mouse or another shortcut. The established
  red system key and green main Enter are retained; Escape is pure red.
- **M1–M5 → workspaces 1–5** (GNOME indices 0–4), using the live workspace count. Unavailable targets,
  releases, autorepeat and ordinary typing are ignored.
- On workspace 6 or higher, all five macro keys stay white. The tray uses
  workspace numbers starting at one; the diagnostic CLI explicitly uses indices.
- The **Command Dial beside Escape** opens GNOME's native Alt+Tab application
  strip for the current workspace when rotated (green). Rotation selects an icon
  without changing applications; press to confirm. Press while idle to open the
  same strip across workspaces (orange), then rotate and press again to activate.
  Escape or eight seconds of inactivity cancels; idle is white. See
  [Command Dial setup and behavior](docs/command-dial.md).
- Local Czech dictation uses Whisper large-v3-turbo, with CUDA or CPU inference.
  The **bottom side button** starts recording, then stops and transcribes.
  **Pause** or **Escape** cancels recording or transcription without delivery;
  the **middle side button** is deliberately inactive;
  the **top side button** finishes an active recording, transcribes, pastes and
  sends one Enter. During transcription it queues submission after successful
  paste; outside recording/transcription it acts as a literal Enter.
  The tray offers normal start/stop and cancellation.
  Original VirtualAssistant sound cues mark recording start, transcription
  (looping typing) and cancellation (paper rip).
  Optional volume ducking quiets one explicitly selected media output during
  recording and spoken notifications. Overlapping activity shares one saved
  volume and restores it when the last activity ends.
- An independent PostgreSQL database holds a one-time copy of VirtualAssistant's
  text corrections. No audio or transcript is sent to an LLM or remote API.
- Spoken agent notifications run inside SIE, with the existing source voices,
  multiple Google keys, durable serial playback, provider fallback to local Piper
  and explicit monitor output. Dictation pauses speech directly. Successfully
  played notification bodies are deleted; old VirtualAssistant history is not
  imported. Credentials stay in the desktop keyring. See
  [standalone spoken notifications and migration](docs/spoken-notifications.md).
- Pending Codex questions from another workspace appear as clickable native
  notifications. Clicking focuses their existing terminal and tmux pane;
  answered questions are removed. Spoken questions use the SIE voice queue. See [agent question notifications](docs/agent-notifications.md).
- Existing GNOME integrations are reused. A small Shell adapter controls the native
  Alt+Tab strip without holding virtual modifiers. Legacy VirtualAssistant
  shortcuts remain independent.

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
- The installed Window Calls GNOME extension for revealing player windows;
  browser tab selection additionally needs the [local SIE browser bridge](docs/media-reveal.md).
- Both Command Dial selection modes need the bundled
  `sie-switcher@olbrasoft.cz` adapter for GNOME 48, installed by `scripts/install.sh`.
  Its first activation requires one logout/login; the tray reports readiness.
  No separate GTK picker or GTK runtime is used. The installer needs Python 3 with
  PyGObject (`python3-gi`) to enable the adapter. Dial passthrough needs desktop-user
  access to `/dev/uinput` via the supplied [udev rule](docs/command-dial.md#local-access).

Tests additionally need `dbus-daemon` and `glib-compile-schemas` (Debian packages
`dbus-daemon` and `libglib2.0-bin`), `tmux`, CMake and a C/C++ compiler for whisper.cpp.
There is no GTK development dependency. Pinned upstream bindings are used during
native builds, so generating bindings with a local libclang is unnecessary.

## Build and run

```sh
cargo test --locked --all-targets
cargo run --locked -- --doctor
cargo run --locked
```

The default command runs the tray, maintains the lighting every two seconds,
and monitors the supported keyboard. While running, it maps M1–M5 and the three side buttons to native Linux macro codes so GNOME
does not also open Settings or toggle microphone mute. The original
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

The installer builds with CUDA when `/usr/local/cuda/bin/nvcc` and an NVIDIA GPU
are available; `SIE_CPU_ONLY=1 ./scripts/install.sh` builds the CPU version.
A CUDA build also requires the matching runtime libraries. The installer installs
a desktop launcher, user service and browser bridge assets. Load the unpacked
extension once as described in [media reveal setup](docs/media-reveal.md).
Installation does not enable automatic startup. If wanted:

```sh
systemctl --user enable sie.service
```

**Ukončit** exits successfully and the service does not restart it. To start it
again, use the desktop launcher or `systemctl --user start sie.service`.

## Media playback indicator

See [media target selection and validation](docs/media-lighting.md). This uses the
existing GNOME/MPRIS interfaces without a new extension. SIE observes playback;
the hardware media keys keep their native behavior. The tray reports the selected
player and state. Keyboard focus does not limit background media control.

Starting/resuming playback with the supported keyboard's Play/Pause button
reveals that player on its existing workspace and selects its existing video
tab. Pausing keeps the current workspace/window/tab focused. Native
playback still toggles once. Space and mouse playback changes update the LED
without moving focus. See [media reveal setup and limitations](docs/media-reveal.md).

## Local dictation

See [the migration and setup guide](docs/dictation.md) and
[example configuration](packaging/config.example.toml). Dictation is disabled
without explicit configuration. It reuses the installed local model,
`pw-record`, `pw-play`, `pactl`, `wl-copy`, `wl-paste`, PostgreSQL and the existing window
tracking/Mutter interfaces. No new GNOME extension or inference service was
installed on the development workstation.

After one-time database import, restart SIE. Bottom button release toggles
recording; Pause or Escape discards it. The middle side button has no action. While transcribing, another bottom
press is ignored. The menu shows recording/transcription and offers cancel.
GUI apps, ordinary terminals and CLI agents use their appropriate paste paths;
attached tmux terminals receive text through a private buffer into the focused
pane. `audio_source` and `sound_sink` select SIE's microphone and speakers without
changing system audio defaults. The enable/disable menu choice applies to the current run. A recording is limited
to five minutes by default. Bottom-button/tray/automatic completion pastes
without Enter. The top side button finishes and submits an active recording,
queues submission of the current transcription, or acts as Enter while idle.
Queued submission is suppressed by cancellation, empty output, failed paste or
a changed target.

```sh
sie --doctor
sie --config /absolute/path/config.toml
sie --transcribe-file /absolute/path/16khz-mono-s16.wav
```

The file command prints corrected text and never pastes. Microphone audio stays
in memory; completed raw/corrected text is stored in the independent SIE history.
Cancel discards the current recording or pending output. VirtualAssistant can
remain stopped when SIE owns dictation and spoken notifications.

The optional CUDA setting `flash_attention = true` can reduce inference latency;
it defaults to false and requires an application restart. See the
[local benchmark and reproduction steps](benchmarks/whisper-flash-attention/README.md)
before choosing it for another GPU or model.

## Development

Create issues before implementation and use GitHub's native sub-issue relations.
The [initial parent issue](https://github.com/Olbrasoft/sie/issues/1) tracks the
documentation, tray, lighting and macro-key tasks. CI runs:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
node --test browser-extension/background.test.mjs
```

Integration tests launch a private D-Bus and mock the device and compositor.
The PostgreSQL test is opt-in and refuses databases whose name does not start
with `test_sie_`; CI uses a disposable PostgreSQL service. To run it locally:

```sh
SIE_TEST_DB="host=localhost dbname=test_sie_dictation user=sie_test password=$SIE_TEST_PASSWORD" \
  cargo test --locked --test correction_database -- --ignored
```

Tests do not read typing, write real lighting, change desktop settings or switch
the logged-in user's workspaces. Physical M1–M5 presses and visual confirmation
of the keyboard colors are separate checks described in the design document.

## License

[MIT](LICENSE), copyright 2026 Jiří Tůma.

Codex F8 voice has its own device selection. The workstation setup for the same
Logitech microphone and ViewSonic speakers is documented in
[Codex voice audio routing](docs/codex-voice-audio.md).

Module responsibilities, ownership rules and the rationale for the current
patterns are documented in [architecture and maintenance](docs/architecture.md).
