//! Bridge only native-messaging stdin/stdout and the local broker protocol.
use super::{INTERFACE, PATH, read_frame, write_frame};
use anyhow::Result;
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;

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
