//! Ordered fallback policy; adapters own credentials, requests and synthesis details.
mod circuit;
mod credentials;
mod providers;
use super::{
    config::{Profile, Provider, Settings},
    store::Store,
};
use anyhow::Result;
use circuit::Circuit;
use providers::Providers;
use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};

pub(super) trait Synthesizer: Send {
    fn synthesize(
        &mut self,
        provider: &Provider,
        profile: &Profile,
        text: &str,
    ) -> impl Future<Output = Result<Vec<u8>>> + Send;
}

pub struct Chain<B = Providers> {
    settings: Settings,
    backend: B,
    circuits: HashMap<String, Circuit>,
}

impl Chain<Providers> {
    pub async fn new(settings: Settings, store: Arc<Store>) -> Result<Self> {
        let backend = Providers::new(&settings, store).await?;
        Ok(Self {
            settings,
            backend,
            circuits: HashMap::new(),
        })
    }
}

impl<B: Synthesizer> Chain<B> {
    pub async fn synthesize(&mut self, text: &str, source: &str) -> Result<Vec<u8>> {
        let profile = self.settings.profile(source).clone();
        let providers = ordered(&self.settings.providers, &profile.provider);
        for provider in providers {
            if !self
                .circuits
                .entry(provider.name.clone())
                .or_default()
                .available()
            {
                continue;
            }
            let result = tokio::time::timeout(
                Duration::from_secs(provider.timeout_seconds),
                self.backend.synthesize(&provider, &profile, text),
            )
            .await;
            let circuit = self.circuits.entry(provider.name.clone()).or_default();
            match result {
                Ok(Ok(audio)) => {
                    circuit.success();
                    tracing::info!(provider=%provider.name,"Notification speech synthesized");
                    return Ok(audio);
                }
                _ => {
                    if provider.name != "Piper" {
                        circuit.fail();
                    }
                    // Never log provider error bodies, request URLs or text.
                    tracing::warn!(provider=%provider.name,"TTS provider unavailable; trying fallback");
                }
            }
        }
        anyhow::bail!("All notification speech providers are unavailable")
    }
}
fn ordered(providers: &[Provider], preferred: &str) -> Vec<Provider> {
    providers
        .iter()
        .filter(|p| p.name == preferred)
        .chain(providers.iter().filter(|p| p.name != preferred))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;
