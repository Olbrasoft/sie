//! Available local models, deferred selection and configuration persistence.
use anyhow::{Context, Result, ensure};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhisperModel {
    LargeV3,
    LargeV3Turbo,
}

impl WhisperModel {
    pub const ALL: [Self; 2] = [Self::LargeV3, Self::LargeV3Turbo];

    pub fn label(self) -> &'static str {
        match self {
            Self::LargeV3 => "Whisper Large-v3",
            Self::LargeV3Turbo => "Whisper Large-v3 Turbo",
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            Self::LargeV3 => "ggml-large-v3.bin",
            Self::LargeV3Turbo => "ggml-large-v3-turbo.bin",
        }
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        Self::ALL.into_iter().find(|model| {
            path.file_name()
                .is_some_and(|name| name == model.file_name())
        })
    }
}

#[derive(Debug, Clone)]
pub struct Status {
    pub selected: PathBuf,
    pub pending: Option<WhisperModel>,
    pub switching: bool,
    pub error: bool,
}

impl Status {
    pub fn new(selected: PathBuf) -> Self {
        Self {
            selected,
            pending: None,
            switching: false,
            error: false,
        }
    }

    pub fn label(&self) -> &str {
        WhisperModel::from_path(&self.selected).map_or("Vlastní model", WhisperModel::label)
    }

    pub fn path(&self, model: WhisperModel) -> PathBuf {
        self.selected.with_file_name(model.file_name())
    }

    pub fn request(&mut self, model: WhisperModel) {
        self.error = !self.path(model).is_file();
        self.pending = if self.error || self.path(model) == self.selected {
            None
        } else {
            Some(model)
        };
    }

    /// Canceled jobs still own their engine until all native inference has drained.
    pub fn begin_switch(
        &mut self,
        recording: bool,
        active: bool,
        pending_jobs: bool,
    ) -> Option<PathBuf> {
        if recording || active || pending_jobs {
            return None;
        }
        let path = self.path(self.pending?);
        self.switching = true;
        Some(path)
    }

    pub fn complete(&mut self, path: PathBuf, success: bool) {
        if success {
            self.selected = path;
        }
        self.pending = None;
        self.switching = false;
        self.error = !success;
    }
}

/// Edit only the model path. Keep credentials and unrelated settings out of logs.
pub(crate) fn persist(config_path: &Path, model_path: &Path) -> Result<()> {
    let path = if config_path.exists() {
        config_path
            .canonicalize()
            .context("Cannot resolve SIE configuration")?
    } else {
        config_path.to_owned()
    };
    let original = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => anyhow::bail!("Cannot read SIE configuration"),
    };
    let mut document = original
        .as_deref()
        .unwrap_or("")
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| anyhow::anyhow!("Invalid SIE configuration"))?;
    toml::from_str::<crate::config::AppConfig>(original.as_deref().unwrap_or(""))
        .map_err(|_| anyhow::anyhow!("Invalid SIE configuration"))?;
    let model = model_path.to_str().context("Invalid model path")?;
    // Preserve a trailing comment on the existing value as well as the document.
    let item = &mut document["dictation"]["model_path"];
    let decor = item.as_value().map(|value| value.decor().clone());
    *item = toml_edit::value(model);
    if let Some(decor) = decor {
        *item.as_value_mut().unwrap().decor_mut() = decor;
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).context("Cannot create SIE configuration directory")?;
    let mut file =
        tempfile::NamedTempFile::new_in(parent).context("Cannot prepare SIE configuration")?;
    // NamedTempFile starts with mode 0600 even when the existing file is public.
    file.write_all(document.to_string().as_bytes())
        .context("Cannot write SIE configuration")?;
    file.as_file()
        .sync_all()
        .context("Cannot sync SIE configuration")?;
    let current = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => anyhow::bail!("Cannot recheck SIE configuration"),
    };
    ensure!(
        current == original,
        "SIE configuration changed during model selection"
    );
    file.persist(&path)
        .map_err(|_| anyhow::anyhow!("Cannot save SIE configuration"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn switching_waits_for_recording_inference_and_canceled_jobs_and_latest_choice_wins() {
        let dir = tempfile::tempdir().unwrap();
        for model in WhisperModel::ALL {
            std::fs::write(dir.path().join(model.file_name()), []).unwrap();
        }
        let original = dir.path().join(WhisperModel::LargeV3.file_name());
        let mut state = Status::new(original.clone());
        state.request(WhisperModel::LargeV3Turbo);
        for busy in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            assert!(state.begin_switch(busy.0, busy.1, busy.2).is_none());
            assert_eq!(state.selected, original);
        }
        state.request(WhisperModel::LargeV3);
        assert!(state.begin_switch(false, false, false).is_none());
        state.request(WhisperModel::LargeV3Turbo);
        let next = state.begin_switch(false, false, false).unwrap();
        assert_eq!(state.selected, original);
        state.complete(next.clone(), false);
        assert_eq!(state.selected, original);
        state.request(WhisperModel::LargeV3Turbo);
        state.complete(next.clone(), true);
        assert_eq!(state.selected, next);
        assert!(!state.error);
    }

    #[test]
    fn missing_models_are_rejected_and_custom_models_have_no_false_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = Status::new(dir.path().join("custom.bin"));
        assert_eq!(state.label(), "Vlastní model");
        state.request(WhisperModel::LargeV3Turbo);
        assert!(state.error);
        assert!(state.pending.is_none());
        assert!(WhisperModel::from_path(&state.selected).is_none());
    }

    #[test]
    fn persistence_preserves_comments_other_settings_and_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("custom.toml");
        std::fs::write(&file, "# Keep this\n[dictation]\nmodel_path = 'old' # local\nlanguage = 'cs'\n[notifications]\nenabled = true\n").unwrap();
        persist(&file, Path::new("/models/large.bin")).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("# Keep this"));
        assert!(text.contains("# local"));
        assert!(text.contains("language = 'cs'"));
        assert!(text.contains("[notifications]\nenabled = true"));
        assert_eq!(
            crate::config::AppConfig::load(Some(file.clone()))
                .unwrap()
                .dictation
                .model_path,
            Path::new("/models/large.bin")
        );
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn new_configuration_and_symlinks_work_but_invalid_configuration_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("new/config.toml");
        persist(&file, Path::new("large.bin")).unwrap();
        let link = dir.path().join("link.toml");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        persist(&link, Path::new("turbo.bin")).unwrap();
        assert!(link.is_symlink());
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains("turbo.bin")
        );
        std::fs::write(&file, "broken = [").unwrap();
        assert!(persist(&file, Path::new("large.bin")).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "broken = [");
        std::fs::write(&file, "dictation = 42").unwrap();
        assert!(persist(&file, Path::new("large.bin")).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "dictation = 42");
    }
}
