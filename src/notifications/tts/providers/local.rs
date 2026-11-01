//! Temporary files and bounded process execution for CLI synthesis providers.
use super::{Profile, Provider, ProviderKind, process};
use anyhow::Result;
use tokio::process::Command;

pub(super) async fn synthesize(
    p: &Provider,
    voice: &str,
    profile: &Profile,
    text: &str,
) -> Result<Vec<u8>> {
    let directory = tempfile::tempdir()?;
    let output = directory.path().join("speech.audio");
    let mut command = Command::new(&p.executable);
    let input = match p.kind()? {
        ProviderKind::Edge => {
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
        ProviderKind::Piper => {
            command
                .arg("--model")
                .arg(&p.model_path)
                .arg("--output_file")
                .arg(&output)
                .arg("--length_scale")
                .arg(p.length_scale.to_string());
            format!("{text}\n").into_bytes()
        }
        ProviderKind::GoogleTts => {
            command.args(["-l", "cs", "-o"]).arg(&output).arg("-");
            text.as_bytes().to_vec()
        }
        _ => anyhow::bail!("Invalid process provider"),
    };
    process::run(&mut command, &input, p.timeout_seconds, 8192).await?;
    process::read_audio(&output).await
}
