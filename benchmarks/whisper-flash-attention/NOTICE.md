# Benchmark data attribution

The original texts in `samples/` and the benchmark code are part of SIE under
the repository MIT license. Synthetic audio was generated locally using the
installed Piper `cs_CZ-jirka-medium` model; no voice model or audio is redistributed.

`fleurs-manifest.json` contains reference excerpts from **FLEURS** by Google,
https://huggingface.co/datasets/google/fleurs, revision
`70bb2e84b976b7e960aa89f1c648e09c59f894dd`, Czech (`cs_cz`) validation split,
first 20 dataset-viewer rows. This material is licensed under
[Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/).
The normalized and raw references retain the dataset spelling.

`fleurs-results.json` includes machine-generated transcriptions of those clips;
`fleurs-quality.json` includes derived measurements. Audio was converted to
16 kHz mono signed 16-bit PCM for local evaluation and is not redistributed.
The dataset-derived excerpts and transcriptions remain attributed to FLEURS
under CC BY 4.0, rather than being relicensed as repository-owned MIT prose.

Dataset citation: Conneau et al., *FLEURS: Few-shot Learning Evaluation of Universal
Representations of Speech*, 2022, https://arxiv.org/abs/2205.12446.
