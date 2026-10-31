# Architecture and maintenance

SIE is a Rust desktop process composed from small domain components and platform
adapters. The application owns the components; adapters do not reach back into
the tray or launch other application components. The Rust public module facades
remain stable so native helpers, examples and integration tests retain their
existing entry points.

## Responsibilities

| Area | Owner and boundary |
| --- | --- |
| CLI and diagnostics | `main.rs` parses options and selects a mode. `doctor.rs` performs read-only integration checks. Neither implements dictation or provider policy. |
| Application composition | `app/mod.rs` creates channels/components and coordinates their events. `routing.rs` routes explicit device actions; `projection.rs` updates lighting/tray state; `workers.rs` owns critical tasks through startup failure and bounded shutdown. |
| Dictation | `dictation/mod.rs` exposes commands/status. `actor.rs` serializes recording transitions. `jobs.rs` owns generation identity, cancellation, pending inference and deferred submission. `pipeline.rs` coordinates local inference, corrections, history and guarded delivery. |
| Desktop | `desktop` re-exports a stable facade. `workspaces.rs` observes/validates existing workspaces and shortcuts; `focus.rs` resolves current focus; `input.rs` owns compositor input sessions; `delivery.rs` stages/restores clipboard selections and issues paste receipts. |
| Spoken notifications | `notifications` coordinates a serial durable queue and recording gate. `api.rs` validates compatible loopback requests; `store.rs` owns PostgreSQL transactions, capacity, consumer locking and quota persistence. |
| Speech synthesis | `tts/mod.rs` owns order, timeouts and fallback policy through the `Synthesizer` boundary. `circuit.rs` owns cooldowns; `credentials.rs` loads keyring references. `providers` owns Google rotation/reservations, bounded HTTP requests and temporary-file CLI adapters. `ProviderKind` gives validation and dispatch one exhaustive provider vocabulary without changing serialized settings. |
| Keyboard | `lighting.rs` renders pure frames; `openrazer.rs` validates hardware and writes frames. `input.rs`, `keymap.rs` and `dial_input.rs` isolate supported device controls and restore owned mappings. `macros.rs` interprets explicit control events. |
| Mouse | `mouse_lighting/mod.rs` owns desired mode and reconnect/retry behavior. `protocol.rs` implements volatile RGB effects over a small transport trait. `transport.rs` validates the supported control interface and performs bounded HID transactions. |
| Shared media attenuation | `ducking.rs` serializes volume snapshots and recording/speech ownership over a testable control interface. Requested/applied channels are one optional value, preventing partially initialized handles. Large fake-device tests live alongside the owner in `ducking/tests.rs`. |
| Audio and inference | `audio.rs` owns bounded microphone capture; `sounds.rs` owns asynchronous cues; `speech.rs` caches the local model. `speech_lock.rs` coordinates native speech or the optional legacy endpoint. These adapters do not choose input actions. |
| Media | `media.rs` owns MPRIS selection and playback state. `media/native.rs` contains the optional dedicated GNOME media-key observer. `media_focus.rs` decides when/where to reveal resumed playback. `switcher.rs` owns a native picker session. |
| Browser | `browser/broker.rs` correlates sessions and pending requests; `wire.rs` bounds native-message framing; `host.rs` bridges stdin/stdout and the local D-Bus interface. The small browser extension only matches and activates tabs. |
| Agent notification origin | `agent_origin/model.rs` defines validated metadata; `resolve.rs` purely selects/revalidates an origin; `processes.rs` reads bounded process/PID metadata; `tmux.rs` queries routing metadata; `desktop.rs` assembles snapshots and activates a revalidated target. `agent_notifications.rs` owns the desktop notification protocol. |
| Corrections and terminal delivery | `corrections.rs` applies correction rules and persists dictation history. `terminal.rs` resolves/delivers to the exact tmux pane, with private-process fixtures in `terminal/tests.rs`. `context.rs` reads the existing desktop tracker. |
| GNOME and helper scripts | The GNOME adapter delegates popup ownership to `controller.js` and workspace scoping to `scope.js`; confirmation uses GNOME's native popup. Python observers separate delivery, desktop helper ownership and request observation. One-time import/install/benchmark tools remain separate from the Rust runtime. |

## Patterns and ownership

The component event loops use bounded command channels and watch channels for
latest status. A single actor owns mutable state that must change serially:
recording, notification playback, media selection or hardware effects. The
composition root routes events rather than implementing those policies itself.
No dependency-injection container, inheritance hierarchy or general plugin
framework is needed.

Traits are used where replacing a side effect improves verification: speech
synthesis, volume control and mouse transport/backend. The speech fallback chain
can therefore exercise errors, timeouts and circuit state with a fake backend,
without keyring access, network calls or PostgreSQL. Concrete adapters remain
simple structs/functions; merely moving a function does not require a new trait.

Critical workers use an abort-on-drop owner. Normal exit closes command channels,
allows a bounded drain and aborts/joins only the remaining owned task. Startup
failure also releases worker resources. Dictation jobs retain cancellation
ownership until completion; dropping their owner sets the inference abort flag.
Generation checks reject stale completions, and a consumed paste receipt prevents
repeated top-button presses from sending Enter twice.

Normal recording completion and the duration limit call the same stop/drain/start
path. Volume restoration is requested before that path awaits capture shutdown;
transcription does not extend recording attenuation. Notification and recording
owners continue sharing a single media-volume snapshot.

Safety boundaries remain in the concrete adapters: supported USB identity,
matrix/control descriptor validation, balanced key release, focus and pane checks,
bounded frames/processes, loopback-only APIs and credential-safe errors. Pure
routing and policy code never reads ordinary typing or captures diagnostic audio.

## Review choices

Public APIs and serialized configuration are kept compatible. Facade modules
re-export types/functions while their internal responsibilities change. No new
runtime service or dependency is introduced by the refactoring.

Small adapters with one coherent purpose remain together. For example,
PostgreSQL queue/quota operations share one transaction/connection owner, and the
recording actor retains its transition loop. Different terminal-recognition rules
are intentional: dictation supports broad input targets, while agent-origin
activation uses a stricter identity policy. Combining those rules would change
behavior.

The GNOME and browser adapters already separated testable policy from the platform
UI. Their code is retained rather than rewritten for style. Legacy speech-lock
compatibility and one-time import tools remain available; removing them would
change supported deployment behavior.

## Verification

Required checks are `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets -- -D warnings` and
`cargo test --locked --all-targets`, on stable Rust and the declared Rust 1.89
minimum. The large private D-Bus target is divided into lighting, workspace,
delivery, context and switcher suites; all use the same explicitly private bus
fixture as the media and agent-notification tests. They never use the logged-in
session bus. Private PostgreSQL checks require an explicit `SIE_TEST_DB` whose
name begins with `test_sie_`; they do not use the production dictation database.

The refactoring adds checks for stale/canceled job completions, owner-drop
cancellation, provider timeout/fallback behavior, worker cleanup during startup
failure/deadline/graceful exit, and preservation of independent lighting fields.
Existing clipboard, deferred Enter, matrix, protocol, volume, queue, quota and
recording-gate checks remain in place. Node and Python suites verify the unchanged
extension and observer boundaries. Physical checks and deployment are reported
separately from isolated automated tests.
