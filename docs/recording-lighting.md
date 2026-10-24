# Recording indication on the ambient strip

SIE 0.8.0 adds a moving rainbow to the keyboard/wrist-rest ambient strip while
Dictation is in `Recording`. During silence it keeps moving: it indicates that
the microphone is recording, rather than attempting to detect speech.

The strip returns to white as soon as SIE reports `Transcribing`, `Idle`,
`Error` or `Disabled`, including bottom/top completion, automatic recording
limit, cancellation and disabling dictation. Normal Quit/SIGTERM restores the
idle strip. The complete lighting-off mode always wins and stays black during
recording, workspace/media changes and reconnects.

Finger zones, active M1–M5, red Escape/system key and the Play/Pause indicator
retain their current colors. No additional model, service, extension or capture
process is needed for this effect.

## Verified device boundary and coordinates

The installed OpenRazer 3.12.4 adapter identifies USB `1532:028d` as an 8 × 23
matrix with custom-frame support. The native
[BlackWidow V4 Pro map](https://github.com/polychromatic/polychromatic/blob/master/data/devicemaps/blackwidow_v4_pro_en_US.svg)
contains 18 keyboard ambient cells on row 6, columns 0–17, and 20 wrist-rest
cells on row 7, columns 0–19. The animation follows this physical order:

```text
keyboard left: (6, 0)..(6, 8)
wrist rest:    (7, 0)..(7, 19)
keyboard right: (6, 9)..(6, 17)
```

The manufacturer's [device guide](https://dl.razerzone.com/master-guides/RazerSynapse3/BLACKWIDOWV4PRO-00000653-en.pdf)
describes lighting along both chassis sides and three wrist-rest sides. These
form the visible perimeter; there is no invented rear strip. Unused matrix
cells are left at their normal values.

SIE continues using its custom matrix instead of a global built-in wave or
spectrum effect, so key colors remain under SIE's control. The
[OpenRazer adapter](https://github.com/openrazer/openrazer/blob/master/daemon/openrazer_daemon/hardware/keyboards.py)
and installed source were inspected; no upstream implementation was copied.

## Rendering and resource cost

A dedicated asynchronous writer serializes all lighting updates. Desired state
uses a latest-value watch channel, so slow USB writes do not block the input or
dictation loop and stale frames cannot accumulate. Every write revalidates USB
IDs and matrix dimensions. Full frames preserve the complete current profile
on hardware reconnect rather than initializing only ambient rows.

The nominal animation cadence is 8 frames/second, with a six-second color cycle.
The phase uses elapsed recording time and survives workspace/media/mode changes.
Missed ticks are skipped; slower hardware produces fewer frames rather than a
catch-up burst. When idle or fully dark, periodic recovery returns to once every
two seconds. State changes still request an immediate frame.

One encoded full frame is 576 bytes, so nominal animation frame data is about
4.5 KiB/second, excluding D-Bus/USB overhead. This is a calculated traffic budget,
not a measured CPU result. Color generation changes 38 cells with integer math;
no audio analysis is performed. The extra hardware writes are the main difference
from the previous static profile.

## Separate assessment: reaction to voice intensity

The existing `audio::Recording` captures raw 16 kHz mono signed 16-bit PCM through
one `pw-record` child. Its bounded reader currently collects bytes until recording
stops; it does not publish live levels. That same stream can support a meter
without another microphone capture or an AI model.

A future change would:

1. Read bounded chunks instead of only `read_to_end`, preserving duration/byte
   limits, sample alignment, stop/cancel behavior and the exact PCM for Whisper.
2. Calculate RMS over approximately 20–50 ms windows, normalize or convert to dBFS,
   and apply a noise floor plus attack/release smoothing.
3. Publish only the latest aggregate level through a bounded watch channel while
   recording. The renderer can retain its existing 8 Hz budget and map the level
   to ambient brightness or pulse strength.
4. Test silence, fixed sample amplitudes, clipping, noise gating, smoothing,
   partial reads, cancellation and recording limits with synthetic PCM; then
   calibrate the actual camera microphone in the room.

| Variant | Work and runtime impact | Current scope |
| --- | --- | --- |
| Time-based rainbow | Matrix mapping, bounded rendering and recording lifecycle; no audio processing | Implemented |
| Voice-intensity response | Streaming-reader change, RMS, level channel, smoothing/noise calibration and additional audio tests; same renderer/recorder | Assessed, not implemented |

The second option is a moderate follow-up rather than a new inference subsystem.
The arithmetic at 16,000 samples/second should be small relative to Whisper
inference, but that is an engineering estimate, not a benchmark. Most extra work
is integration, tests and microphone calibration. Loudness also responds to room
noise, speaker cues and keyboard sounds; it does not by itself identify speech.

Recommendation: use the recording rainbow first. It provides a clear indication
even while the user pauses speaking. Add a voice-strength pulse only if that
additional feedback is still useful after trying the first stage. The assessment
is complete; a voice-reactive implementation is outside this change.

## Validation

Isolated frame tests cover every matrix cell, the 38-cell restriction, moving
colors, exact cycle repeat, white restoration and the complete-black override.
A private D-Bus writer test covers animation, unchanged key rows, simultaneous
workspace/media changes, lights off, absence/reconnect, recording stop and normal
shutdown during recording. No real keyboard or microphone is used by those tests.
Physical strip visibility and installed-frame observations are recorded separately
in the issues and pull request.
