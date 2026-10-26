//! Explicit local-file benchmark. Never records, pastes, or connects to the desktop/database.
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde::Serialize;
use sie::{
    config::DictationConfig,
    speech::{SpeechEngine, read_wav},
};

#[derive(Parser)]
#[command(about = "Compare local CUDA transcription with Flash Attention off/on/on/off")]
struct Args {
    /// Installed GGML model, used without modification.
    #[arg(long)]
    model: PathBuf,
    #[arg(long, default_value = "cs")]
    language: String,
    /// Warm measured calls per sample per block (two blocks per setting).
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..=100))]
    repetitions: u32,
    /// JSON report includes the supplied samples' transcripts; stdout if omitted.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Explicitly selected 16 kHz mono signed 16-bit WAV files.
    #[arg(required = true)]
    wav: Vec<PathBuf>,
}

struct Sample {
    path: PathBuf,
    pcm: Vec<i16>,
}

#[derive(Serialize)]
struct Measurement {
    block: usize,
    flash_attention: bool,
    phase: &'static str,
    sample: PathBuf,
    audio_seconds: f64,
    elapsed_seconds: f64,
    text: String,
}

#[derive(Serialize)]
struct Summary {
    sample: PathBuf,
    flash_attention: bool,
    measured_calls: usize,
    median_seconds: f64,
    min_seconds: f64,
    max_seconds: f64,
    distinct_transcripts: Vec<String>,
}

#[derive(Serialize)]
struct Report {
    sie_version: &'static str,
    model: PathBuf,
    language: String,
    beam_size: u32,
    repetitions_per_block: u32,
    block_order: [bool; 4],
    summary: Vec<Summary>,
    measurements: Vec<Measurement>,
}

fn measure(
    engine: &SpeechEngine,
    sample: &Sample,
    block: usize,
    flash_attention: bool,
    phase: &'static str,
) -> Result<Measurement> {
    let start = Instant::now();
    let text = engine.transcribe(&sample.pcm, Arc::new(AtomicBool::new(false)))?;
    let elapsed_seconds = start.elapsed().as_secs_f64();
    Ok(Measurement {
        block,
        flash_attention,
        phase,
        sample: sample.path.clone(),
        audio_seconds: sample.pcm.len() as f64 / 16000.0,
        elapsed_seconds,
        text,
    })
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        cfg!(feature = "cuda"),
        "Build this example with --features cuda"
    );
    ensure!(!args.language.is_empty(), "Language must not be empty");
    let samples = args
        .wav
        .iter()
        .map(|path| {
            let pcm = read_wav(path).with_context(|| format!("Cannot read {}", path.display()))?;
            ensure!(
                (1600..=16000 * 600).contains(&pcm.len()),
                "Benchmark WAV duration must be 0.1–600 seconds"
            );
            Ok(Sample {
                path: path.clone(),
                pcm,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let block_order = [false, true, true, false];
    let mut measurements = Vec::new();
    for (block, flash_attention) in block_order.into_iter().enumerate() {
        eprintln!(
            "Block {} of 4: flash_attention={flash_attention}",
            block + 1
        );
        // Only one model context is retained at a time, including on 8 GB GPUs.
        let engine = SpeechEngine::new(DictationConfig {
            model_path: args.model.clone(),
            language: args.language.clone(),
            use_gpu: true,
            flash_attention,
            ..DictationConfig::default()
        });
        // First call includes model initialization AND inference; exclude it from medians.
        measurements.push(measure(
            &engine,
            &samples[0],
            block,
            flash_attention,
            "first_call_with_model_initialization",
        )?);
        // Warm every sample shape before timing it, even when the first sample already ran.
        for sample in &samples {
            measurements.push(measure(&engine, sample, block, flash_attention, "warmup")?);
        }
        for _ in 0..args.repetitions {
            for sample in &samples {
                measurements.push(measure(
                    &engine,
                    sample,
                    block,
                    flash_attention,
                    "measured",
                )?);
            }
        }
    }
    let mut summary = Vec::new();
    for sample in &samples {
        for flash_attention in [false, true] {
            let selected: Vec<_> = measurements
                .iter()
                .filter(|m| {
                    m.sample == sample.path
                        && m.flash_attention == flash_attention
                        && m.phase == "measured"
                })
                .collect();
            let mut times: Vec<_> = selected.iter().map(|m| m.elapsed_seconds).collect();
            times.sort_by(f64::total_cmp);
            let median_seconds = (times[(times.len() - 1) / 2] + times[times.len() / 2]) / 2.0;
            let mut distinct_transcripts: Vec<_> =
                selected.iter().map(|m| m.text.clone()).collect();
            distinct_transcripts.sort();
            distinct_transcripts.dedup();
            summary.push(Summary {
                sample: sample.path.clone(),
                flash_attention,
                measured_calls: times.len(),
                median_seconds,
                min_seconds: times[0],
                max_seconds: times[times.len() - 1],
                distinct_transcripts,
            });
        }
    }
    let json = serde_json::to_string_pretty(&Report {
        sie_version: sie::VERSION,
        model: args.model,
        language: args.language,
        beam_size: 5,
        repetitions_per_block: args.repetitions,
        block_order,
        summary,
        measurements,
    })?;
    if let Some(path) = args.output {
        // Do not silently overwrite a previous comparison or arbitrary user file.
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?
            .write_all(json.as_bytes())?;
    } else {
        println!("{json}");
    }
    Ok(())
}
