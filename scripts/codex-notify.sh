#!/usr/bin/env bash
# Codex CLI notify hook → SIE spoken notifications.
# Codex invokes this script at agent-turn-complete with a single JSON arg:
#   { "type": "agent-turn-complete", "thread-id": "...", "turn-id": "...",
#     "cwd": "...", "input-messages": [...], "last-assistant-message": "..." }
# We POST only the final spoken-summary paragraph to the SIE REST API; SIE picks
# a voice for source=codex.

set -u

API_URL="${SIE_NOTIFICATION_API_URL:-${VIRTUAL_ASSISTANT_URL:-http://localhost:5055}/api/notifications}"
PAYLOAD="${1:-}"

[ -z "$PAYLOAD" ] && exit 0

EVENT_TYPE=$(printf '%s' "$PAYLOAD" | jq -r '.type // empty' 2>/dev/null)
[ "$EVENT_TYPE" != "agent-turn-complete" ] && exit 0

SESSION_UUID=$(printf '%s' "$PAYLOAD" | jq -r '."thread-id" // empty' 2>/dev/null)
CODEX_STATE_DIR="${CODEX_HOME:-$HOME/.codex}"

# Temporary background turns can invoke the same global hook as an interactive
# conversation. A known registry with no matching thread is positive evidence
# that this completion has no persisted user conversation. Do not speak it or
# let it replace the PID index. Missing/older registries retain legacy behavior.
THREAD_VISIBILITY=$(timeout --kill-after=0.1s 1s python3 - "$CODEX_STATE_DIR" "$SESSION_UUID" 2>/dev/null <<'PY'
from pathlib import Path
import sqlite3
import sys

database = Path(sys.argv[1]) / "state_5.sqlite"
try:
    connection = sqlite3.connect(database.absolute().as_uri() + "?mode=ro", uri=True, timeout=0.15)
    try:
        row = connection.execute("SELECT 1 FROM threads WHERE id = ?", (sys.argv[2],)).fetchone()
    finally:
        connection.close()
    print("persisted" if row else "nonpersisted")
except (OSError, sqlite3.Error):
    print("unavailable")
PY
)
if [ "$THREAD_VISIBILITY" = "nonpersisted" ]; then
    logger -t sie-codex-notify -- 'Skipped completion without a persisted Codex conversation' 2>/dev/null || true
    exit 0
fi

# --- pid-index maintenance (ghnotify tier-1 routing) -----------------
#
# Refresh ~/.codex/sessions/pids/<pid>.json so that ghnotify can map
# this session's UUID → tmux pane by walking /proc from the Codex pid
# (parity with how it handles Claude via ~/.claude/sessions/<pid>.json).
# `$PPID` here is the Codex CLI process that invoked this notify script.
# Codex's notify payload exposes the session UUID as `.thread-id` and
# the session cwd as `.cwd`. Write best-effort — a failure in this
# block must not stop the spoken-notification path below, so all errors
# are squelched and we only proceed when every required field is
# present and the UUID has the expected shape.
PID_DIR="$CODEX_STATE_DIR/sessions/pids"
mkdir -p "$PID_DIR" 2>/dev/null
CODEX_CWD=$(printf '%s' "$PAYLOAD" | jq -r '.cwd // empty' 2>/dev/null)
if [[ "$SESSION_UUID" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]] \
    && [ -n "$CODEX_CWD" ] && [ -n "$PPID" ]; then
    jq -n \
        --argjson pid "$PPID" \
        --arg sessionId "$SESSION_UUID" \
        --arg cwd "$CODEX_CWD" \
        '{pid: $pid, sessionId: $sessionId, cwd: $cwd}' \
        > "$PID_DIR/$PPID.json" 2>/dev/null || true
fi

FULL_TEXT=$(printf '%s' "$PAYLOAD" | jq -r '."last-assistant-message" // empty' 2>/dev/null)
[ -z "$FULL_TEXT" ] && exit 0

# Codex passes the whole final answer to notify. The user only wants the last
# short Czech paragraph read aloud, so extract the final non-empty paragraph and
# strip common Markdown that TTS would otherwise verbalize awkwardly.
TEXT=$(printf '%s' "$FULL_TEXT" | perl -0ne '
    s/\r//g;
    s/[ \t]+\n/\n/g;
    s/^\s+|\s+$//g;
    my @paragraphs = grep { /\S/ } split(/\n\s*\n/);
    my $text = @paragraphs ? $paragraphs[-1] : "";
    $text =~ s/^\s*>\s?//mg;
    $text =~ s/\[([^\]]+)\]\([^)]+\)/$1/g;
    $text =~ s/`([^`]+)`/$1/g;
    $text =~ s/\*\*([^*]+)\*\*/$1/g;
    $text =~ s/\s+/ /g;
    $text =~ s/^\s+|\s+$//g;
    print $text;
')
[ -z "$TEXT" ] && exit 0

# Keep the spoken notification short even if the final paragraph is malformed.
if [ "${#TEXT}" -gt 350 ]; then
    TEXT="${TEXT:0:347}..."
fi

BODY=$(jq -n \
    --arg text "$TEXT" \
    --arg source "codex" \
    --arg providerName "openai" \
    --arg modelName "codex" \
    '{text: $text, source: $source, providerName: $providerName, modelName: $modelName}')

curl -sS -X POST "$API_URL" \
    -H 'Content-Type: application/json' \
    --max-time 5 \
    -d "$BODY" >/dev/null 2>&1 || true

exit 0
