//! Local browser native-messaging broker; messages contain only matching tabs.
use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot},
};

pub const PATH: &str = "/cz/olbrasoft/SystemInterfaceExtensions/Browser";
pub const INTERFACE: &str = "cz.olbrasoft.SystemInterfaceExtensions.Browser";
const MAX_FRAME: usize = 65536;
struct Session {
    pid: u32,
    host_pid: u32,
    tx: mpsc::Sender<String>,
    rx: tokio::sync::Mutex<mpsc::Receiver<String>>,
}
struct Pending {
    session: u64,
    reply: oneshot::Sender<Value>,
}
struct PendingGuard<'a> {
    entries: &'a Mutex<HashMap<u64, Pending>>,
    id: u64,
}
impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.entries.lock().unwrap().remove(&self.id);
    }
}
pub struct Broker {
    sessions: Mutex<HashMap<u64, Arc<Session>>>,
    pending: Mutex<HashMap<u64, Pending>>,
    next: AtomicU64,
}
impl Default for Broker {
    fn default() -> Self {
        Self {
            sessions: Mutex::default(),
            pending: Mutex::default(),
            next: AtomicU64::new(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
                    * 1024,
            ),
        }
    }
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tab {
    pub id: u32,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub audible: bool,
}
impl Broker {
    fn register(&self, pid: u32, host_pid: u32) -> u64 {
        self.sessions
            .lock()
            .unwrap()
            .retain(|_, s| std::path::Path::new(&format!("/proc/{}", s.host_pid)).exists());
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(8);
        self.sessions.lock().unwrap().insert(
            id,
            Arc::new(Session {
                pid,
                host_pid,
                tx,
                rx: tokio::sync::Mutex::new(rx),
            }),
        );
        id
    }
    fn disconnect(&self, id: u64) {
        self.sessions.lock().unwrap().remove(&id);
        self.pending.lock().unwrap().retain(|_, p| p.session != id);
    }
    async fn next_command(&self, id: u64) -> String {
        let session = self.sessions.lock().unwrap().get(&id).cloned();
        let Some(session) = session else {
            return String::new();
        };
        tokio::time::timeout(Duration::from_secs(20), session.rx.lock().await.recv())
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }
    fn reply(&self, session: u64, id: u64, value: Value) {
        let mut pending = self.pending.lock().unwrap();
        if pending.get(&id).is_some_and(|p| p.session == session)
            && let Some(p) = pending.remove(&id)
        {
            let _ = p.reply.send(value);
        }
    }
    async fn request(&self, session_id: u64, mut command: Value) -> Result<Value> {
        let session = self
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .cloned()
            .context("Browser bridge disconnected")?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        command["request_id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(
            id,
            Pending {
                session: session_id,
                reply: tx,
            },
        );
        // Outer reveal timeouts can cancel this future before its own timeout.
        let _pending = PendingGuard {
            entries: &self.pending,
            id,
        };
        async {
            session
                .tx
                .try_send(serde_json::to_string(&command)?)
                .context("Browser bridge is busy")?;
            let reply = tokio::time::timeout(Duration::from_secs(2), rx)
                .await
                .context("Browser bridge did not respond")??;
            ensure!(
                reply.get("ok").and_then(Value::as_bool) == Some(true),
                "Browser tab request was rejected"
            );
            Ok(reply)
        }
        .await
    }
    pub async fn activate(&self, pid: u32, title: &str, url: &str) -> Result<Tab> {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| s.pid == pid)
            .map(|(id, _)| *id)
            .collect();
        ensure!(!sessions.is_empty(), "Browser extension is not connected");
        let mut requests = FuturesUnordered::new();
        for session in sessions {
            requests.push(async move {
                (
                    session,
                    self.request(session, json!({"op":"find","title":title,"url":url}))
                        .await,
                )
            });
        }
        let mut matches = Vec::new();
        while let Some((session, reply)) = requests.next().await {
            if let Ok(reply) = reply {
                let tabs: Vec<Tab> =
                    serde_json::from_value(reply.get("tabs").cloned().unwrap_or(json!([])))?;
                ensure!(tabs.len() <= 16, "Browser returned too many matching tabs");
                matches.extend(tabs.into_iter().map(|tab| (session, tab)));
            }
        }
        if matches.len() > 1 && matches.iter().filter(|(_, tab)| tab.audible).count() == 1 {
            matches.retain(|(_, tab)| tab.audible);
        }
        ensure!(matches.len() == 1, "Media tab is missing or ambiguous");
        let (session, tab) = matches.pop().unwrap();
        let reply = self
            .request(
                session,
                json!({"op":"activate","tab_id":tab.id,"title":title,"url":url}),
            )
            .await?;
        Ok(serde_json::from_value(
            reply
                .get("tab")
                .cloned()
                .context("Browser activation was not confirmed")?,
        )?)
    }
}
struct Interface(Arc<Broker>);
#[zbus::interface(name = "cz.olbrasoft.SystemInterfaceExtensions.Browser")]
impl Interface {
    fn register(&self, pid: u32, host_pid: u32) -> zbus::fdo::Result<u64> {
        if self.0.sessions.lock().unwrap().len() >= 32 {
            return Err(zbus::fdo::Error::LimitsExceeded(
                "Too many browser connections".into(),
            ));
        }
        Ok(self.0.register(pid, host_pid))
    }
    #[zbus(property)]
    fn connected_browsers(&self) -> Vec<u32> {
        let mut pids: Vec<_> = self
            .0
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(|s| s.pid)
            .collect();
        pids.sort_unstable();
        pids.dedup();
        pids
    }
    async fn next(&self, session: u64) -> zbus::fdo::Result<String> {
        if !self.0.sessions.lock().unwrap().contains_key(&session) {
            return Err(zbus::fdo::Error::Failed("Browser session expired".into()));
        }
        Ok(self.0.next_command(session).await)
    }
    fn reply(&self, session: u64, id: u64, payload: &str) -> zbus::fdo::Result<()> {
        if payload.len() > MAX_FRAME {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Browser response too large".into(),
            ));
        }
        let value = serde_json::from_str(payload)
            .map_err(|_| zbus::fdo::Error::InvalidArgs("Invalid browser response".into()))?;
        self.0.reply(session, id, value);
        Ok(())
    }
    fn disconnect(&self, session: u64) {
        self.0.disconnect(session);
    }
}
pub async fn export(connection: &zbus::Connection, broker: Arc<Broker>) -> Result<()> {
    connection
        .object_server()
        .at(PATH, Interface(broker))
        .await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Value>> {
    let mut size = [0; 4];
    if reader.read(&mut size[..1]).await? == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut size[1..])
        .await
        .context("Incomplete native frame")?;
    let size = u32::from_ne_bytes(size) as usize;
    ensure!(size > 0 && size <= MAX_FRAME, "Invalid native frame size");
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_FRAME, "Native frame too large");
    writer
        .write_all(&(bytes.len() as u32).to_ne_bytes())
        .await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn host() -> Result<()> {
    let pid = nix::unistd::getppid().as_raw() as u32;
    let connection = zbus::connection::Builder::session()?
        .method_timeout(Duration::from_secs(25))
        .build()
        .await?;
    let proxy = zbus::Proxy::new(&connection, crate::BUS_NAME, PATH, INTERFACE).await?;
    let session = Arc::new(AtomicU64::new(0));
    let current = session.clone();
    let poll = proxy.clone();
    let (tx, mut commands) = mpsc::channel(8);
    let worker = tokio::spawn(async move {
        loop {
            if let Ok(id) = poll
                .call::<_, _, u64>("Register", &(pid, std::process::id()))
                .await
            {
                current.store(id, Ordering::SeqCst);
                while let Ok(command) = poll.call::<_, _, String>("Next", &(id,)).await {
                    if !command.is_empty() && tx.send(command).await.is_err() {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    let (frames, mut incoming) = mpsc::channel(8);
    let reader = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        loop {
            match read_frame(&mut stdin).await {
                Ok(Some(frame)) => {
                    if frames.send(Ok(frame)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = frames.send(Err(error)).await;
                    break;
                }
            }
        }
    });
    let mut stdout = tokio::io::stdout();
    let result:Result<()>=async{
        loop{tokio::select!{
            message=incoming.recv()=>{
                let Some(message)=message else{break;};
                let message=message?;
                if let Some(id)=message.get("request_id").and_then(Value::as_u64){
                    let payload=serde_json::to_string(&message)?;
                    let _=proxy.call::<_,_,()>("Reply",&(session.load(Ordering::SeqCst),id,payload)).await;
                }
            },
            Some(command)=commands.recv()=>write_frame(&mut stdout,&serde_json::from_str(&command)?).await?,
        }}
        Ok(())
    }.await;
    reader.abort();
    let _ = reader.await;
    worker.abort();
    let _ = worker.await;
    let _ = proxy
        .call::<_, _, ()>("Disconnect", &(session.load(Ordering::SeqCst),))
        .await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_frames_preserve_unicode_and_reject_incomplete_or_oversized_data() {
        let value = json!({"title":"Příliš žluťoučký kůň"});
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &value).await.unwrap();
        let mut reader = bytes.as_slice();
        assert_eq!(read_frame(&mut reader).await.unwrap(), Some(value));
        assert!(read_frame(&mut reader).await.unwrap().is_none());
        let mut partial = &[1u8, 0][..];
        assert!(read_frame(&mut partial).await.is_err());
        let size = (MAX_FRAME as u32 + 1).to_ne_bytes();
        let mut oversized = size.as_slice();
        assert!(read_frame(&mut oversized).await.is_err());
    }
    #[tokio::test]
    async fn broker_correlates_local_requests_and_rejects_other_sessions() {
        let broker = Arc::new(Broker::default());
        let session = broker.register(123, std::process::id());
        let other = broker.register(456, std::process::id());
        let b = broker.clone();
        let request = tokio::spawn(async move { b.request(session, json!({"op":"find"})).await });
        let command: Value = serde_json::from_str(&broker.next_command(session).await).unwrap();
        let id = command["request_id"].as_u64().unwrap();
        broker.reply(other, id, json!({"ok":true,"wrong":true}));
        assert!(!request.is_finished());
        broker.reply(session, id, json!({"ok":true,"tabs":[]}));
        assert!(request.await.unwrap().is_ok());
        assert!(broker.pending.lock().unwrap().is_empty());
        let b = broker.clone();
        let canceled = tokio::spawn(async move { b.request(session, json!({"op":"find"})).await });
        let _ = broker.next_command(session).await;
        canceled.abort();
        let _ = canceled.await;
        assert!(broker.pending.lock().unwrap().is_empty());
        broker.disconnect(session);
        assert!(broker.request(session, json!({})).await.is_err());
        assert!(broker.activate(999, "missing", "").await.is_err());
    }
}
