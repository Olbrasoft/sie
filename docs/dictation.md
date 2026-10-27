# Local dictation migration

Version 0.4.0 ports VirtualAssistant's non-LLM Dictation path into Rust. The
source implementation is MIT-licensed by the same project owner. Relevant
behavior was reviewed in DictationKeyHandler, LightweightTextFilter,
DictationSpeechCoordinator and the clipboard output strategies. VirtualAssistant
source code, services, shortcuts, model files and database were not modified.

## Controls and lifecycle

The BlackWidow V4 Pro sends the left edge buttons as F18 (top), F19 (middle),
and F20 (bottom). The user physically verified middle then bottom during
calibration. XKB maps F20 to microphone mute. SIE temporarily maps all eight
macro/side buttons to Linux codes 656–663 on the supported device only, avoiding
that collision without grabbing or replaying normal typing. Quit and SIGTERM
restore the original map. Version 0.7.0 assigns the top side button to explicit
finish-and-submit.

- Bottom release: idle → recording → local transcription → paste → idle.
- Middle release: cancel recording or transcription; discard pending output.
- Top release while recording: stop → local transcription → paste → one Enter.
  Idle, disabled and transcribing states ignore it; it cannot arm a future session.
- Automatic recording-limit completion and bottom/tray completion remain
  paste-only. Submission belongs only to the recording finished with the top button.
- Repeats and presses are ignored for side buttons. Bottom during transcription
  is ignored. M1–M5 continue to use presses for workspace switching.
- Tray actions provide start/stop, cancel, module enable/disable and phase status.
- Cancellation flags, session generations and a native inference abort callback
  prevent stale results from being delivered by later sessions.
- Quit stops capture, cancels inference and releases optional speech coordination.

Audio is captured by PipeWire as 16 kHz mono signed 16-bit PCM into bounded memory,
with a configurable 1–600 second limit (default 300). SIE does not unmute a muted
microphone. It uses whisper-rs/whisper.cpp directly and caches the loaded model;
first inference includes model loading. CUDA is an optional build feature, with
CPU inference available in either build by setting `use_gpu = false`.

Version 0.8.1 adds the optional `flash_attention = true` CUDA optimization.
It defaults to false for existing configurations and is disabled when GPU use
is explicitly disabled or the binary was built without CUDA.
Restart SIE after changing it, because the model context is cached. Set it back
to false and restart to restore the original inference path. The model, beam
search and correction rules stay the same. See the [reproducible comparison](
../benchmarks/whisper-flash-attention/README.md) for measurements and limitations.

