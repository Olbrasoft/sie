use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    pub enabled: bool,
    pub listen: SocketAddr,
    pub settings_path: PathBuf,
    pub max_pending: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: ([127, 0, 0, 1], 5055).into(),
            settings_path: crate::config::AppConfig::path().with_file_name("notifications.json"),
            max_pending: 256,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub profiles: BTreeMap<String, Profile>,
    pub default_profile: Profile,
    pub providers: Vec<Provider>,
    pub sound_sink: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub provider: String,
    pub voice: String,
    #[serde(default = "default_rate")]
    pub rate: String,
    #[serde(default = "default_volume")]
    pub volume: String,
    #[serde(default = "default_pitch")]
    pub pitch: String,
}
fn default_rate() -> String {
    "+0%".into()
}
fn default_volume() -> String {
    "+0%".into()
}
fn default_pitch() -> String {
    "+0Hz".into()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Provider {
    pub name: String,
    pub voice: String,
    pub credentials: Vec<Credential>,
    pub region: String,
    pub executable: PathBuf,
    pub model_path: PathBuf,
    pub length_scale: f32,
    pub timeout_seconds: u64,
}

/// Internal provider dispatch; serialized configuration keeps its existing names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProviderKind {
    GoogleCloud,
    Azure,
    Edge,
    VoiceRss,
    GoogleTts,
    Piper,
}
impl Provider {
    pub(super) fn kind(&self) -> Result<ProviderKind> {
        match self.name.as_str() {
            "GoogleCloudMultiKey" => Ok(ProviderKind::GoogleCloud),
            "AzureTTS" => Ok(ProviderKind::Azure),
            "EdgeTTS-WebSocket" => Ok(ProviderKind::Edge),
            "VoiceRSS" => Ok(ProviderKind::VoiceRss),
            "GoogleTTS" => Ok(ProviderKind::GoogleTts),
            "Piper" => Ok(ProviderKind::Piper),
            _ => anyhow::bail!("Unknown TTS provider"),
        }
    }
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            name: String::new(),
            voice: String::new(),
            credentials: vec![],
            region: "westeurope".into(),
            executable: PathBuf::new(),
            model_path: PathBuf::new(),
            length_scale: 0.5,
            timeout_seconds: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub name: String,
    pub reference: String,
    #[serde(default = "monthly_limit")]
    pub monthly_limit: i64,
}
fn monthly_limit() -> i64 {
    1_000_000
}

impl Settings {
    pub fn load(options: &Options) -> Result<Self> {
        ensure!(
            options.listen.ip().is_loopback(),
            "Notification API must use loopback"
        );
        ensure!(
            (1..=4096).contains(&options.max_pending),
            "Invalid notification queue limit"
        );
        let bytes = std::fs::read(&options.settings_path)?;
        let settings: Self = serde_json::from_slice(&bytes).map_err(|_| {
            anyhow::anyhow!("Invalid notification settings; credentials belong in the keyring")
        })?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.providers.is_empty() && self.providers.len() <= 12,
            "Invalid TTS provider count"
        );
        ensure!(
            !self.sound_sink.trim().is_empty() && !self.sound_sink.starts_with('@'),
            "An explicit notification sound output is required"
        );
        let mut names = std::collections::HashSet::new();
        for p in &self.providers {
            p.kind()?;
            ensure!(names.insert(&p.name), "Duplicate TTS provider");
            ensure!(
                (1..=120).contains(&p.timeout_seconds),
                "Invalid TTS timeout"
            );
            ensure!((0.1..=3.0).contains(&p.length_scale), "Invalid Piper speed");
            ensure!(
                p.region
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
                "Invalid Azure region"
            );
            for c in &p.credentials {
                ensure!(
                    !c.reference.is_empty() && c.reference.len() <= 160 && c.monthly_limit > 0,
                    "Invalid TTS credential reference"
                );
            }
        }
        for p in self.profiles.values().chain([&self.default_profile]) {
            ensure!(
                self.providers.iter().any(|v| v.name == p.provider),
                "TTS profile refers to an unavailable provider"
            );
            ensure!(
                !p.voice.is_empty() && p.voice.len() < 160,
                "Invalid TTS voice"
            );
            for (value, suffix) in [(&p.rate, "%"), (&p.volume, "%"), (&p.pitch, "Hz")] {
                ensure!(
                    value
                        .strip_suffix(suffix)
                        .and_then(|v| v.parse::<i32>().ok())
                        .is_some_and(|v| (-100..=100).contains(&v)),
                    "Invalid TTS prosody"
                );
            }
        }
        Ok(())
    }

    pub fn profile(&self, source: &str) -> &Profile {
        self.profiles
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(source))
            .map(|(_, value)| value)
            .unwrap_or(&self.default_profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_example_is_valid_and_inline_credentials_are_rejected() {
        let example = include_str!("../../packaging/notifications.example.json");
        let settings: Settings = serde_json::from_str(example).unwrap();
        settings.validate().unwrap();
        assert_eq!(settings.profile("CODEX").voice, "cs-CZ-Chirp3-HD-Enceladus");
        let mut value: serde_json::Value = serde_json::from_str(example).unwrap();
        value["providers"][0]["api_key"] = serde_json::json!("fake-secret-must-not-be-accepted");
        assert!(serde_json::from_value::<Settings>(value).is_err());
        let options = Options {
            listen: ([0, 0, 0, 0], 5055).into(),
            ..Default::default()
        };
        assert!(Settings::load(&options).is_err());
    }
}
