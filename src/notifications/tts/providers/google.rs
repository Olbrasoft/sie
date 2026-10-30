//! Google key rotation and durable quota reservations.
use super::{
    Profile, Provider, Providers,
    http::{body, sensitive},
    process,
};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;

impl Providers {
    pub(super) async fn google(
        &mut self,
        p: &Provider,
        voice: &str,
        profile: &Profile,
        text: &str,
    ) -> Result<Vec<u8>> {
        let n = p.credentials.len();
        ensure!(n > 0, "Google credentials are not configured");
        let start = self.next_key % n;
        for offset in 0..n {
            let index = (start + offset) % n;
            let credential = &p.credentials[index];
            let Some(key) = self.credentials.0.get(&credential.reference) else {
                continue;
            };
            if !self
                .store
                .reserve_key(
                    &credential.reference,
                    text.encode_utf16().count() as i64,
                    credential.monthly_limit,
                )
                .await?
            {
                continue;
            }
            self.next_key = (index + 1) % n;
            let response = self
                .client
                .post("https://texttospeech.googleapis.com/v1/text:synthesize")
                .header("X-Goog-Api-Key", sensitive(key)?)
                .json(&google_request(text, voice, profile))
                .send()
                .await;
            let Ok(response) = response else {
                self.store
                    .key_error(&credential.reference, 503, false)
                    .await?;
                continue;
            };
            let status = response.status();
            if !status.is_success() {
                let bytes = body(response).await.unwrap_or_default();
                let error = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
                let quota =
                    status.as_u16() == 403 && (error.contains("quota") || error.contains("limit"));
                let code =
                    if error.contains("api key not valid") || error.contains("api_key_invalid") {
                        401
                    } else {
                        status.as_u16()
                    };
                self.store
                    .key_error(&credential.reference, code, quota)
                    .await?;
                continue;
            }
            let parsed: serde_json::Value = serde_json::from_slice(&body(response).await?)
                .map_err(|_| anyhow::anyhow!("Invalid Google speech response"))?;
            let encoded = parsed["audioContent"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Google audio missing"))?;
            let audio = STANDARD
                .decode(encoded)
                .map_err(|_| anyhow::anyhow!("Invalid Google audio"))?;
            ensure!(
                !audio.is_empty() && audio.len() <= process::MAX_AUDIO,
                "Invalid Google audio size"
            );
            return Ok(audio);
        }
        anyhow::bail!("Google speech keys unavailable or exhausted")
    }
}
pub(in crate::notifications::tts) fn google_request(
    text: &str,
    voice: &str,
    profile: &Profile,
) -> serde_json::Value {
    let language = voice.split('-').take(2).collect::<Vec<_>>().join("-");
    let mut config = json!({"audioEncoding":"MP3","sampleRateHertz":24000});
    // Preserve VirtualAssistant's rate mapping (+10% -> 1.3), including
    // Chirp's supported pace control. Chirp receives no pitch controls.
    let rate = profile
        .rate
        .trim_end_matches('%')
        .parse::<f64>()
        .unwrap_or(0.0)
        / 100.0;
    let rate = if rate >= 0.0 {
        1.0 + rate * 3.0
    } else {
        1.0 + rate * 0.75
    };
    config["speakingRate"] =
        json!(rate.clamp(0.25, if voice.contains("Chirp3") { 2.0 } else { 4.0 }));
    json!({"input":{"text":text},"voice":{"languageCode":language,"name":voice},"audioConfig":config})
}
