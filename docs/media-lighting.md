# Global media playback indicator

SIE 0.5.0 colors the BlackWidow V4 Pro's second upper-right media button,
Play/Pause, according to the player controlled by GNOME's native media keys:

| State | Play/Pause color |
| --- | --- |
| Playing and pausable | Green `#00FF00` |
| Paused/stopped and able to play/pause | Red `#FF0000` |
| No target, unavailable state or unsupported controls | White `#FFFFFF` |
| Keyboard lighting switched off | Black, regardless of media state |

This is global: a paused video on workspace 5 remains a red target while the user
dictates into a terminal on workspace 1. Starting/pausing through Space, the
mouse, another controller or the hardware button changes the same indicator.
SIE does not recolor Space, create a new media binding, inject playback input or
send any playback methods. Focusing another application does not clear a target
that the hardware button still controls.

## Existing interfaces

Players expose `PlaybackStatus`, `CanControl`, `CanPlay` and `CanPause` through
[MPRIS](https://specifications.freedesktop.org/mpris/latest/Player_Interface.html).
The application subscribes to property/name-owner changes before discovery,
reads the initial state and uses a two-second recovery refresh. Slow or vanished
players cannot block the tray; unknown state fails to white. No new package,
Shell extension, media service or player plugin is required on the workstation.
Players need to expose MPRIS; Chrome's media session and VLC support it here.

Target ordering follows the behavior in
[GNOME 48's media controller](https://github.com/GNOME/gnome-settings-daemon/blob/gnome-48/plugins/media-keys/mpris-controller.c):
a playing current target is retained; a new player can replace a paused target;
another player starting playback can replace a nonplaying target; removing a
target falls back to the remembered player order. Focus is not a selection rule.
These policies are independently implemented in Rust.

An optional dedicated D-Bus monitor also observes only `PlayPause` method calls
from `org.gnome.SettingsDaemon.MediaKeys` to the MPRIS player interface/path. This
reveals the actual native destination and synchronizes the indicator, without
intercepting or duplicating the call. It receives no ordinary keystrokes. Its
connection is separate from SIE's normal bus connection and sends no calls after
becoming a monitor. If monitoring permission is unavailable, property tracking
and target ordering continue to work.

GNOME does not expose its already-selected target as a public property. When
several players predate SIE, their connection order and playing state initialize
the estimate; the next observed native Play/Pause call synchronizes it exactly.
A single existing player is correct immediately, including when already playing.
Custom media-key handlers that bypass GNOME/MPRIS are outside this adapter.

## Lighting integration

The installed native device graphic `blackwidow_v4_pro_en_US.svg` identifies the
four media LEDs as columns 19–22 on row 0. Play/Pause is `(row 0, column 20)`.
The overlay changes only that RGB cell after the finger zones and workspace LED
have been generated. The first, third and fourth media buttons keep their prior
colors. The complete-black lighting mode takes precedence over every overlay;
re-enabling reads the current media/workspace state.

The main loop updates lighting immediately when the media status watch changes,
with the existing periodic refresh as recovery. The Czech tray status displays
the selected player and whether it is playing or paused/stopped. Media titles
and URLs are not logged.

## Validation

Isolated tests cover capabilities, unknown/stopped states, initial playing state,
GNOME target retention/promotion/fallback, target observations, background
property changes, closing players, and the no-control-call invariant. Private
D-Bus tests use fake players; the optional observer test confirms that a native
call is observed once and other senders are excluded. Matrix tests assert all
cells, including the off-state override and unchanged workspace/finger colors.

The local suite passes with 38 unit/private-tmux tests and 15 private D-Bus tests
(12 desktop/device plus three media tests). Physical YouTube/VLC and keyboard
checks are recorded separately in the issue and PR validation notes. Camera
verification was unavailable because the camera no longer faced the keyboard;
no camera image was retained.
