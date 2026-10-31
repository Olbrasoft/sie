# Clickable agent questions

SIE 0.14.0 adds desktop actions to the existing local Codex question observer.
When a pending question belongs to an agent terminal on a different workspace,
GNOME shows its text, project name, the workspace number starting at one, and a button to
return to the question. Clicking the banner or its button activates that
existing terminal/workspace and selects the exact tmux session/window/pane.
Showing a notification never changes focus. On the same workspace no banner
is created; the spoken question plays through the local notification API (SIE after migration).

The notification requests persistence in the notification center, is removed
when answered/resolved or when its observer stops, and has a one-hour resource
limit. Pending questions request high urgency so GNOME also shows their banners
during fullscreen playback or Do Not Disturb. GNOME retains control over
lock-screen privacy. Notification sound is suppressed: the existing voice path
continues using the configured ViewSonic output without a second system sound.

## Components and routing

`scripts/codex-question-notify.py` preserves the installed observer's spoken
question formatting, retry logic and SQLite deduplication. Desktop delivery
runs independently and uses only thread identity, thread directory and question
text. Existing app-server question events and native resolution/user-reply
events drive delivery/cancellation; the observer never answers requests or
approves tools. See the [official app-server question lifecycle](https://learn.chatgpt.com/docs/app-server#toolrequestuserinput).
No API key, new remote request or conversation forwarding is added.

One Rust `sie-agent-notify` helper per question owns the desktop notification.
It sends [standard notification actions](https://specifications.freedesktop.org/notification/latest/protocol.html)
over the session bus and listens for its own notification ID. It reuses the
installed Window Calls and Desktop State Tracker GNOME extensions. This feature
requires no new Shell code or logout/login.

Origin discovery reads process identity/ancestry/start times, tmux metadata and
GNOME window metadata. It never reads terminal contents, typed characters or
keystrokes. Live PID/session descriptors are preferred; shared Codex app-server
daemons are excluded because their PID does not identify the originating TUI.
Without an exact descriptor, a unique attached tmux terminal with an agent and
matching absolute directory is accepted. Multiple attached candidates, shared
GUI-process ambiguity, detached sessions or an unknown origin produce no
actionable banner rather than choosing an unrelated terminal. The initial
automatic integration is Codex; other agents can call the helper with their
own session metadata when their native process identity is available.

At click time, process start times and all window/pane/client identities are
checked again. A moved window is followed to its current workspace; a closed
or replaced target is rejected. Existing GNOME window activation and tmux
selection are used. No workspace/window is created and no Enter, terminal
command, answer or clipboard content is sent.

## Install and runtime

`scripts/install.sh` installs `~/.local/bin/sie-agent-notify`. If the local Codex
question observer already exists, it updates that observer in place; restart
its existing service after installation:

```sh
systemctl --user restart codex-question-notify.service
```

The observer needs Python 3 and `websockets` (Debian `python3-websockets`), the
existing local Codex app-server control socket, and the SIE notification API (or the existing VirtualAssistant during migration)
notification URL for speech. Desktop actions additionally require tmux, the
GNOME session bus and native notification action support. Failure of voice
delivery does not prevent the banner, and a missing desktop helper does not
prevent speech. No additional persistent process or service is introduced on
the development workstation; the already-running observer is reused.

For a new machine, copy the observer to `~/.codex/codex-question-notify.py` and
install the example `packaging/codex-question-notify.service` as a user service.
The default socket is `~/.codex/app-server-control/app-server-control.sock`;
existing `CODEX_HOME`, `CODEX_QUESTION_SOCKET` and `VIRTUAL_ASSISTANT_URL`
overrides remain supported. `SIE_AGENT_NOTIFY_BIN` overrides the helper path.

## Helper input

The helper accepts one bounded JSON object on stdin, keeping question text out
of process arguments:

```json
{"agent":"codex","session_id":"session-uuid","cwd":"/absolute/project","text":"Question text"}
```

`sie-agent-notify --resolve-only` reads the same input and prints only target
window/workspace/pane metadata; it never displays a banner or changes focus.
The helper exits quietly when the origin is unavailable, ambiguous or already
on the current workspace. Other failures return a nonzero status. SIGTERM
closes its outstanding notification; the observer owns/cancels this child on
resolution, user reply, thread closure or observer shutdown.

## Validation

Synthetic origin tests cover unique matching, exact PID preference, detached
and ambiguous candidates, shared daemons, malformed metadata and stale process
identities. Notification tests use a private D-Bus server, exercising matching
and unrelated actions, cancellation, cleanup and activation failure without
touching real input or the desktop. Python tests cover voice/desktop independence,
native/async deduplication, per-thread cancellation and owned-child cleanup.

Read-only origin resolution and a real cross-workspace banner/click are recorded
separately as desktop checks. Automated tests do not inspect the user's desktop.

Spoken delivery migrates independently through the compatible API; see [standalone SIE speech](spoken-notifications.md). No desktop banner sound is added.
