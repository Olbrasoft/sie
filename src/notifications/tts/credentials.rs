//! Load referenced secrets from the desktop keyring without exposing Debug output.
use super::super::{config::Settings, process};
use anyhow::Result;
use std::collections::HashMap;
use tokio::process::Command;

// Intentionally no Debug implementation: these values must never reach logs.
pub(super) struct Credentials(pub(super) HashMap<String, String>);

impl Credentials {
    pub async fn load(settings: &Settings) -> Result<Self> {
        let mut values = HashMap::new();
        for provider in &settings.providers {
            for credential in &provider.credentials {
                if values.contains_key(&credential.reference) {
                    continue;
                }
                let mut command = Command::new("secret-tool");
                command.args([
                    "lookup",
                    "application",
                    "sie-tts",
                    "credential",
                    &credential.reference,
                ]);
                match process::run(&mut command, b"", 5, 8192).await {
                    Ok(bytes) => {
                        let value = String::from_utf8(bytes)
                            .map_err(|_| anyhow::anyhow!("Invalid keyring credential"))?;
                        let value = value.trim_end().to_owned();
                        if !value.is_empty() {
                            values.insert(credential.reference.clone(), value);
                        }
                    }
                    Err(_) => {
                        tracing::warn!(provider=%provider.name,credential=%credential.name,"TTS credential unavailable in desktop keyring")
                    }
                }
            }
        }
        Ok(Self(values))
    }
}
