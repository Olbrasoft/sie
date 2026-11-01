//! Bounded HTTP synthesis with credential-safe errors and headers.
use super::{Profile, Provider, Providers, process};
use anyhow::{Result, ensure};
use reqwest::{Response, header::HeaderValue};

impl Providers {
    pub(super) async fn azure(
        &self,
        p: &Provider,
        voice: &str,
        profile: &Profile,
        text: &str,
    ) -> Result<Vec<u8>> {
        let key = self.key(p)?;
        let url = format!(
            "https://{}.tts.speech.microsoft.com/cognitiveservices/v1",
            p.region
        );
        let ssml = format!(
            "<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\" xml:lang=\"cs-CZ\"><voice name=\"{}\"><prosody rate=\"{}\" pitch=\"{}\" volume=\"{}\">{}</prosody></voice></speak>",
            xml(voice),
            xml(&profile.rate),
            xml(&profile.pitch),
            xml(&profile.volume),
            xml(text)
        );
        let response = self
            .client
            .post(url)
            .header("Ocp-Apim-Subscription-Key", sensitive(key)?)
            .header(
                "X-Microsoft-OutputFormat",
                "audio-24khz-96kbitrate-mono-mp3",
            )
            .header("Content-Type", "application/ssml+xml")
            .body(ssml)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Azure request failed"))?;
        ensure!(response.status().is_success(), "Azure rejected synthesis");
        body(response).await
    }
    pub(super) async fn voice_rss(
        &self,
        p: &Provider,
        voice: &str,
        profile: &Profile,
        text: &str,
    ) -> Result<Vec<u8>> {
        let key = self.key(p)?;
        let rate = (3 + profile
            .rate
            .trim_end_matches('%')
            .parse::<i32>()
            .unwrap_or(0)
            / 10)
            .clamp(-10, 10)
            .to_string();
        let fields = [
            ("key", key),
            ("src", text),
            ("hl", "cs-cz"),
            ("v", voice),
            ("c", "MP3"),
            ("f", "44khz_16bit_mono"),
            ("r", rate.as_str()),
        ];
        let response = self
            .client
            .post("https://api.voicerss.org/")
            .form(&fields)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("VoiceRSS request failed"))?;
        ensure!(
            response.status().is_success(),
            "VoiceRSS rejected synthesis"
        );
        let audio = body(response).await?;
        ensure!(
            !audio.starts_with(b"ERROR"),
            "VoiceRSS synthesis unavailable"
        );
        Ok(audio)
    }
}
pub(in crate::notifications::tts) fn sensitive(value: &str) -> Result<HeaderValue> {
    let mut header =
        HeaderValue::from_str(value).map_err(|_| anyhow::anyhow!("Invalid TTS credential"))?;
    header.set_sensitive(true);
    Ok(header)
}

pub(crate) async fn body(mut response: Response) -> Result<Vec<u8>> {
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= process::MAX_AUDIO as u64),
        "TTS response too large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("TTS response failed"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= process::MAX_AUDIO,
            "TTS response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(in crate::notifications::tts) fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
