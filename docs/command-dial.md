# Command Dial window switching

SIE 0.13.1 supports the top-left Command Dial beside Escape on the full-size
Razer BlackWidow V4 Pro (`1532:028d`, OpenRazer matrix 8 × 23).

| Action | Behavior | Dial color |
| --- | --- | --- |
| Rotate without opening the picker | Activate the next/previous eligible window on the current workspace | Green during activation, with a short 150 ms visible completion indication |
| Press | Open GNOME's native Alt+Tab application strip across workspaces | Orange |
| Rotate with the strip open | Select the next/previous application without activating it | Orange |
| Press again or Enter | Activate the selected existing window and its workspace | White |
| Escape, click outside, or eight seconds of inactivity | Cancel without activating a selection | White |

Idle is white. Lighting off takes precedence and keeps the dial black while
controls remain usable. Recording rainbow, finger zones, workspace and media
indicators share the existing single Rust renderer and retain their behavior.
Quit, disconnected dial input and picker errors clear switching feedback.

The global strip is the actual GNOME `AppSwitcherPopup`, with the same icons,
application grouping, initial next-application selection and window thumbnails
as Alt+Tab. Native keyboard and mouse navigation remain available. The dial
advances applications; GNOME's thumbnail navigation can select another window
of an application. Browser tabs are not separate switcher entries. There is no
additional application window or custom GTK imitation.

Local rotation cycles concrete normal/dialog windows on the current workspace.
Desktop/dock/menu windows are excluded; at most 128 eligible windows are used.
Local cycling uses stable window-ID order rather than focus order, avoiding
bouncing between the two most recently used windows. A local target must still
exist on the original current workspace when activated. No workspace is created.

## Existing desktop integration

Rust owns device handling, local window policy and lighting feedback. Window Calls
provides `List` and `Activate` on `org.gnome.Shell.Extensions.Windows`; the
installed Olbrasoft tracker provides `CurrentWorkspace`.

The bundled `sie-switcher@olbrasoft.cz` extension is a narrow native-UI adapter
for GNOME 48. Existing Window Calls, Desktop State Tracker and Computer Use
interfaces do not expose the switcher lifecycle; opening synthetic Alt+Tab also
does not change their focused-window value when the strip is opened or canceled.
The adapter therefore creates one native popup directly in no-modifier mode.
It changes timeout/navigation methods only on that instance, never on Shell
prototypes. No additional daemon is installed. GNOME handles application
selection and activation, Escape, mouse cancellation and popup destruction.

The session-bus interface `cz.olbrasoft.SIE.Switcher`, object
`/cz/olbrasoft/SIE/Switcher`, offers `Version`, `Open`, `Move`, `Finish` and
`Active`. SIE 0.13.1 requires protocol version 2, which includes fresh-timestamp
confirmation. An older already-loaded adapter is reported as requiring a new
login rather than being treated as ready just because its bus name exists.
Each popup belongs to its caller's unique bus name and generation token, so
another caller or stale token cannot move/confirm it. Caller disappearance or
extension disable cancels. Like the native keybinding, opening requires the
normal unlocked desktop; locking cancels any owned popup. The extension resets its eight-second cancellation
timer during activity; Rust checks popup state every 100 ms while global mode
is active to restore white after native dismissal. Dial disconnect and SIE
shutdown request cancellation too. Communication errors clear feedback, while
the Shell-side timer bounds a popup whose caller can no longer reach it.

