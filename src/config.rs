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
    pub flash_attention: bool,
    pub virtual_assistant_speech_lock: bool,
    pub sound_effects: bool,
    pub sound_sink: Option<String>,
    pub duck_sink: Option<String>,
    pub duck_volume_percent: u8,
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
            flash_attention: false,
            virtual_assistant_speech_lock: false,
            sound_effects: true,
            sound_sink: None,
            duck_sink: None,
            duck_volume_percent: 20,
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
        ensure!(
            config.dictation.duck_volume_percent <= 100,
            "Ducking volume must be 0–100 percent of the original volume"
        );
        if let Some(sink) = &config.dictation.duck_sink {
            ensure!(
                !sink.trim().is_empty() && !sink.starts_with('@') && sink.parse::<u32>().is_err(),
                "Ducking requires an explicit stable output name"
            );
            ensure!(
                config.dictation.sound_sink.as_ref() != Some(sink),
                "Ducking output must differ from the feedback output"
            );
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ducking_is_opt_in_and_requires_a_separate_stable_output_and_valid_percentage() {
        use std::io::Write;
        let defaults = AppConfig::default();
        assert!(defaults.dictation.duck_sink.is_none());
        assert_eq!(defaults.dictation.duck_volume_percent, 20);
        for (settings, valid) in [
            ("duck_sink = 'test.bose'\nduck_volume_percent = 20", true),
            ("duck_volume_percent = 101", false),
            ("duck_sink = ''", false),
            ("duck_sink = '@DEFAULT_SINK@'", false),
            ("duck_sink = '7'", false),
            (
                "duck_sink = 'test.monitor'\nsound_sink = 'test.monitor'",
                false,
            ),
        ] {
            let mut file = tempfile::NamedTempFile::new().unwrap();
            write!(file, "[dictation]\n{settings}\n").unwrap();
            assert_eq!(AppConfig::load(Some(file.path().into())).is_ok(), valid);
        }
    }

    #[test]
    fn existing_configuration_keeps_flash_attention_disabled() {
        let config: AppConfig =
            toml::from_str("[dictation]\nenabled = true\nuse_gpu = true\nlanguage = \"cs\"\n")
                .unwrap();
        assert!(config.dictation.enabled);
        assert!(config.dictation.use_gpu);
        assert!(!config.dictation.flash_attention);
    }

    #[test]
    fn flash_attention_can_be_enabled_and_disabled_explicitly() {
        for enabled in [true, false] {
            let config: AppConfig =
                toml::from_str(&format!("[dictation]\nflash_attention = {enabled}\n")).unwrap();
            assert_eq!(config.dictation.flash_attention, enabled);
        }
    }
}
