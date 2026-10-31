//! CLI entry point; desktop application lifecycle is owned by app.
mod app;
mod doctor;
use anyhow::{Context, Result};
use clap::Parser;
use sie::{
    config::AppConfig,
    corrections::Database,
    desktop::{self, Desktop},
    openrazer,
};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[derive(Parser)]
#[command(
    version,
    about = "System Interface Extensions — GNOME/Wayland keyboard proof of concept"
)]
struct Args {
    /// Configuration path (default: XDG_CONFIG_HOME/sie/config.toml).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Transcribe a 16 kHz mono 16-bit WAV locally and print corrected text; never paste.
    #[arg(long, conflicts_with_all = ["doctor", "apply_lighting", "switch_workspace", "tray_only"])]
    transcribe_file: Option<PathBuf>,
    /// Check the existing desktop integrations without changing settings or lighting.
    #[arg(long, conflicts_with_all = ["apply_lighting", "switch_workspace", "tray_only"])]
    doctor: bool,
    /// Apply the native touch-typing frame once, then exit.
    #[arg(long, conflicts_with_all = ["switch_workspace", "tray_only"])]
    apply_lighting: bool,
    /// Test a zero-based workspace target (0–4), then exit.
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=4), conflicts_with = "tray_only")]
    switch_workspace: Option<u32>,
    /// Run the tray without taking control of keyboard lighting or macro keys.
    #[arg(long)]
    tray_only: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sie=info".into()),
        )
        .init();
    let config = AppConfig::load(args.config)?;
    if let Some(path) = args.transcribe_file {
        let pcm = sie::speech::read_wav(&path)?;
        let engine = sie::speech::SpeechEngine::new(config.dictation.clone());
        let raw = engine.transcribe(&pcm, Arc::new(AtomicBool::new(false)))?;
        let processor = Database::connect(&config.dictation.database_url)
            .await?
            .postprocessor()
            .await?;
        println!("{}", processor.apply(&raw).0);
        return Ok(());
    }
    let connection = zbus::connection::Builder::session()?
        .method_timeout(Duration::from_secs(3))
        .build()
        .await
        .context("A desktop session D-Bus is required")?;
    if args.doctor {
        return doctor::run(&connection, &config.dictation).await;
    }

    // DoNotQueue prevents a second instance from waiting and later taking ownership.
    sie::claim_instance(&connection)
        .await
        .context("SIE is already running, or its application name cannot be acquired")?;
    if args.apply_lighting {
        let active = desktop::workspace_state(&connection)
            .await
            .ok()
            .and_then(|state| state.active_macro());
        let count = openrazer::apply(&connection, active).await?;
        anyhow::ensure!(count > 0, "No supported BlackWidow V4 Pro found");
        println!("Touch-typing lighting applied to {count} keyboard(s).");
        return Ok(());
    }
    if let Some(index) = args.switch_workspace {
        let desktop = Desktop::connect(&connection).await?;
        let result = desktop.switch(index).await;
        desktop.stop().await?;
        println!(
            "Workspace {index}: {}",
            if result? {
                "shortcut sent"
            } else {
                "unavailable; ignored"
            }
        );
        return Ok(());
    }
    app::run(connection, args.tray_only, config).await
}
