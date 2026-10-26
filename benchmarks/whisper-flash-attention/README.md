# Whisper Flash Attention comparison, 2026-10-26

Flash Attention reduced warmed transcription time on this workstation. It is
available as `flash_attention = true` in `[dictation]` since SIE 0.8.1; the
portable default remains false. This is a CUDA application setting. No GPU
unlock module, driver update, firmware flash or boot configuration was used.

## Environment and method

- GeForce RTX 3060 **8 GB**, GA106, PCI `10de:2544`, compute capability 8.6.
- Debian 13, kernel `6.12.111+deb13-amd64`, NVIDIA `610.57.04`, CUDA 13.1.
- Release build, CUDA architecture 86; whisper-rs 0.16.0 / whisper-rs-sys 0.15.0.
- Existing `ggml-large-v3-turbo.bin`, FP16, SHA-256
  `1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69`.
- Production `SpeechEngine`, Czech language, beam search size 5, temperature 0,
  no context carry-over. Only the Flash Attention flag differs.
- Four blocks: off, on, on, off. Each creates a fresh model context, measures a
  first call including model initialization **and** inference, warms every
  sample shape, then measures complete synchronous transcription calls.
  First calls and warm-ups are excluded from medians. This is not a measurement
  of model loading alone, a cold filesystem cache, or a fresh CUDA process.
- Only one benchmark model is retained at a time. Existing desktop, SIE and
  VirtualAssistant processes remained running with their models resident.
  Background GPU activity was not controlled; these are workstation measurements,
  not isolated GPU microbenchmarks or end-to-end microphone-to-paste latency.

The synthetic comparison ran the feature working tree based on `173f0a6` before
the version bump (its JSON therefore says 0.8.0). The natural-speech comparison
and cancellation regression ran 0.8.1 with the same speech implementation.

## Synthetic Czech speech

The three repository-owned [texts](samples/) were synthesized locally with
piper-tts 1.8.0 and the installed `cs_CZ-jirka-medium` voice, default CPU synthesis,
then converted to 16 kHz mono signed 16-bit WAV. No microphone or private
dictation history was used. The manifest records model and WAV hashes; Piper
synthesis is not guaranteed to produce bit-identical WAV files on another run.

Each median contains six measured calls per sample and setting:

| Sample | Audio duration | Off | On | Less time |
| --- | ---: | ---: | ---: | ---: |
| short | 13.98 s | 0.742 s | 0.502 s | 32.4% |
| medium | 47.90 s | 1.789 s | 1.282 s | 28.3% |
| long | 98.85 s | 3.731 s | 2.705 s | 27.5% |
| Sum of sample medians | | 6.262 s | 4.489 s | 28.3% |

Outputs were stable within each setting but not always identical between
settings. The short sample changed `kůň` to `kun`; the long sample changed
`vy v` to `výv` (both incorrect for the reference `vliv`). The medium transcript
was identical. With Unicode case folding and punctuation removed, retaining
diacritics, word error rate was 3.571% for both settings (9 edits per 252 reference
words). Character error rate changed from 0.677% to 0.790%. This small synthetic
set does **not** establish unchanged accuracy.

Artifacts: [manifest](synthetic-manifest.json), [all timings and transcripts](
synthetic-results.json), [quality calculation](synthetic-quality.json).

## Natural Czech speech

An additional check used the first 20 rows of the public Google FLEURS Czech
validation split, without selection based on recognition results. The dataset
revision, original filenames, reference transcripts and converted WAV hashes
are in [the manifest](fleurs-manifest.json). The selection contains male speech
only and is a small validation set, not a representative accuracy benchmark.
The source is [Google FLEURS](https://huggingface.co/datasets/google/fleurs),
licensed CC BY 4.0; see [attribution](NOTICE.md).

Each sample and setting has two measured calls, from separate model contexts:

| Metric | Off | On |
| --- | ---: | ---: |
| Sum of the 20 sample medians | 12.810 s | 7.929 s |
| Word error rate, diacritics retained | 11.892% | 11.892% |
| Character error rate, diacritics retained | 2.765% | 2.765% |

The sum of medians decreased by **38.1%**. The 370 unique reference words are
evaluated twice per setting: 88 word edits / 740 evaluated words in each arm.
This rate includes ordinary transcription errors and reference-formatting
differences, such as written versus spoken numbers; numbers are not rewritten
by the scoring script. Exact text equality and error rate are separate checks.
Two sample transcripts changed: `braly` became `brali` in row 10, while a different
word-boundary error changed in row 15. The equal aggregate error rates do not mean
that each utterance was equally accurate or that outputs are bit-identical.

Artifacts: [all timings and transcripts](fleurs-results.json), [quality
calculation](fleurs-quality.json). None of these samples are the user's speech.

## Reproduction

Prepare any explicitly selected 16 kHz mono signed 16-bit WAV files. The example
never captures audio, connects to the desktop/database, or inserts text. Its
JSON deliberately includes full transcripts of the supplied files.

```sh
CUDACXX=/usr/local/cuda/bin/nvcc CUDA_PATH=/usr/local/cuda \
CMAKE_CUDA_ARCHITECTURES=86 \
  cargo build --locked --release --features cuda --example speech_benchmark

target/release/examples/speech_benchmark \
  --model /absolute/path/ggml-large-v3-turbo.bin \
  --repetitions 3 --output /new/path/results.json \
  /absolute/path/short.wav /absolute/path/medium.wav /absolute/path/long.wav
```

Both file outputs refuse to overwrite an existing file. Synthetic WAV preparation
uses the installed Piper voice (no model download or audio playback):

```sh
mkdir -p /tmp/sie-benchmark-audio
for text in benchmarks/whisper-flash-attention/samples/*.txt; do
  name=$(basename "$text" .txt)
  piper --model /absolute/path/cs_CZ-jirka-medium.onnx \
    --output_file "/tmp/sie-benchmark-audio/$name-source.wav" < "$text"
  ffmpeg -i "/tmp/sie-benchmark-audio/$name-source.wav" -ar 16000 -ac 1 \
    -c:a pcm_s16le "/tmp/sie-benchmark-audio/$name.wav"
done
```

For FLEURS, use the validation split at the pinned revision and select the exact
20 original filenames in the manifest. Originals are in `data/cs_cz/audio/dev.tar.gz`;
the measurement downloaded only those 20 clips through the dataset viewer API.
Convert each to `fleurs-NNN.wav` with the same ffmpeg arguments, then run the
benchmark with `--repetitions 1` and the files in increasing row-index order.

Recompute quality from either published report:

```sh
python3 scripts/score-speech-benchmark.py \
  benchmarks/whisper-flash-attention/fleurs-results.json \
  benchmarks/whisper-flash-attention/fleurs-manifest.json
```

The scorer uses Levenshtein distance over normalized word tokens and characters,
retaining diacritics and normalized spaces. Every measured call is counted;
it does not select the best transcription. Warm-ups and first calls are excluded.

## Deployment and remaining checks

The measured speed benefit justifies an opt-in workstation trial, with the
synthetic diacritic difference documented rather than hidden. The real-model
regression passed with both flags: initial inference, cancellation during
inference, then three identical successful calls using the same cached model.

Set `flash_attention = true` in `[dictation]` and restart SIE while dictation is
idle. `sie --doctor` reports the effective flag. To roll back, set it to false
and restart. Explicit CPU mode and builds without CUDA disable Flash Attention. The portable default
remains false; no GPU or model is assumed safe solely from this workstation test.

Natural microphone dictation, including terminal delivery, remains a physical
user check after deployment. The opt-in model regression and public recordings
do not substitute for that check.