Dial confirmation runs in a D-Bus callback rather than an input-event handler.
It obtains a fresh compositor timestamp with
`Meta.Display.get_current_time_roundtrip()` immediately before the native
`_finish()` call. `global.get_current_time()` may return zero or stale time in
this context, causing GNOME's activation guard to leave the old window focused
even though the strip closes. Escape and idle cancellation never activate a
window. Failed confirmation cancels the popup and clears its timer/ownership.
See the upstream [event timestamp contract](https://github.com/GNOME/gnome-shell/blob/48.7/src/shell-global.c#L1285)
and [application activation guard](https://github.com/GNOME/gnome-shell/blob/48.7/src/shell-app.c).

GNOME's `org.gnome.shell.app-switcher current-workspace-only` setting must be
false for global application switching; that is the workstation's existing
value. The adapter refuses workspace-only mode rather than silently offering
an incomplete global list. It does not modify this preference or keybindings.

SIE does not hold virtual Alt or another modifier or inspect ordinary typing.
It does not ask an application to open a replacement window. The adapter uses
installed GNOME Shell UI APIs; its metadata explicitly targets version 48.
Other Shell versions require validation before expanding that support.

## First activation

`scripts/install.sh` copies the adapter and enables only its UUID, preserving
other enabled extensions. GNOME 48 discovers new local extensions at session
startup, so the first installation requires a logout and login. The installer
does not restart Shell or log the user out. The tray reports “odhlaste se a
znovu přihlaste” until the adapter's bus name is available. Existing local
cycling, dictation, lighting and media behavior remain usable in that state.
If SIE is not configured to start at login, run `sie start` after logging in.
Updates to already loaded Shell JavaScript also take effect at the next login.

## Input boundary

In the installed OpenRazer 3.12.4 driver mode, dial press is `KEY_F24` on the
keyboard interface; SIE remaps only its scan code to `KEY_MACRO9` while running
and restores it on exit, just like the existing macro/side keys. Only release
activates it; autorepeat and press events are ignored. Keyboard nodes are never
grabbed, and ordinary typing is neither stored nor replayed.

Rotation is `REL_HWHEEL` on the dedicated `-if02-event-mouse` interface.
The matching high-resolution event is a duplicate and is not counted twice.
Clockwise advances; counterclockwise goes back. This interface is exclusively
owned only after verifying exact USB IDs, path, required horizontal axis and
a narrow mouse/volume/control-key capability whitelist. A node advertising
ordinary typing keys is rejected. Any absolute axis must be `ABS_VOLUME`.

A virtual mouse/control device is created before grabbing that mouse-only
interface. Non-dial pointer, button and volume events are forwarded unchanged;
only the dial horizontal axes are consumed. The separate right volume roller
therefore remains available. Failure to create the replacement leaves the
physical interface untouched. The actual Logitech mouse is not involved.
Input and command queues are bounded; unplugging retries without retaining
events or application titles in logs.

The installed kernel driver's BlackWidow V4 Pro event mappings and the upstream
[OpenRazer report](https://github.com/openrazer/openrazer/issues/2228) were
inspected. The dial LED is matrix row 0, column 1. No external dial daemon or
third-party implementation is bundled or required.

## Local access

Run SIE as the desktop user. Existing OpenRazer input access remains required.
The evdev uinput builder needs read/write access; on this workstation the input
group initially had only write access. Grant access to the active local user:

```sh
sudo install -m644 packaging/70-sie-dial-uinput.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger --action=change /sys/devices/virtual/misc/uinput
```

The rule uses `uaccess` for `/dev/uinput`; it does not make the device world
writable. This permits the active user to create virtual input devices, so it
is a desktop-session permission rather than a hardware-only permission. It
does not change ownership or permissions of physical keyboard nodes. The
installer reports when this one-time rule is missing.

## Validation

Automated tests use synthetic events/frames and private D-Bus services only.
They cover capability rejection, both rotation directions and high-resolution
deduplication, release-only clicks and keymap restoration, workspace filtering,
stable cycling and wraparound, activation guards, feedback restoration, and
the lighting-off override. Private D-Bus tests cover native click/rotate/confirm,
native dismissal, dial disconnect, shutdown, service errors and missing adapter
without injecting keyboard events or opening a GUI. Node tests use fake native
popups/timers to cover eight-second cancellation, activity reset, foreign/stale
tokens and owner disappearance. Neither suite touches real input devices.

Physical acceptance must separately check clockwise/counterclockwise cycling
with several windows on one workspace, click/rotate/click across workspaces,
Escape/idle cancellation, green/orange/white visibility, the right volume
roller, and controls with lighting off. No camera is used.
