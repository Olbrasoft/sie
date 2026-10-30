use super::*;
use super::{
    credentials::Credentials,
    providers::{
        google::google_request,
        http::{sensitive, xml},
    },
};
use reqwest::Client;
use std::time::Instant;
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
        backend: Providers {
            credentials: Credentials(HashMap::from([
                ("one".into(), "fake-secret-one".into()),
                ("two".into(), "fake-secret-two".into()),
            ])),
            store,
            client,
            next_key: 0,
        },
        circuits: Default::default(),
    };
    for _ in 0..4 {
        assert_eq!(
            chain.synthesize("Test", "codex").await.unwrap(),
            b"fixture-audio"
        );
    }
    assert_eq!(std::fs::read(edge.with_extension("log")).unwrap(), b"xxx");
}

struct FakeSynthesizer {
    attempts: Vec<String>,
    succeed: Option<String>,
    stall: Option<String>,
}
impl Synthesizer for FakeSynthesizer {
    async fn synthesize(
        &mut self,
        provider: &Provider,
        _profile: &Profile,
        _text: &str,
    ) -> Result<Vec<u8>> {
        self.attempts.push(provider.name.clone());
        if self.stall.as_ref() == Some(&provider.name) {
            std::future::pending::<()>().await;
        }
        anyhow::ensure!(
            self.succeed.as_ref() == Some(&provider.name),
            "Synthetic provider failure"
        );
        Ok(b"fixture-audio".to_vec())
    }
}
fn fake_chain(succeed: Option<&str>, stall: Option<&str>) -> Chain<FakeSynthesizer> {
    let settings: Settings = serde_json::from_str(include_str!(
        "../../../packaging/notifications.example.json"
    ))
    .unwrap();
    Chain {
        settings,
        backend: FakeSynthesizer {
            attempts: Vec::new(),
            succeed: succeed.map(str::to_owned),
            stall: stall.map(str::to_owned),
        },
        circuits: Default::default(),
    }
}
#[tokio::test]
async fn fallback_policy_skips_open_circuits_preserves_order_and_retries_local_provider() {
    let mut chain = fake_chain(Some("Piper"), None);
    for _ in 0..4 {
        assert_eq!(
            chain.synthesize("Fixture", "codex").await.unwrap(),
            b"fixture-audio"
        );
    }
    assert_eq!(
        chain
            .backend
            .attempts
            .iter()
            .filter(|p| p.as_str() == "GoogleCloudMultiKey")
            .count(),
        3
    );
    assert_eq!(
        chain
            .backend
            .attempts
            .iter()
            .filter(|p| p.as_str() == "Piper")
            .count(),
        4
    );
    assert!(chain.circuits["Piper"].available());
    let mut failed = fake_chain(None, None);
    for _ in 0..4 {
        assert!(failed.synthesize("Fixture", "codex").await.is_err());
    }
    assert_eq!(
        failed
            .backend
            .attempts
            .iter()
            .filter(|p| p.as_str() == "Piper")
            .count(),
        4
    );
    assert!(failed.circuits["Piper"].available());
}
#[tokio::test]
async fn a_timed_out_backend_does_not_block_the_next_provider() {
    let mut chain = fake_chain(Some("Piper"), Some("GoogleCloudMultiKey"));
    // A zero-duration synthetic deadline keeps this policy test independent of wall-clock delays.
    chain.settings.providers[0].timeout_seconds = 0;
    assert_eq!(
        chain.synthesize("Fixture", "codex").await.unwrap(),
        b"fixture-audio"
    );
    assert_eq!(
        chain.backend.attempts.first().unwrap(),
        "GoogleCloudMultiKey"
    );
    assert_eq!(chain.backend.attempts.last().unwrap(), "Piper");
}
