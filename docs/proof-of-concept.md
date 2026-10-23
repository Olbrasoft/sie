# System Interface Extensions proof of concept

SIE should gradually become the user's main application for extending computer
input and control. The first deliverable is a small, visible keyboard companion;
later iterations can take over selected dictation and control capabilities from
VirtualAssistant without interrupting the existing service.

## Scope of version 0.3.0

The application runs in the logged-in GNOME/Wayland session, displays a tray
indicator, owns a touch-typing lighting frame, and maps the five dedicated macro
keys to existing workspaces. A tray switch turns all RGB lighting off or on
without disconnecting the keyboard. The active workspace's macro key glows green
when lighting is enabled,
including after switches made through the mouse or other shortcuts. About displays the version compiled into the
installed binary. Quit and SIGTERM release the process and input subscriptions.
Starting a second controlling instance fails without replacing the first.

Speech capture, Whisper transcription, text-to-speech, model selection, mouse
automation, configurable profiles and support for other desktops are future work.
Whisper is a speech-to-text component; speech synthesis needs a separate engine.

## Why Rust

Rust fits this proof of concept: Linux event devices are supported by `evdev`,
D-Bus by `zbus`, and the standard tray protocol by `ksni`. Tokio handles input,
periodic device refresh and lifecycle events. The application needs no embedded
browser, GTK development libraries or Python background process.

The implementation is entirely Rust, apart from the shell installer. OpenRazer,
GNOME and its already installed extensions remain platform dependencies. Rust
does not bypass Wayland's compositor boundary. Future speech features should
initially reuse proven native engines through bindings, subprocesses or service
APIs; their model runtimes do not all need to be rewritten in Rust.

