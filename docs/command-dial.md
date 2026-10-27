# Command Dial window switching

SIE 0.12.0 supports the top-left Command Dial beside Escape on the full-size
Razer BlackWidow V4 Pro (`1532:028d`, OpenRazer matrix 8 × 23).

| Action | Behavior | Dial color |
| --- | --- | --- |
| Rotate without opening the picker | Activate the next/previous eligible window on the current workspace | Green during activation, with a short 150 ms visible completion indication |
| Press | Open the window picker across all existing workspaces | Orange |
| Rotate with the picker open | Move selection without activating the target | Orange |
| Press again or Enter | Activate the selected existing window and its workspace | White |
| Escape, close, or focus another window | Cancel the picker | White |

Idle is white. Lighting off takes precedence and keeps the dial black while
controls remain usable. Recording rainbow, finger zones, workspace and media
indicators share the existing single Rust renderer and retain their behavior.
Quit, disconnected dial input and picker errors clear switching feedback.

“Applications” here means concrete normal/dialog windows, so separate windows
of the same application are selectable. The picker shows the application,
window title and workspace number starting at one. Desktop/dock/menu windows
and the picker itself are excluded. At most 128 eligible windows are shown.
Local cycling uses stable window-ID order rather than focus order, avoiding
bouncing between the two most recently used windows. A local target must still
exist on the original current workspace when activated. No workspace is created.

## Existing desktop integration

Rust owns discovery, filtering, selection, activation and feedback. Window Calls
provides `List` and `Activate` on `org.gnome.Shell.Extensions.Windows`; the
installed Olbrasoft tracker provides `CurrentWorkspace`. A small GTK 3 view
embedded in the binary displays the global picker. It requires Python 3,
PyGObject and GTK 3, all already installed on the development workstation.
There is no additional Shell extension or background service and no GTK build
dependency. The picker closes if focus moves elsewhere. Selection command
revisions prevent delayed UI echoes from undoing rapid dial turns.

SIE does not hold virtual Alt or another modifier, change GNOME keybindings,
create windows for the chosen application, or inspect ordinary typing. A
window closed during selection fails safely instead of launching a replacement.
Application-specific browser tabs are not separate window-switcher entries.

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
the lighting-off override. They do not open real input devices or a GUI.

Physical acceptance must separately check clockwise/counterclockwise cycling
with several windows on one workspace, click/rotate/click across workspaces,
Escape/focus-loss cancellation, green/orange/white visibility, the right volume
roller, and controls with lighting off. No camera is used.
