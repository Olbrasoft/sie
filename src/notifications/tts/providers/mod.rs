//! Concrete synthesis adapters. Fallback policy does not inspect their transport.
pub(super) mod google;
pub(super) mod http;
mod local;
use super::{Synthesizer, credentials::Credentials};
use crate::notifications::{
    config::{Profile, Provider, ProviderKind, Settings},
    process,
    store::Store,
};
use anyhow::Result;
use reqwest::Client;
use std::{sync::Arc, time::Duration};

pub(in crate::notifications) struct Providers {
    pub(super) credentials: Credentials,
    pub(super) store: Arc<Store>,
    pub(super) client: Client,
    pub(super) next_key: usize,
}
impl Providers {
    pub(super) async fn new(settings: &Settings, store: Arc<Store>) -> Result<Self> {
        let credentials = Credentials::load(settings).await?;
        let client = Client::builder()
            .user_agent("SystemInterfaceExtensions")
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            credentials,
            store,
            client,
            next_key: 0,
        })
    }
    fn key<'a>(&'a self, p: &Provider) -> Result<&'a str> {
        p.credentials
            .iter()
            .find_map(|c| self.credentials.0.get(&c.reference).map(String::as_str))
            .ok_or_else(|| anyhow::anyhow!("Provider credential unavailable"))
    }
}

impl Synthesizer for Providers {
    async fn synthesize(&mut self, p: &Provider, profile: &Profile, text: &str) -> Result<Vec<u8>> {
        let voice = if p.name == profile.provider {
            &profile.voice
        } else {
            &p.voice
        };
        match p.kind()? {
            ProviderKind::GoogleCloud => self.google(p, voice, profile, text).await,
            ProviderKind::Azure => self.azure(p, voice, profile, text).await,
            ProviderKind::VoiceRss => self.voice_rss(p, voice, profile, text).await,
            ProviderKind::Edge | ProviderKind::Piper | ProviderKind::GoogleTts => {
                local::synthesize(p, voice, profile, text).await
            }
        }
    }
}
