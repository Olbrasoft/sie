//! Native notification actions; displaying a question never moves focus.
use crate::agent_origin::Question;
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use std::{collections::HashMap, future::Future, time::Duration};
use zbus::{
    Connection, Proxy,
    zvariant::{Str, Value},
};

fn markup_text(text: &str) -> String {
    text.chars()
        .take(700)
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The activation closure is invoked only for this notification's user action.
pub async fn present<F, A, C>(
    connection: &Connection,
    question: &Question,
    workspace: i32,
    activate: F,
    cancelled: C,
) -> Result<()>
where
    F: FnOnce() -> A,
    A: Future<Output = Result<()>>,
    C: Future<Output = ()>,
{
    let proxy = Proxy::new(
        connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await?;
    let caps: Vec<String> = proxy.call("GetCapabilities", &()).await?;
    ensure!(
        caps.iter().any(|s| s == "actions"),
        "Desktop notifications do not support click actions"
    );
    let mut actions = proxy.receive_signal("ActionInvoked").await?;
    let mut closed = proxy.receive_signal("NotificationClosed").await?;
    tokio::pin!(cancelled);
    // A question resolved while metadata was being queried must not flash up.
    tokio::select! {biased; _=&mut cancelled=>return Ok(()), _=std::future::ready(())=>{}}
    let mut hints = HashMap::<&str, Value<'_>>::new();
    hints.insert("suppress-sound", Value::from(true));
    // Pending questions need a banner even while the user watches fullscreen video.
    hints.insert("urgency", Value::from(2u8));
    hints.insert(
        "desktop-entry",
        Value::from(Str::from("cz.olbrasoft.SystemInterfaceExtensions")),
    );
    let project = question
        .cwd
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .take(48)
        .collect::<String>();
    let summary = format!(
        "{} čeká na odpověď · {} · Plocha {}",
        question.agent,
        project,
        workspace + 1
    );
    let body = markup_text(&question.text);
    let id: u32 = proxy
        .call(
            "Notify",
            &(
                "System Interface Extensions",
                0u32,
                "dialog-question",
                &summary,
                &body,
                vec!["default", "Otevřít otázku", "open", "Přejít k otázce"],
                hints,
                0i32,
            ),
        )
        .await?;
    let result: Result<()> = async {
        ensure!(id > 0, "Invalid notification ID");
        let expiry = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(expiry);
        loop {
            tokio::select! {
                biased;
                _=&mut cancelled=>return Ok(()),
                message=actions.next()=>{
                    let message=message.context("Notification action stream closed")?;
                    if let Ok((notification,action))=message.body().deserialize::<(u32,String)>()
                        && notification==id && matches!(action.as_str(),"default"|"open") {
                            return activate().await;
                        }
                },
                message=closed.next()=>{
                    let message=message.context("Notification close stream closed")?;
                    if let Ok((notification,_))=message.body().deserialize::<(u32,u32)>()
                        && notification==id {return Ok(());}
                },
                _=&mut expiry=>return Ok(()),
            }
        }
    }
    .await;
    let _ = proxy.call::<_, _, ()>("CloseNotification", &(id,)).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn question_body_is_plain_text_and_bounded_without_breaking_unicode() {
        assert_eq!(markup_text("<věta & další>"), "&lt;věta &amp; další&gt;");
        assert_eq!(markup_text(&"č".repeat(1000)).chars().count(), 700);
    }
}