The pinned whisper-rs 0.16 abort wrapper has a callback-data type mismatch
([upstream source](https://docs.rs/crate/whisper-rs/0.16.0/source/src/whisper_params.rs));
SIE uses the native callback with an explicitly owned atomic cancellation flag.
The real-model regression verifies cancellation followed by repeated inference.

## Keyboard recording indication

Since 0.8.0, the ambient strip animates a rainbow during `Recording` and returns
to white when capture ends, including while transcribing. All three buttons and
the tray use the same phase signal. Lighting off overrides the animation. This
first stage is time-based, not reactive to voice intensity; see the separate
[effect design and voice-intensity feasibility assessment](recording-lighting.md).

## Sound cues

The three original VirtualAssistant MP3 assets and their MIT notice are included
in `assets/sounds`. SIE embeds the optimized recording WAV and the two other MP3s.
Successful recording start
plays `recording-start`; transcription loops `write`; active cancellation plays
`paper-rip`. Idle cancellation and failed starts are silent. Completion, error,
cancel, disable and shutdown stop the previous playback, including the long
writing loop. The same actor transitions drive side-button and tray sounds.
Playback runs asynchronously via `pw-play`, with only that child killed on
cancellation. Since 0.9.2 the recording cue uses decoded PCM WAV with its first
280 ms of near-silence removed: audio above -50 dB starts at about 10 ms instead
of 290 ms. Playback requests a 20 ms buffer instead of the client's default
100 ms. The selected output is unchanged; actual onset also depends on the
audio device waking from idle. Capture still starts before the cue is queued.
The recording-start cue can be audible to the microphone, matching
the source behavior; start speaking after it ends.

On the development workstation, SIE explicitly uses the StreamCam analog-stereo
capture node and the NVIDIA HDMI monitor sink. The camera audio card was initially
in the off profile; enabling its analog input profile exposed the microphone.
The system-wide TONOR input/Bose analog output defaults were left unchanged.

`sound_effects = false` disables feedback. `sound_sink` can select a PipeWire
output; by default SIE uses the current system output. Missing playback support
is logged without preventing dictation. Sound tests inspect the embedded PCM
onset and command arguments and select cues with playback disabled; CI never
plays real audio. Escape/Pause remain VirtualAssistant keys;
SIE cancellation is the middle side button or its tray action.

## Independent database and setup

Install runtime tools `pipewire-bin`, `pulseaudio-utils`, `wl-clipboard`, and a
local PostgreSQL instance. The one-time administration helper additionally needs
system Python's `python3-psycopg2`. Copy or install the large-v3-turbo GGML model
locally; no model download is performed by SIE. Use a desktop user with read/write
access to the supported Razer input devices, never a root GUI process.

On the development workstation, Unix peer authentication already gives the
current user access to PostgreSQL and permission to create a database:

```sh
/usr/bin/python3 scripts/import-dictation.py \
  --source-db virtual_assistant --target-db sie \
  --filters /opt/olbrasoft/virtual-assistant/app/Filters/text-filters.json \
  --write-config --coordinate-virtual-assistant
./scripts/install.sh
systemctl --user restart sie.service
sie --doctor
```

The importer refuses an existing target and never overwrites existing config.
It opens the source read-only, creates the independent target, applies
`migrations/001_dictation.sql`, copies corrections including IDs/active flags/
priority/case sensitivity/notes/timestamps, and imports hallucination filters.
It does not copy source history, secrets, LLM/provider configuration or unrelated
VirtualAssistant tables. Do not rerun it over the deployed database. The local
configuration is created with mode 0600; actual rules/config are not committed.
The input selection is validated against its own mute/availability state, not
against the unrelated default microphone. `--doctor` reports the configured
input/output nodes and desktop context.
The application also refuses a connection to the `virtual_assistant` database.

The schema contains correction rules, JSON postprocessing settings, rule usage
counts, transcription history and a version table. Migration 001 is additive and
idempotent. Fresh independent databases can apply it directly with `psql` instead
of importing source data. Future rule changes in either database are independent.
Rules are reloaded for each transcription; no service restart is needed for SQL
rule changes.

Postprocessing follows the source non-LLM order: whole-text and suffix
hallucination removal, active literal substring corrections (priority descending,
ID ascending, per-rule case sensitivity), spoken Czech `lomeno` to `/`, and trim/
collapse ordinary repeated spaces. Newlines are retained. Regex lookaround from
arbitrary future source filters is not supported by Rust's regex engine; invalid
filters fail preflight before microphone capture.

Completed nonempty raw/corrected transcripts and delivery status are stored in
SIE's local `dictation_history`. Audio is not written to disk. Canceled/stale
work is not saved. If the target window changes, SIE refuses to paste and saves
the undelivered transcription in history. Own logs contain state/counts/errors,
not dictated text. History retention and a rule editor are future work.

## Wayland output and coexistence

The same GNOME focus tracker used by VirtualAssistant's DesktopMonitor supplies
live active-application/window context. The Windows extension adds a stable
window ID and terminal process ID. SIE refreshes this context at delivery and
refuses to paste into a different window. The actual focused input field in a
GUI receives Ctrl+V; no mouse-coordinate guessing is involved.

Routing distinguishes GUI applications, ordinary terminals (clipboard +
Ctrl+Shift+V) and non-tmux CLI agents such as Codex/Claude (PRIMARY + Shift+Insert),
matching VirtualAssistant's three-way decision. Agent detection uses terminal
title/process metadata. No ordinary keystrokes are read or recorded.

For a terminal attached to the default local tmux server, SIE associates the
terminal process tree with its actual tmux client and active pane. It refuses
ambiguous matches. A private named tmux buffer delivers UTF-8 directly into that
pane with bracketed paste; the clipboard stays untouched. Default completion
sends no Enter. Explicit top-button completion waits for the paste event, rechecks
cancellation and the same focused window/pane, and sends Enter to that exact pane.
Only the target pane's copy-mode is canceled, and focus/pane identity and
cancellation are rechecked before paste. Each private buffer is removed after
success or failure. Separate/custom tmux sockets are not auto-discovered.

Non-tmux clipboard staging retries transient ownership loss up to four times,
with compositor setup completed first. Prior text or PNG contents are restored
if still owned by the dictation; newer user selections are preserved. Arbitrary
multi-format bundles are not preserved. Keep the intended input field focused
until delivery. Default completion does not submit; explicit top-button
completion waits for paste, rechecks focus/cancellation, and sends a separate
Enter. The transcript and clipboard text do not get a synthetic newline. Empty
output, cancellation, paste failure or changed focus/pane suppress Enter. If
text was inserted but submission fails, the tray says so; it does not retry
Enter automatically.

The existing focus tracker, Windows extension, AppIndicator and Mutter input API
are sufficient here. No additional GNOME extension was needed. No direct typed
key replay, synthetic Scroll Lock/Pause or whole-keyboard grab is used.

`virtual_assistant_speech_lock = true` optionally calls only the existing local
speech-lock start/stop endpoints at `127.0.0.1:5055`, using `curl`. It sends a
bounded timeout, no audio/text, and pauses spoken notifications while recording
or transcribing. Start fails visibly if this explicitly configured bridge is
unavailable. Cancel, completion and normal shutdown release it; after a crash it
expires automatically. The default is false for standalone installations.
VirtualAssistant remains running for existing features and legacy hotkeys.
LLM processing and automatic agent-window focusing were not ported. Submission
is an explicit per-recording top-button action.

## Reproducible checks

Automated tests run without a real model, microphone or desktop. Private D-Bus
mocks verify workspace/key releases/canceled output. The opt-in PostgreSQL test
uses synthetic rules in a database whose name must begin `test_sie_` and verifies
migration idempotence, correction ordering, filters, history and usage counts.

For an explicit manual Unicode output check, run the dedicated GTK fixture and
then the Rust example while that fixture is focused:

```sh
/usr/bin/python3 tests/fixtures/dictation-window.py /tmp/sie-dictation-probe.json
cargo run --example paste_probe
cargo run --example paste_probe -- --submit  # Also verify one Enter in the probe
```

The fixture records the resulting text and an Enter count in its JSON file.
The example refuses other target applications. The GTK dependency is solely for
this manual fixture, not the SIE application. Physical buttons and real audio
must be verified separately; see [validation results](validation.md).
