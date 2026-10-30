# Codex F8 audio devices

Codex CLI 0.166.0 supports separate microphone and speaker selections in
`/voice settings` → Set sound devices. This is separate from SIE configuration.
Selections apply to the next voice conversation. The official
[Codex changelog](https://learn.chatgpt.com/docs/changelog) documents the picker;
the [configuration schema](https://developers.openai.com/codex/config-schema.json)
defines the root `[audio]` settings.

On this workstation, Codex's ALSA backend needs named PulseAudio PCM connections
to use the same shared PipeWire nodes as SIE. Direct `hw` connections can contend
with the running audio server. The installed ALSA PulseAudio plugin already
supports a `device` setting for each connection; no extra service is needed.

The [example ALSA fragment](../packaging/codex-voice.asoundrc.example) defines two
connections for this workstation. Merge it into `~/.asoundrc`, preserving any
existing configuration. It neither overrides `pcm.!default` nor changes system
input/output defaults. The stable node names select the Logitech StreamCam
microphone and ViewSonic HDMI monitor speakers. Adjust them on other machines.

In Codex's native picker choose **SIE Logitech StreamCam microphone** and
**SIE ViewSonic monitor speakers**. Codex stores these display names, not the ALSA
connection identifiers:

```toml
[audio]
microphone = "SIE Logitech StreamCam microphone"
speaker = "SIE ViewSonic monitor speakers"
```

Settings are local to `~/.codex/config.toml`, with no provider keys involved.
Already-running Codex interfaces may retain their previous selection; select
these devices in that interface's picker or reopen the interface before the next
F8 conversation. Do not restart another active conversation automatically.

Verification separates device enumeration and routing metadata from human audio
acceptance. Inspect PipeWire/PulseAudio links during the user's F8 conversation;
confirm input comes from StreamCam and output reaches ViewSonic. Enumeration
itself does not capture microphone audio. Ordinary media continues on Bose.
