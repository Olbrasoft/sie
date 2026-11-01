use super::{Control, Request, store::Store};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Api {
    store: Arc<Store>,
    control: Arc<Control>,
    credentials: Arc<Vec<super::config::Credential>>,
}
type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

pub fn router(
    store: Arc<Store>,
    control: Arc<Control>,
    credentials: Vec<super::config::Credential>,
) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/notifications", post(enqueue))
        .route("/api/tts/status", get(status))
        .route("/api/tts/keys-usage", get(keys_usage))
        .route("/api/tts/stop", post(stop))
        .route("/api/tts/mute", post(mute))
        .route("/api/tts/flush-queue", post(flush))
        .route("/api/speech-lock/start", post(lock))
        .route("/api/speech-lock/stop", post(unlock))
        .route("/api/speech-lock/status", get(status))
        .layer(DefaultBodyLimit::max(32768))
        .with_state(Api {
            store,
            control,
            credentials: Arc::new(credentials),
        })
}
async fn keys_usage(State(api): State<Api>) -> Reply {
    Ok(Json(
        api.store
            .key_usage(&api.credentials)
            .await
            .map_err(|_| unavailable())?,
    ))
}
fn unavailable() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"success":false,"error":"Notification queue is unavailable or full"})),
    )
}
fn local(headers: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    // Agent clients do not send Origin. A browser page cannot drive this local
    // speech/control API with cross-origin requests, even using simple POSTs.
    if headers.contains_key("origin") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"success":false,"error":"Browser origins are not accepted"})),
        ));
    }
    Ok(())
}
async fn health(State(api): State<Api>) -> Reply {
    api.store.count().await.map_err(|_| unavailable())?;
    Ok(Json(
        json!({"status":"healthy","service":"SIE","version":crate::VERSION}),
    ))
}
async fn enqueue(
    State(api): State<Api>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> Reply {
    local(&headers)?;
    request.validate().map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"success":false,"error":e.to_string()})),
        )
    })?;
    let id = api
        .store
        .enqueue(&request)
        .await
        .map_err(|_| unavailable())?;
    api.control.wake.notify_one();
    Ok(Json(
        json!({"success":true,"id":id,"text":request.text,"source":request.source}),
    ))
}
async fn status(State(api): State<Api>) -> Reply {
    let count = api.store.count().await.map_err(|_| unavailable())?;
    let gate = api.control.gate.borrow();
    let speech = api.control.status();
    Ok(Json(
        json!({"success":true,"isLocked":gate.recording || gate.external_until.is_some_and(|t|t>Instant::now()),"muted":gate.muted,"isProcessing":speech.speaking,"queueCount":count,"batchingQueueCount":count,"ttsQueueCount":0,"unavailable":speech.unavailable}),
    ))
}
async fn stop(State(api): State<Api>, headers: HeaderMap) -> Reply {
    local(&headers)?;
    api.control.stop_current();
    Ok(Json(json!({"success":true})))
}
#[derive(Deserialize)]
struct Mute {
    muted: bool,
}
async fn mute(State(api): State<Api>, headers: HeaderMap, Json(value): Json<Mute>) -> Reply {
    local(&headers)?;
    api.control.gate.send_modify(|g| g.muted = value.muted);
    Ok(Json(json!({"success":true,"muted":value.muted})))
}
async fn flush(State(api): State<Api>, headers: HeaderMap) -> Reply {
    local(&headers)?;
    api.control.wake.notify_one();
    Ok(Json(json!({"success":true})))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lock {
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
}
fn default_timeout() -> u64 {
    30
}
async fn lock(State(api): State<Api>, headers: HeaderMap, Json(value): Json<Lock>) -> Reply {
    local(&headers)?;
    if !(1..=3600).contains(&value.timeout_seconds) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"success":false,"error":"Invalid speech lock timeout"})),
        ));
    }
    api.control.gate.send_modify(|g| {
        g.external_until = Some(Instant::now() + Duration::from_secs(value.timeout_seconds))
    });
    api.control.wait_silent().await.map_err(|_| unavailable())?;
    Ok(Json(json!({"success":true,"isLocked":true})))
}
async fn unlock(State(api): State<Api>, headers: HeaderMap) -> Reply {
    local(&headers)?;
    api.control.gate.send_modify(|g| g.external_until = None);
    status(State(api)).await
}
