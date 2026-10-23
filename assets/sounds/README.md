# Dictation sound assets

Copied without modification from
[Olbrasoft/VirtualAssistant](https://github.com/Olbrasoft/VirtualAssistant/tree/main/assets/sounds)
under that repository's MIT license; see LICENSE.VirtualAssistant.

- recording-start.mp3: successful recording start.
- write.mp3: loops while transcribing, stops on completion/error/cancel.
- paper-rip.mp3: cancellation of an active recording or transcription.

The bytes are embedded in the Rust binary. PipeWire's `pw-play` decodes and plays
them from stdin; a separate VirtualAssistant installation is not required.
