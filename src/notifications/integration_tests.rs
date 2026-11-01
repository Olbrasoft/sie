//! Explicit private PostgreSQL + loopback HTTP, with fake speech only.
use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn request(text: &str) -> Request {
    Request {
        text: text.into(),
        source: "codex".into(),
        issue_ids: vec![85],
        provider_name: None,
        model_name: None,
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
struct FakeSpeaker {
    events: Arc<Mutex<Vec<String>>>,
    active: Arc<AtomicUsize>,
    maximum: Arc<AtomicUsize>,
}
struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Speaker for FakeSpeaker {
    async fn speak(&mut self, r: &Request) -> Result<()> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        let _guard = Active(self.active.clone());
        self.events
            .lock()
            .unwrap()
            .push(format!("start:{}", r.text));
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.events.lock().unwrap().push(format!("done:{}", r.text));
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires a dedicated SIE_TEST_DB; no real desktop, keyring, providers or audio"]
async fn durable_api_fifo_quota_and_recording_coordination() {
    let url = std::env::var("SIE_TEST_DB").unwrap();
    let (store, admin) = Store::isolated_test_store(&url, 2).await.unwrap();
    let store = Arc::new(store);
    store.own_consumer().await.unwrap();
    let control = Control::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(
        axum::serve(
            listener,
            api::router(store.clone(), control.clone(), vec![]),
        )
        .into_future(),
    );
    let client = reqwest::Client::new();
    let endpoint = format!("http://{address}/api/notifications");
    let send = |r: Request| client.post(&endpoint).json(&r).send();
    let (one, two) = tokio::join!(send(request("first")), send(request("second")));
    let mut accepted = [
        one.unwrap().json::<serde_json::Value>().await.unwrap(),
        two.unwrap().json::<serde_json::Value>().await.unwrap(),
    ];
    assert!(accepted.iter().all(|r| r["success"] == true));
    accepted.sort_by_key(|r| r["id"].as_i64().unwrap());
    assert_eq!(store.count().await.unwrap(), 2);
    assert_eq!(send(request("full")).await.unwrap().status(), 503);
    assert_eq!(
        client
            .post(&endpoint)
            .header("Origin", "https://example.invalid")
            .json(&request("blocked"))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(send(request("")).await.unwrap().status(), 400);
    assert_eq!(store.count().await.unwrap(), 2);

    assert!(store.reserve_key("key-one", 6, 10).await.unwrap());
    assert!(!store.reserve_key("key-one", 5, 10).await.unwrap());
    assert!(store.reserve_key("key-two", 5, 10).await.unwrap());
    store.key_error("key-two", 429, false).await.unwrap();
    assert!(!store.reserve_key("key-two", 1, 10).await.unwrap());
    store.key_error("key-one", 401, false).await.unwrap();
    assert!(!store.reserve_key("key-one", 1, 10).await.unwrap());
    admin.execute("UPDATE notification_key_usage SET counter_month=counter_month-INTERVAL '1 month',disabled=FALSE,cooldown_until=NULL",&[]).await.unwrap();
    assert!(store.reserve_key("key-one", 10, 10).await.unwrap());

    control.recording(true).await.unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(worker(
        store.clone(),
        control.clone(),
        FakeSpeaker {
            events: events.clone(),
            active: active.clone(),
            maximum: maximum.clone(),
        },
    ));
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(events.lock().unwrap().is_empty());
    control.recording(false).await.unwrap();
    until(|| active.load(Ordering::SeqCst) == 1).await;
    // A redundant unlock/mute-off must not restart speech already in progress.
    control.recording(false).await.unwrap();
    control.gate.send_modify(|g| g.external_until = None);
    tokio::time::sleep(Duration::from_millis(15)).await;
    assert_eq!(events.lock().unwrap().len(), 1);
    control.recording(true).await.unwrap();
    assert_eq!(
        active.load(Ordering::SeqCst),
        0,
        "Recording waits until owned speech is stopped"
    );
    assert_eq!(
        store.count().await.unwrap(),
        2,
        "Interrupted speech remains durable"
    );
    control.recording(false).await.unwrap();
    until(|| {
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("done:"))
            .count()
            == 2
    })
    .await;
    until(|| !control.status().speaking && control.status().pending == 0).await;
    let done: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|s| s.strip_prefix("done:").map(str::to_owned))
        .collect();
    assert_eq!(
        done,
        accepted
            .iter()
            .map(|r| r["text"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    );
    assert_eq!(maximum.load(Ordering::SeqCst), 1, "Speech cannot overlap");
    assert_eq!(
        store.count().await.unwrap(),
        0,
        "Completed notification bodies are deleted"
    );

    store.enqueue(&request("recover")).await.unwrap();
    until(|| active.load(Ordering::SeqCst) == 1).await;
    task.abort();
    let _ = task.await;
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.count().await.unwrap(),
        1,
        "A stopped worker never loses the queued item"
    );
    let next_control = Control::new();
    let next = tokio::spawn(worker(
        store.clone(),
        next_control.clone(),
        FakeSpeaker {
            events: events.clone(),
            active: active.clone(),
            maximum: maximum.clone(),
        },
    ));
    until(|| events.lock().unwrap().contains(&"done:recover".into())).await;
    until(|| !next_control.status().speaking && next_control.status().pending == 0).await;
    next_control.gate.send_modify(|g| g.shutdown = true);
    next.await.unwrap();
    server.abort();
    let _ = server.await;
    assert_eq!(store.count().await.unwrap(), 0);
}
