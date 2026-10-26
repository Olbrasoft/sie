# Proof of concept validation

Validated on 2026-10-23 with GNOME Shell 48.7 on Wayland, OpenRazer 3.12.4,
the connected Razer BlackWidow V4 Pro (`1532:028d`), and Rust 1.99.0.

## Combined lighting in version 0.9.0

- The workstation mouse is a Logitech G203 LIGHTSYNC (`046d:c092`) with HID++ RGB
  Effects version 0 and one Logo cluster. Direct read-only discovery and reversible
  RGB power commands succeeded; read-back confirmed PowerOff 3 and restored
  FullRgb 1/device control 0. Onboard-profile mode remained 1 throughout.
- A scoped udev rule grants the active desktop user access to USB interface 1
  only. After applying it to that node, its control descriptor became readable/
  writable without sudo, and the separate pointer hidraw node stayed inaccessible.
- Mouse protocol/fake worker tests are isolated from actual hardware. Existing
  private D-Bus checks continue to validate the keyboard's matrix and dark mode.
  See [the design and physical checks](mouse-lighting.md). Real tray/visual
  verification after deployment is reported separately from automated tests.

## Optional Whisper Flash Attention in version 0.8.1

- Formatting, Clippy with warnings denied and 73 isolated Rust tests passed.
  Existing configurations retain the original inference path; an explicit flag
  enables CUDA Flash Attention. CPU mode disables it. The diagnostic command
  reports the selected effective flag without loading or transcribing audio.
- The explicit CUDA real-model regression passed with Flash Attention off and
  on: initial transcription, cancellation during inference, then three identical
  successful transcriptions using the same cached model in each setting.
- Three locally synthesized Czech clips (14–99 seconds) and 20 public FLEURS
  Czech validation clips were measured in off/on/on/off blocks. See the
  [full methodology, timings, transcripts and quality limitations](
  ../benchmarks/whisper-flash-attention/README.md). The sums of warmed sample
  medians decreased by 28.3% and 38.1%, respectively. This does not measure
  microphone-to-paste latency or prove identical accuracy on every utterance.
- No microphone, clipboard, dictation database or physical keyboard was used
  by these benchmarks. Driver, firmware and boot configuration were unchanged.
  Physical microphone dictation and delivery after deployment require the
  user's separate check.

## Explicit dictation submission in version 0.7.0

- The top side-button release now finishes only an active recording and passes
  a per-session submit mode to delivery. Bottom/tray/recording-limit completion
  remain paste-only; idle/transcribing top presses do not arm submission.
- Formatting, Clippy and 69 isolated Rust tests passed. Private compositor tests
  cover the three clipboard shortcut paths followed by exactly one Enter,
  failed paste, cancellation before/after paste, focus changes and empty text.
  A private tmux terminal with numbered canonical input confirms exactly one
  submitted line and no submission for paste-only/canceled/refocused output.
- A real dedicated GTK probe received the Czech Unicode phrase through the
  ordinary path with Enter count zero, then through submit mode with Enter count
  exactly one. No unrelated application received test text or Enter; the original
  terminal focus was restored. This probe checks delivery independently of speech.
- Physical top-button/microphone confirmation is tracked separately in the
  deployment issue. Database schema/rules and audio routing are unchanged.

## Resume-only media reveal in version 0.6.1

- The user reported that version 0.6.0 reveals the player even when pausing a
  background video. The requested behavior now keeps the current workspace,
  window and tab focused on pause, and reveals only when starting/resuming.
- Formatting, Clippy and 65 isolated Rust tests passed. New private D-Bus actor
  checks cover delayed playback confirmation, pause without activation/Raise,
  unique native destinations, delayed input processing, a very recent mouse
  start, unknown/unconfirmed states and zero additional playback commands.
- The media watcher retains the pre-transition state and its timestamp across
  unchanged refreshes. Media actions retain their physical evdev timestamp;
  input-queue delays cannot reverse the interpreted toggle direction.
- Physical start-versus-pause verification of this patch remains separate from
  automated checks in the deployment issue.

## Media reveal in version 0.6.0

- Formatting, Clippy with warnings denied, 59 isolated Rust tests and four
  JavaScript tests passed. The additional tests exercise native framing,
  a spawned Rust host over pipes/private D-Bus, ambiguous tabs, existing-window
  activation and the absence of extra playback commands.
- A CUDA release and both Rust binaries were installed. SIE 0.6.0 is running;
  the unpacked SIE Media Tab Bridge was loaded into the existing Chrome profile
  without restarting Chrome. Its native host registered the same process ID as
  Chrome's MPRIS service.
- Real physical workspace/tab activation is tracked separately in the deployment
  issue; automated checks do not claim physical verification.

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
- Physical on/off behavior was additionally checked using 1920 × 1080 snapshots
  from the connected Logitech StreamCam. The images clearly distinguish the
  illuminated RGB matrix from the unlit keyboard. White, blue/cyan, pink/purple
  and green areas can be distinguished, but the current oblique, soft image
  does not reliably identify every key label or precise color shade. Photos
  were kept locally and opened in the desktop image viewer.

