# Reveal the controlled media player

Starting with SIE 0.6.1, starting/resuming playback with the BlackWidow V4 Pro
Play/Pause button activates the controlled player's existing window on its existing GNOME
workspace. For example, a press in the terminal on workspace 1 reveals the
browser on workspace 5 and its existing video tab. Playback toggles once through
GNOME's native handler; SIE never sends a second playback command. Pausing a
background video keeps the current workspace, window and browser tab focused.

Only a nonrepeating press that starts/resumes playback requests focus. Space, mouse controls, playback property changes and other media buttons
keep their existing focus behavior. Their playback changes still update the
Play/Pause LED. Turning keyboard lighting off does not disable media controls.

## Existing desktop integration

SIE correlates the physical press with the native GNOME MPRIS destination. If
the optional D-Bus observer is unavailable, it uses the same remembered target
as the playback indicator. It reads the target process, title and URL from
MPRIS and uses the installed Window Calls extension's
`org.gnome.Shell.Extensions.Windows` interface to activate an existing window.
The media watcher preserves each player's previous playback state and the time
of its last transition, including when the state signal arrives just before
the queued input action. The action retains the kernel's physical press time,
so a recent mouse start before a pause press is not mistaken for that press
starting playback. A resume request waits up to 1.2 seconds for actual Playing
confirmation from that same target. Pausing and unknown/unconfirmed states do
not request browser activation or window focus. State tracking and LED updates
continue independently.

The window interface switches to the window's workspace; SIE does not create
workspaces, move windows or assume a fixed workspace number.

Native players can expose MPRIS `Raise`; SIE also resolves and activates their
GNOME window. A browser whose video tab is already visible can be activated
directly. Chrome does not implement `Raise`, and MPRIS cannot select a hidden
browser tab, so that case uses the small local bridge below.

## Browser setup

Run `./scripts/install.sh` first. In Chrome, open `chrome://extensions`, enable
Developer mode, choose **Load unpacked**, and select:

```text
~/.local/share/sie/browser-extension
```

The name is **SIE Media Tab Bridge**, with stable extension ID
`liiidhkomocgfaophldioaikcnneahkb`. Load it in each browser profile where media
tab selection is wanted. The installer registers its Rust native-messaging host
for existing Chrome, Chromium and Microsoft Edge configuration directories.
Additional Chromium variants need the same native-host manifest in their own
configuration directory. Firefox is not supported by this bridge.

The extension needs only `tabs` and `nativeMessaging`. It reads local tab
titles/URLs to match the requested media metadata, returns matching candidates,
and activates an existing tab/window. It has no content scripts or host
permissions, reads no page content, sends no network requests, and never opens,
reloads or navigates a media tab. Normal typing is not observed.

The native host communicates over framed standard input/output and the user's
session D-Bus. Matching and routing run in Rust. Sessions and queues are bounded,
messages are limited to 64 KiB, and requests have timeouts. Multiple profiles
are searched before any tab is activated. An ambiguous match is refused unless
exactly one matching tab is audible. A tab is checked again before activation.
Media titles, URLs and browser responses are not logged or persisted.

The extension reconnects after a native-host disconnect. The host re-registers
after an SIE restart; neither Chrome nor the video needs restarting. After
updating the extension assets, use its Reload button on the extensions page.

## Limits and diagnostics

MPRIS must expose sufficient metadata to identify the media tab. URL matching
takes precedence; without a URL the bridge uses the normalized title and known
site-title suffixes. Multiple identical paused copies cannot always be
distinguished. Closed, changed, unresponsive or ambiguous targets fail without
opening an unrelated tab. An error appears in the SIE tray; a later successful
press clears it. Native playback remains available even when reveal fails.

When several players were already running before SIE started, the next observed
GNOME Play/Pause call provides the exact native destination. Custom handlers
that bypass GNOME/MPRIS remain outside this integration.

Inspect a fresh list of connected browser process IDs with:

```sh
gdbus call --session --dest cz.olbrasoft.SystemInterfaceExtensions \
  --object-path /cz/olbrasoft/SystemInterfaceExtensions/Browser \
  --method org.freedesktop.DBus.Properties.Get \
  cz.olbrasoft.SystemInterfaceExtensions.Browser ConnectedBrowsers
journalctl --user -u sie.service
```

## Validation

Isolated Rust tests cover media-key down/release/repeat filtering, window
selection, existing-window activation, no extra playback calls, native frame
limits/truncation, request/session correlation, and a real spawned Rust native
host talking to a fake browser over pipes and a private D-Bus. Ambiguous browser
matches are rejected before activation. JavaScript tests cover Unicode titles,
URL precedence, candidate discovery without focus changes, existing-tab
activation and stale-tab rejection. These checks never control the real desktop.

Physical keyboard/workspace/tab verification is recorded separately in the
deployment issue and pull request.
