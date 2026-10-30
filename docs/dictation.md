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
- Since 0.11.0, top release during transcription queues one submission for that
  job, after successful paste and another window/pane/cancellation check. Repeated
  requests coalesce and cannot arm a later session. Submission remains cancelable.
- Top release while idle, disabled or in error sends a literal Enter to the
  currently focused input, leaving the clipboard and dictation state untouched.
- Automatic recording-limit completion and bottom/tray completion remain
  paste-only unless the user explicitly requests submission with the top button
  while that job is still active. An idle top press can submit already pasted text.
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

### Short cues after monitor audio has been idle

The development workstation's ViewSonic VX3276-QHD output was automatically
suspended after five seconds of inactivity. After trimming the recording cue,
the user reported that the first cue after silence sometimes went unheard,
while immediate repeated cues worked. Successful PipeWire playback and active
graph links do not establish whether the monitor speakers were already awake.

The optional [ViewSonic WirePlumber fragment](../packaging/60-sie-viewsonic-awake.conf)
keeps that specific stereo output open by setting
`session.suspend-timeout-seconds = 0`. The shortened cue and 20 ms latency request
remain unchanged. This follows the existing session manager's
[per-node suspend policy](https://pipewire.pages.freedesktop.org/wireplumber/daemon/configuration/alsa.html#node-properties).
It trades idle audio power saving for prompt short-cue playback. It does not
change the default output, any microphone policy or playback volume.

On this workstation, install the fragment in
`~/.config/wireplumber/wireplumber.conf.d/60-sie-viewsonic-awake.conf`, then restart
the user `wireplumber.service` while dictation and other audio sessions are idle.
The installer does not apply this machine-specific policy automatically. On
other machines, identify the intended sink with `pactl list short sinks` and
adjust the exact `node.name` match first. Remove the fragment and reload
WirePlumber to restore its normal idle suspension policy.

For manual verification, leave the output without playback for at least ten
seconds. The matched PipeWire node should remain idle with the ALSA PCM endpoint
running, instead of becoming suspended. Repeat recording starts separated by
quiet intervals and confirm each cue through the physical monitor speakers;
automated Rust tests do not use desktop audio or a microphone.

## Media volume while recording

Since 0.10.0, an optional `duck_sink` names the one playback output to attenuate
during microphone capture. The portable default is unset, so other installations
do not change any output volume. On the development workstation it selects Bose
(`alsa_output.pci-0000_0a_00.4.playback.0.0`), separate from ViewSonic feedback.

`duck_volume_percent = 20` scales each channel's existing PulseAudio volume value
to one fifth, preserving channel balance and the mute flag. This is relative to
the volume at recording start, not an absolute output level. Values from 0 to 100
are accepted. Use an explicit stable sink name, not an index or `@DEFAULT_SINK@`;
the configured feedback output must be different.

The dictation actor only signals a separate serialized volume worker. It requests
restoration before awaiting capture shutdown on normal finish, finish-and-submit
and the recording deadline, and also on cancellation, disable or graceful exit.
Transcription can proceed concurrently and does not prolong attenuation. Failed
recording starts never request attenuation. Missing outputs or volume errors are
logged or ignored without preventing dictation; volume subprocesses have bounded
timeouts and failed restoration is retried.

Restoration uses the exact saved per-channel values while the same output still
has SIE's attenuated volume. A manual volume change supersedes that saved state;
it is preserved rather than overwritten. Output name, index, channel map and
PipeWire serial guard against restoring into a replacement device. Force-killing
the process or losing the audio server cannot guarantee restoration.

Tests use in-memory output controls, including delayed/failed writes, and never
change real desktop audio or record a microphone.

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
sends no Enter. A top-button request during recording or transcription waits for
the paste event, rechecks
cancellation and the same focused window/pane, and sends Enter to that exact pane.
Only the target pane's copy-mode is canceled, and focus/pane identity and
cancellation are rechecked before paste. Each private buffer is removed after
success or failure. Separate/custom tmux sockets are not auto-discovered.

Non-tmux clipboard staging retries transient ownership loss up to four times,
with compositor setup completed first. Prior text or PNG contents are restored
if still owned by the dictation; newer user selections are preserved. Arbitrary
multi-format bundles are not preserved. Keep the intended input field focused
until delivery. Default completion does not submit; a top-button request for the
current job waits for paste, rechecks focus/cancellation, and sends a separate
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
is an explicit top-button action. Outside an active job the same button sends
literal Enter to the current input, so it can send text already inserted by a
previous paste-only completion.

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

When [standalone SIE notifications](spoken-notifications.md) are enabled, speech
coordination stays inside the Rust process and the legacy VirtualAssistant lock
is bypassed. Set `virtual_assistant_speech_lock = false` after migration.