## Local dictation in version 0.4.0

- Formatting and Clippy with warnings denied passed. The isolated suite has 33
  unit tests and 12 private D-Bus integration tests. The additional PostgreSQL
  test passed against a dedicated `test_sie_dictation` database, including a
  repeated migration, synthetic corrections, filters, history and usage counts.
- The one-time import copied 191 correction rows (190 active) into the new `sie`
  database. Source and destination rows were compared, and source data remained
  unchanged. Real rules/configuration were not committed.
- A native CUDA build succeeded with the installed CUDA toolkit and RTX 3060.
  A local synthetic Czech WAV was transcribed using the installed Turbo model.
  This checks functionality, not transcription accuracy on natural speech.
- A real GTK test window received the Unicode phrase “Příliš žluťoučký kůň úpěl
  ďábelské ódy.” through the new clipboard/Mutter output path. No Enter was sent.
- The user calibrated the middle/bottom side buttons. Both real keyboard input
  interfaces expose codes 656–663 under SIE, and microphone mute remains off.
  The tray exposes recording, transcription, cancel and enable/disable actions.
- Initial repeated microphone attempts revealed spurious native inference
  aborts. Inspection identified a callback-data mismatch in whisper-rs 0.16's
  safe abort wrapper. SIE now passes a live AtomicBool through the native FFI
  callback, with explicit lifetime reasoning and a cross-thread unit test.
  An opt-in real-model regression passed on CUDA: initial transcription,
  cancellation during inference, then three successful identical transcriptions
  using the same cached model. The cancellation emits an expected native encode
  abort message; subsequent uncanceled calls succeed.
- VirtualAssistant's optional existing local speech lock was observed locked
  during SIE recording and unlocked after cancellation. The source service and
  its Scroll Lock/Pause shortcuts remain available.
- After deploying the callback fix, the user physically confirmed both bottom
  start/stop with text insertion and middle-button cancellation with no additional
  text. The first, failed microphone attempts are recorded above separately.

The model regression is intentionally opt-in, using an explicitly supplied local
model and synthetic 16 kHz mono signed 16-bit WAV:

```sh
SIE_TEST_MODEL=/absolute/path/ggml-large-v3-turbo.bin \
SIE_TEST_WAV=/absolute/path/synthetic.wav \
CUDACXX=/usr/local/cuda/bin/nvcc CUDA_PATH=/usr/local/cuda \
CMAKE_CUDA_ARCHITECTURES=86 \
  cargo test --locked --release --features cuda --test speech_model -- --ignored
```

Omit CUDA build arguments/features for the CPU smoke test. No real model or
microphone is used by default CI tests.

- The three original sound assets are embedded with the source MIT notice.
  PipeWire playback from stdin was verified; the user confirmed hearing all
  three actor cues. Isolated tests cover cue selection, idle
  cancellation, failed starts and disabled playback without producing sound.
- An initial terminal delivery failed while staging PRIMARY. The undelivered
  text was preserved in history. A repeat probe reproduced loss of selection
  ownership (wl-copy exited successfully before delivery). Staging now explicitly
  closes stdin, retries ownership up to four times and creates the compositor
  session before staging to reduce the race window. A dedicated Alacritty probe
  delivered Unicode through PRIMARY + Shift+Insert after this change. A further
  repeat was safely refused after focus moved away from the test window; it is
  not counted as successful delivery. The competing owner was not identified;
  no clipboard-manager configuration was changed.

### Terminal routing and explicit audio devices

- The user reported that real terminal dictation still failed despite a successful
  dedicated probe. This was treated as an unresolved terminal regression, not
  generalized from the earlier GUI/terminal smoke results.
- Reviewed VirtualAssistant's DesktopMonitorBackgroundService, DesktopContextService,
  WaylandTerminalDetector, TerminalAgentIdentifier, paste routing and tmux guard.
  SIE now reads the same focus tracker, combines window ID/title/PID, distinguishes
  GUI/ordinary-terminal/CLI targets and resolves only the actual attached tmux
  client pane. Tmux delivery uses a private buffer, bypassing PRIMARY races.
- A private tmux server test passed: Czech Unicode reached exactly its target,
  no Enter was sent, only that pane left copy-mode, a different pane remained in
  copy-mode, canceled/refocused output was suppressed and private buffers were
  removed. No real tmux pane or logged-in desktop is touched by this test.
- The StreamCam audio card was initially off. Its analog input profile was enabled,
  and SIE config now explicitly selects that camera capture node and the existing
  monitor HDMI sink. Their mute states were checked independently. System defaults
  remain TONOR capture and the analog speaker output.
- The user confirmed real terminal delivery by dictating and sending the next chat
  message directly from the same Codex/tmux terminal. The prior failed terminal
  attempts are not counted as successful verification. Monitor-speaker confirmation
  after the explicit route change remains separate from the earlier cue confirmation.
- CI database authentication uses an encrypted repository secret; the replacement
  PR contains no hardcoded database password. The earlier public test password was
  flagged by GitGuardian; no production credential was involved.
