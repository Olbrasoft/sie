# Dictation sound assets

The original MP3 files were copied without modification from
[Olbrasoft/VirtualAssistant](https://github.com/Olbrasoft/VirtualAssistant/tree/main/assets/sounds)
under that repository's MIT license; see LICENSE.VirtualAssistant.

- recording-start.mp3: original successful recording-start cue, retained as source.
- recording-start.wav: decoded recording cue with 280 ms of leading near-silence
  removed for prompt feedback; used by SIE since 0.9.2.
- write.mp3: loops while transcribing, stops on completion/error/cancel.
- paper-rip.mp3: cancellation of an active recording or transcription.

The playback bytes are embedded in the Rust binary. PipeWire's `pw-play` plays
them from stdin with a 20 ms latency request; a separate VirtualAssistant
installation is not required. The recording cue no longer needs MP3 decoding at
playback time. The other cues remain MP3.

To reproduce the WAV from the original using FFmpeg and Python's standard library:

```python
import subprocess
import wave

pcm = subprocess.check_output([
    "ffmpeg", "-v", "error", "-i", "assets/sounds/recording-start.mp3",
    "-f", "s16le", "-acodec", "pcm_s16le", "-",
])
with wave.open("assets/sounds/recording-start.wav", "wb") as output:
    output.setnchannels(2)
    output.setsampwidth(2)
    output.setframerate(24000)
    output.writeframes(pcm[6720 * 2 * 2:])
```

The cut removes only the initial quiet section; the retained PCM samples are
unchanged. At a -50 dB threshold, the first audible frame moves from 289.7 ms to
9.7 ms. An isolated Rust test guards the onset and duration of the embedded WAV.
