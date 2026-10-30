# Spoken agent notifications

SIE 0.16.0 owns the local notification API, durable pending queue, TTS provider
selection and PipeWire playback. VirtualAssistant and its .NET process are not
required at runtime. Existing Codex, Claude Code and OpenCode callers can keep
posting to `http://localhost:5055/api/notifications`; the Codex question observer
also keeps its separate clickable desktop banners.

Notifications play sequentially across all agents and workspaces. The source
selects the existing voice profile: Codex uses Google Enceladus, Claude uses Google
Charon, OpenCode uses Edge Antonin and Gemini uses Edge Vlasta on this workstation.
A preferred provider is tried first, followed by the configured provider order:
Google Cloud with multiple keys, Azure, independent Edge TTS, VoiceRSS, Google
TTS and local Piper. Each fallback uses its own provider-compatible voice.

## Queue and recording

Accepted notifications are committed to `notification_queue` in the independent
SIE PostgreSQL database before HTTP acknowledgement. The default capacity is 256
pending items. Inputs are limited to 4,000 UTF-16 characters, 20 positive issue
references and a 32 KiB HTTP body. Full/unavailable queues return HTTP 503 rather
than falsely acknowledging a lost message.

One consumer owns playback. FIFO ordering includes retries: a failed head item
waits with bounded exponential backoff rather than being overtaken. Database
connections reconnect and consumer ownership is reacquired before work resumes.
Successful playback deletes the notification body and its issue/model metadata.
No old VirtualAssistant notification or transcription history is imported.
Existing SIE dictation history and correction rules remain unchanged.

Recording directly pauses notification speech inside the Rust process before
microphone capture starts. An interrupted notification remains pending; generated
audio can be reused in memory, and playback restarts after dictation completes or
is canceled. External speech-lock clients have an independent bounded lease and
cannot unlock an active SIE recording. Mute pauses the queue; Stop discards only
the current notification. These controls appear in the SIE tray.

Delivery is durable **at least once**. If the application or computer stops after
audio finishes but before the deletion commits, the last item can repeat on the
next start. A notification interrupted by recording restarts from the beginning.
There is no permanent audio cache or played-message archive. Private temporary
audio/text files for CLI providers are deleted after use; process output, network
response sizes and execution time are bounded.

## Voices, quotas and credentials

Configuration stores only references such as `google/primary`. The desktop
keyring holds the actual API keys under attributes `application=sie-tts` and
`credential=<reference>`. Runtime retrieval uses `secret-tool`; credentials are
never put in Git, configuration JSON/TOML, PostgreSQL, child arguments or request
URLs. Google/Azure keys use sensitive HTTP headers; VoiceRSS uses a POST body.
HTTP redirects are disabled. Logs omit message text, provider response bodies,
credentials and request URLs.

Google keys rotate. Monthly character reservations and quota/rate cooldowns are
persisted separately from the notification queue. Month boundaries use UTC;
failed/uncertain requests conservatively retain their character reservation, so
the local cap cannot be bypassed by retries or restarts. Current-month usage is
imported without importing old notification bodies. Expired-month source counters
cannot overwrite a new month's SIE usage. A bad credential can be disabled;
transient provider failures open an exponential circuit breaker. Piper remains
an always-retryable local fallback. Prosody preserves the previous Google rate
mapping, including `+10%` becoming a 1.3 pace; the source voice remains unchanged.
See Google's [Chirp pace controls](https://docs.cloud.google.com/text-to-speech/docs/chirp3-hd#pace-control)
and Microsoft's [speech REST interface](https://learn.microsoft.com/azure/ai-services/speech-service/rest-text-to-speech).

## One-time migration

Use the installed desktop keyring, PostgreSQL, `ffmpeg`, `pw-play`, `edge-tts`,
`gtts-cli` and `piper` with its local Czech model. The import tool additionally
uses the already installed .NET 10 SDK/SecureStore package and Python psycopg2 to
read the existing encrypted vault. Neither .NET nor psycopg2 is a SIE runtime
dependency.

```sh
python3 scripts/import-notifications.py          # inspect counts only
python3 scripts/import-notifications.py --apply  # settings, keyring, current quotas
```

This installs a mode-0600 `notifications.json` beside the SIE config. Existing
settings are backed up before replacement. The vault and its key stay untouched;
only selected TTS secrets are copied into the desktop keyring. The importer
changes neither `config.toml` nor running services. A generic non-secret example
is available in [notifications.example.json](../packaging/notifications.example.json).

When ready to hand over the port, finish active dictation and let the old speech
queue drain. Back up the SIE binary/config and the current service preferences.
Stop VirtualAssistant, enable the following SIE settings and restart `sie.service`:

```toml
[notifications]
enabled = true
listen = "127.0.0.1:5055"
max_pending = 256
# settings_path defaults to notifications.json beside the normal SIE config.
```

The database URL is reused from `[dictation]`; dictation itself need not be
enabled. With native notifications enabled, recording uses direct coordination
and never calls VirtualAssistant. Set the legacy
`dictation.virtual_assistant_speech_lock` option to false for standalone use.
The chosen notification sink is explicit and independent of default audio
devices. This workstation uses the ViewSonic monitor sink copied by the importer.

Update the existing question observer from `scripts/codex-question-notify.py`,
its service ordering to `After=sie.service`, and the existing Codex notify hook
from `scripts/codex-notify.sh`. `scripts/install.sh` installs these scripts when
they already exist. `SIE_NOTIFICATION_API_URL` optionally overrides the complete
notification endpoint; the legacy `VIRTUAL_ASSISTANT_URL` base override remains
compatible. Existing running MCP/OpenCode clients using localhost:5055 need no
restart. SIE's login-start preference is not changed by the installer.

Useful local endpoints:

```sh
curl --fail http://localhost:5055/health
curl --fail http://localhost:5055/api/tts/status
curl --fail http://localhost:5055/api/tts/keys-usage
curl --fail -X POST http://localhost:5055/api/notifications \
  -H 'Content-Type: application/json' \
  -d '{"text":"Hlasové oznámení přehrává SIE.","source":"codex"}'
curl --fail -X POST http://localhost:5055/api/tts/stop
```

The API binds only to loopback, rejects browser Origin headers and does not
expose message contents through status endpoints. Compatible speech-lock
start/stop/status and TTS mute/flush-queue endpoints support existing local
callers. Flush wakes processing without bypassing a recording or mute gate.

For rollback, stop SIE, restore the saved config/binary/observer files, restore
the original service preferences and start VirtualAssistant again. Never run
both APIs on the same port. Pending SIE rows remain available for the next SIE
start, while successful notifications leave no played history.

## Verification

Ordinary Rust tests use no real devices, keyring, provider credentials or audio.
Private PostgreSQL/loopback tests are opt-in through `SIE_TEST_DB` and refuse
database names outside `test_sie_*`. They cover concurrent admission, capacity,
FIFO deletion, quota persistence, recording interruption, serial playback,
restart recovery and fallback through stub executables with blocked network.
Python tests cover non-secret settings import and quota-state preservation.

Physical acceptance separately checks the same Codex voice, monitor routing,
two sequential notifications, recording interruption/resumption and working
dictation while VirtualAssistant is inactive. No camera is used.
