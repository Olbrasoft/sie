use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, Response, header::HeaderValue};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::process::Command;

use super::{
    config::{Profile, Provider, Settings},
    process,
    store::Store,
};

// Intentionally no Debug implementation: these values must never reach logs.
pub struct Credentials(HashMap<String, String>);

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

#[derive(Default)]
struct Circuit {
    failures: u32,
    open_until: Option<Instant>,
    rounds: u32,
}

impl Circuit {
    fn available(&self) -> bool {
        self.open_until.is_none_or(|at| at <= Instant::now())
    }
    fn success(&mut self) {
        *self = Self::default();
    }
    fn fail(&mut self) {
        self.failures += 1;
        if self.failures >= 3 {
            self.rounds = self.rounds.saturating_add(1).min(4);
            self.open_until = Some(
                Instant::now()
                    + Duration::from_secs((300u64 * 2u64.pow(self.rounds - 1)).min(3600)),
            );
            self.failures = 0;
        }
    }
}

pub struct Chain {
    settings: Settings,
    credentials: Credentials,
    store: Arc<Store>,
    client: Client,
    circuits: HashMap<String, Circuit>,
    next_key: usize,
}

impl Chain {
    pub async fn new(settings: Settings, store: Arc<Store>) -> Result<Self> {
        let credentials = Credentials::load(&settings).await?;
        let client = Client::builder()
            .user_agent("SystemInterfaceExtensions")
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            settings,
            credentials,
            store,
            client,
            circuits: HashMap::new(),
            next_key: 0,
        })
    }

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
                self.one(&provider, &profile, text),
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

    async fn one(&mut self, p: &Provider, profile: &Profile, text: &str) -> Result<Vec<u8>> {
        let voice = if p.name == profile.provider {
            &profile.voice
        } else {
            &p.voice
        };
        match p.name.as_str() {
            "GoogleCloudMultiKey" => self.google(p, voice, profile, text).await,
            "AzureTTS" => {
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
            "VoiceRSS" => {
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
            "EdgeTTS-WebSocket" | "Piper" | "GoogleTTS" => local(p, voice, profile, text).await,
            _ => anyhow::bail!("Unknown provider"),
        }
    }

    fn key<'a>(&'a self, p: &Provider) -> Result<&'a str> {
        p.credentials
            .iter()
            .find_map(|c| self.credentials.0.get(&c.reference).map(String::as_str))
            .ok_or_else(|| anyhow::anyhow!("Provider credential unavailable"))
    }

    async fn google(
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

fn ordered(providers: &[Provider], preferred: &str) -> Vec<Provider> {
    providers
        .iter()
        .filter(|p| p.name == preferred)
        .chain(providers.iter().filter(|p| p.name != preferred))
        .cloned()
        .collect()
}

fn sensitive(value: &str) -> Result<HeaderValue> {
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

fn google_request(text: &str, voice: &str, profile: &Profile) -> serde_json::Value {
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

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

async fn local(p: &Provider, voice: &str, profile: &Profile, text: &str) -> Result<Vec<u8>> {
    let directory = tempfile::tempdir()?;
    let output = directory.path().join("speech.audio");
    let mut command = Command::new(&p.executable);
    let input = match p.name.as_str() {
        "EdgeTTS-WebSocket" => {
            let input = directory.path().join("text.txt");
            tokio::fs::write(&input, text).await?;
            command.arg("--file").arg(input).args(["--voice", voice]);
            for (flag, value) in [
                ("--rate", &profile.rate),
                ("--volume", &profile.volume),
                ("--pitch", &profile.pitch),
            ] {
                command.arg(format!("{flag}={value}"));
            }
            command.arg("--write-media").arg(&output);
            Vec::new()
        }
        "Piper" => {
            command
                .arg("--model")
                .arg(&p.model_path)
                .arg("--output_file")
                .arg(&output)
                .arg("--length_scale")
                .arg(p.length_scale.to_string());
            format!("{text}\n").into_bytes()
        }
        "GoogleTTS" => {
            command.args(["-l", "cs", "-o"]).arg(&output).arg("-");
            text.as_bytes().to_vec()
        }
        _ => anyhow::bail!("Invalid process provider"),
    };
    process::run(&mut command, &input, p.timeout_seconds, 8192).await?;
    process::read_audio(&output).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_order_and_voice_configuration_preserve_the_source() {
        let providers = ["GoogleCloudMultiKey", "AzureTTS", "Piper"].map(|name| Provider {
            name: name.into(),
            ..Default::default()
        });
        assert_eq!(
            ordered(&providers, "Piper")
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["Piper", "GoogleCloudMultiKey", "AzureTTS"]
        );
        let profile = Profile {
            provider: "GoogleCloudMultiKey".into(),
            voice: "cs-CZ-Chirp3-HD-Enceladus".into(),
            rate: "+10%".into(),
            volume: "+0%".into(),
            pitch: "+0Hz".into(),
        };
        let value = google_request("Test", &profile.voice, &profile);
        assert_eq!(value["voice"]["name"], profile.voice);
        assert_eq!(value["voice"]["languageCode"], "cs-CZ");
        assert_eq!(value["audioConfig"]["speakingRate"], 1.3);
        assert!(value["audioConfig"].get("pitch").is_none());
        assert_eq!(xml("<test a=\"&\">"), "&lt;test a=&quot;&amp;&quot;&gt;");
        assert!(sensitive("test-key").unwrap().is_sensitive());
    }
    #[test]
    fn circuit_opens_after_failures_and_success_resets_it() {
        let mut c = Circuit::default();
        c.fail();
        c.fail();
        assert!(c.available());
        c.fail();
        assert!(!c.available());
        c.open_until = Some(Instant::now());
        assert!(c.available());
        c.success();
        assert!(c.available());
    }

    #[tokio::test]
    #[ignore = "requires private SIE_TEST_DB; fake credentials, blocked network and fake synthesis helpers"]
    async fn exhausted_google_keys_fall_back_to_local_synthesis_and_trip_failed_provider() {
        use super::super::config::Credential;
        use std::os::unix::fs::PermissionsExt;
        let url = std::env::var("SIE_TEST_DB").unwrap();
        let (store, _) = Store::isolated_test_store(&url, 8).await.unwrap();
        let store = Arc::new(store);
        for name in ["one", "two"] {
            assert!(store.reserve_key(name, 1, 1).await.unwrap());
        }
        let directory = tempfile::tempdir().unwrap();
        let edge = directory.path().join("edge");
        let piper = directory.path().join("piper");
        std::fs::write(&edge, "#!/bin/sh\nprintf x >> \"$0.log\"\nexit 1\n").unwrap();
        std::fs::write(
            &piper,
            "#!/bin/sh\nprintf fixture-audio > \"$4\"\ncat > /dev/null\n",
        )
        .unwrap();
        for file in [&edge, &piper] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let profile = Profile {
            provider: "GoogleCloudMultiKey".into(),
            voice: "cs-CZ-Chirp3-HD-Enceladus".into(),
            rate: "+10%".into(),
            volume: "+0%".into(),
            pitch: "+0Hz".into(),
        };
        let settings = Settings {
            profiles: Default::default(),
            default_profile: profile,
            sound_sink: "test.monitor".into(),
            providers: vec![
                Provider {
                    name: "GoogleCloudMultiKey".into(),
                    credentials: ["one", "two"]
                        .map(|s| Credential {
                            name: s.into(),
                            reference: s.into(),
                            monthly_limit: 1,
                        })
                        .to_vec(),
                    ..Default::default()
                },
                Provider {
                    name: "EdgeTTS-WebSocket".into(),
                    voice: "cs-CZ-AntoninNeural".into(),
                    executable: edge.clone(),
                    ..Default::default()
                },
                Provider {
                    name: "Piper".into(),
                    executable: piper,
                    model_path: "/test/model.onnx".into(),
                    ..Default::default()
                },
            ],
        };
        let client = Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let mut chain = Chain {
            settings,
            credentials: Credentials(HashMap::from([
                ("one".into(), "fake-secret-one".into()),
                ("two".into(), "fake-secret-two".into()),
            ])),
            store,
            client,
            circuits: Default::default(),
            next_key: 0,
        };
        for _ in 0..4 {
            assert_eq!(
                chain.synthesize("Test", "codex").await.unwrap(),
                b"fixture-audio"
            );
        }
        assert_eq!(std::fs::read(edge.with_extension("log")).unwrap(), b"xxx");
    }
}
