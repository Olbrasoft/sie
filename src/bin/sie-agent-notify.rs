//! One pending agent question; stdin carries its text, never the process args.
use anyhow::Result;
use clap::Parser;
use sie::{
    agent_notifications,
    agent_origin::{self, Question},
    desktop,
};
use std::time::Duration;
use tokio::io::AsyncReadExt;

#[derive(Parser)]
#[command(
    version,
    about = "Show a clickable question for an agent on another workspace"
)]
struct Args {
    /// Resolve the origin without displaying a notification or changing focus.
    #[arg(long)]
    resolve_only: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut bytes = Vec::new();
    tokio::io::stdin()
        .take(32769)
        .read_to_end(&mut bytes)
        .await?;
    anyhow::ensure!(bytes.len() <= 32768, "Question input is too large");
    let question: Question = serde_json::from_slice(&bytes)?;
    question.validate()?;
    let connection = zbus::connection::Builder::session()?
        .method_timeout(Duration::from_secs(2))
        .build()
        .await?;
    let snapshot = agent_origin::snapshot(&connection, &question).await?;
    let Some(origin) = agent_origin::resolve(&snapshot, &question) else {
        println!(
            "{}",
            serde_json::json!({"shown":false,"reason":"origin-unresolved-or-ambiguous"})
        );
        return Ok(());
    };
    let workspace = origin.workspace(&snapshot).unwrap();
    if args.resolve_only {
        println!(
            "{}",
            serde_json::json!({"window":origin.window,"workspace":workspace,"pane":origin.pane,"session":origin.session})
        );
        return Ok(());
    }
    if desktop::current_workspace(&connection).await? == workspace {
        println!(
            "{}",
            serde_json::json!({"shown":false,"reason":"same-workspace"})
        );
        return Ok(());
    }
    agent_notifications::present(
        &connection,
        &question,
        workspace,
        || agent_origin::activate(&connection, &origin, &question),
        async {
            tokio::select! {_=terminate.recv()=>{}, _=tokio::signal::ctrl_c()=>{}}
        },
    )
    .await
}
