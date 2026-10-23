use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub dictation: DictationConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DictationConfig {
    pub enabled: bool,
    pub model_path: PathBuf,
    pub language: String,
    pub database_url: String,
    pub audio_source: Option<String>,
    pub max_duration_seconds: u64,
    pub use_gpu: bool,
    pub virtual_assistant_speech_lock: bool,
    pub sound_effects: bool,
    pub sound_sink: Option<String>,
}

impl Default for DictationConfig {
    fn default() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        Self {
            enabled: false,
            model_path: home.join(".local/share/whisper-models/ggml-large-v3-turbo.bin"),
            language: "cs".into(),
            database_url: format!(
                "host=/var/run/postgresql dbname=sie user={}",
                std::env::var("USER").unwrap_or_else(|_| "sie".into())
            ),
            audio_source: None,
            max_duration_seconds: 300,
            use_gpu: true,
            virtual_assistant_speech_lock: false,
            sound_effects: true,
            sound_sink: None,
        }
    }
}

impl AppConfig {
    pub fn path() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            })
            .join("sie/config.toml")
    }

    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = path.unwrap_or_else(Self::path);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).context("Cannot read SIE configuration")?;
        let config: Self =
            toml::from_str(&text).map_err(|_| anyhow::anyhow!("Invalid SIE configuration"))?;
        ensure!(
            (1..=600).contains(&config.dictation.max_duration_seconds),
            "Recording duration must be 1–600 seconds"
        );
        ensure!(
            !config.dictation.language.is_empty(),
            "Dictation language is required"
        );
        Ok(config)
    }
}