References: [ksni](https://github.com/iovxw/ksni),
[evdev](https://docs.rs/evdev/0.13.2/evdev/),
[zbus](https://github.com/z-galaxy/zbus).

## Existing desktop integration

The development workstation uses GNOME Shell 48 on Wayland. Inspection of
VirtualAssistant and live D-Bus introspection established these boundaries:

| Existing component | Role in SIE |
| --- | --- |
| OpenRazer daemon and kernel driver | Device discovery and custom RGB frame writes |
| GNOME AppIndicator extension | Display the Rust StatusNotifierItem and menu |
| `focus-tracker@olbrasoft.cz` | Read live workspace count and current index |
| Mutter RemoteDesktop D-Bus API | Send the configured workspace accelerator |
| `window-calls@domandoman.xyz` | Existing window control; no new dependency for this PoC |
| `computer-use-linux@avifenesh.dev` | Existing agent window control; no workspace activation method exposed |

The current tracker exposes workspace state but no activation method. The
installed window-control extensions also do not expose direct workspace
activation. VirtualAssistant's remote desktop command uses the existing
Super + numpad shortcuts. SIE reuses those shortcuts and sends input through
Mutter, whose session creation/start/stop works on this workstation. The existing
local computer-control experiments also use this Mutter interface.

**No new GNOME extension is installed or required for this setup.** SIE does not
use unrestricted Shell Eval, alter the installed extensions, change keybindings
or depend on VirtualAssistant being available. Mutter's interface is GNOME
specific; a different compositor needs its own adapter. `--doctor` checks the
available interfaces and configured shortcuts before hardware testing.

## Keyboard lighting

The supported device is `1532:028d` with an 8-row, 23-column matrix. SIE checks
both USB IDs and dimensions before writing. Other Razer models are skipped.

The previous `keyboard-razer-blackwidow-v4-pro` experiment provides the intended
colors and physical matrix coordinates only. SIE generates its own frame in
Rust, without running Polychromatic, importing its effect file or copying its
implementation. Character keys follow the established eight finger zones:

| Finger zone | Color |
| --- | --- |
| Left little finger | `#F0445A` |
| Left ring finger | `#FF8A3D` |
| Left middle finger | `#F6C453` |
| Left index finger | `#65B84B` |
| Right index finger | `#45A5C9` |
| Right middle finger | `#3657B7` |
| Right ring finger | `#7664B4` |
| Right little finger | `#D05291` |

Remaining keys, modifiers, navigation, numpad, inactive macro keys, media controls
and ambient LEDs are white. The active workspace's macro key is green
(`#00FF00`). The existing explicit accents are retained: the left
system key is Debian red (`#A80030`) and the main Enter is green (`#00FF00`).
Numpad Enter stays white.

The macro LED coordinates are `(row, column)`: M1 `(5, 1)`, M2 `(4, 1)`,
M3 `(3, 1)`, M4 `(2, 1)` and M5 `(1, 1)`, verified against the installed native
BlackWidow device map. The matrix's row order is reversed relative to key order.
Each frame starts with the established touch-typing colors and overlays exactly
one macro LED for GNOME indices 0–4. Higher indices or missing desktop state
leave all five macro LEDs white, clearing any previous highlight.

SIE subscribes to the existing tracker's `WorkspaceChanged` signal and reads
`CurrentWorkspace` and `TotalWorkspaces` afresh when it arrives. Lighting updates
do not inject keyboard input or depend on M1–M5 being pressed. The two-second
refresh also recovers state if signals are missed or the tracker restarts.

Each driver row contains `[row, first_column, last_column, RGB…]`; eight rows
form one 576-byte `ay` payload. SIE calls `setKeyRow` and then `setCustom` on
`razer.device.lighting.chroma`. A two-second refresh maintains ownership and
rediscovers hardware after reconnect or daemon restart. Error states remain
visible in the tray, whose Quit action remains available.

Only one lighting controller should actively render effects. SIE does not stop
Polychromatic or change its saved settings. This PoC does not restore the prior
controller's effect on exit, nor does it change global OpenRazer sync settings.
Keep OpenRazer effect synchronization disabled when independently controlling
multiple Razer devices.

Reference: [OpenRazer lighting API implementation](https://github.com/openrazer/openrazer/blob/master/daemon/openrazer_daemon/dbus_services/dbus_methods/chroma_keyboard.py).

## Lighting toggle

The tray shows `✅ Vypnout osvětlení klávesnice` when lighting is on and
`❌ Zapnout osvětlení klávesnice` when it is off. The icon describes the current
state and the text names the action. Both states remain clickable. The action
requests the specific displayed mode, so repeated clicks on a stale menu do not
accidentally reverse the user's requested state.

`LightingMode::Off` generates a complete black 8 × 23 frame instead of applying
any profile or workspace overlay. The normal refresh and workspace-change paths
use the same mode, so the keyboard remains dark when the workspace changes or
the hardware reconnects. Turning lighting on generates a fresh frame using the
current workspace. The saved hardware brightness is never changed.

Macro key monitoring and workspace activation operate independently of the RGB
mode. The selection is held for the current process; a new SIE process starts
with lighting on. `--tray-only` disables the lighting action. If the keyboard is
absent or the driver fails, the tray reports that state and retains the requested
mode for the next refresh.

## Macro keys and workspaces

User-facing workspace numbers start at one: workspace 3 means the third
workspace, whose GNOME index is 2. Tray text and discussions use this numbering.
The diagnostic `--switch-workspace` option explicitly accepts GNOME indices.

| Physical key | OpenRazer default event | Workspace number | GNOME index | Existing GNOME accelerator |
| --- | --- | --- | --- | --- |
| M1 | F13 | 1 | 0 | Super + KP 1 |
| M2 | F14 | 2 | 1 | Super + KP 2 |
| M3 | F15 | 3 | 2 | Super + KP 3 |
| M4 | F16 | 4 | 3 | Super + KP 4 |
| M5 | F17 | 5 | 4 | Super + KP 5 |

Linux `KEY_MACRO1` through `KEY_MACRO5` are also recognized. The application
opens only by-id paths for the supported keyboard, verifies its USB IDs and
monitors relevant keyboard interfaces. GNOME's default XKB map interprets F13
as `XF86Tools`, which opens Settings. While SIE runs, it temporarily maps only
the five macro scancodes to Linux `KEY_MACRO1`–`KEY_MACRO5` on each supported
device. Those codes do not launch the desktop's default actions. A scoped guard
restores the exact original mappings on Quit, SIGTERM or partial setup failure.
This requires write access to the existing input nodes, already granted by the
workstation's OpenRazer rules. It does not change GNOME's keybindings or
exclusively grab the device.
Ordinary typing is immediately ignored and never stored or logged. Releases and
autorepeat do not trigger actions. Input devices are rescanned every three
seconds so reconnects do not require restarting SIE.

Before a shortcut is sent, SIE reads the current count through D-Bus without
caching it and checks the target. Four workspaces permit M1–M4; M5 emits no
keyboard input. SIE does not create workspaces or alter dynamic-workspace
settings. GNOME may independently maintain an empty final workspace.

For a valid target, SIE verifies the existing shortcut, starts a short Mutter
input session, sends Super + the corresponding numpad key using Linux evdev
keycodes, releases both keys
even after a key-press error, and waits for the tracker to confirm activation.
Then it stops the session. Num Lock, conflicting bindings, held physical
modifiers or a compositor refusing input can prevent activation; failed
confirmation is logged instead of reporting success. No root daemon or
additional input-injection tool is installed.

Reference: [OpenRazer keyboard driver](https://github.com/openrazer/openrazer/blob/master/driver/razerkbd_driver.c).

## Verification

Automated tests cover the complete color matrix, row serialization, accepted
models, all five macro mappings, ignored events, menu actions and exact shortcut
matching. Private D-Bus integration tests exercise the real application binary,
all five target accelerators, live workspace-count changes, no input for absent
targets, key release on failure, OpenRazer byte-array calls and duplicate-instance
rejection. Tests also cover every active macro LED, unchanged finger zones,
workspace changes made without macro input, one-based tray labels and clearing
the highlight when workspace state disappears. CI runs formatting, Clippy and
tests on stable Rust and Rust 1.89.

For a real desktop smoke test:

1. Run `sie --doctor`; all required integrations should report OK.
2. Start SIE; verify its keyboard tray icon and click it.
3. Open About and verify version 0.3.0. Close the dialog.
4. Visually inspect the keyboard's finger zones, white remaining keys and accents.
5. Press M1–M5 and verify workspaces 1–5 (indices 0–4). Release and hold keys to
   check repeat behavior. Switch using the mouse or another shortcut: the active
   macro key should turn green and the previous one should return to white.
6. With four existing workspaces, press M5 and verify the active workspace stays
   unchanged. Do this in a suitable test session rather than removing occupied
   workspaces from the user's active desktop.
7. Unplug/reconnect the keyboard; input and lighting should recover.
8. Turn lighting off from the tray; verify the item changes to the on action and
   every RGB zone stays dark after workspace changes. Turn it on and verify the
   current workspace's M key and the finger zones return.
9. Choose Quit; verify the tray and process disappear. Restart through the launcher.

Physical key presses, visual LED inspection and reconnect testing must be
distinguished from mocked CI results in implementation reports.

## Subsequent increments

Keep each increment tracked by issues created before coding, with actual GitHub
GraphQL `addSubIssue` relations when work spans multiple tasks. Leave closure to
the maintainer after review.

The next candidates are configurable profiles and key assignments, a more
general desktop adapter, and staged migration of push-to-talk from
VirtualAssistant. Speech work should separate capture, Whisper transcription,
text insertion, command interpretation and text-to-speech. Existing model
services can continue running while SIE takes over one capability at a time.
