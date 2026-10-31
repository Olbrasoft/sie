//! Public dictation commands and status; the actor owns session transitions.
mod actor;
mod jobs;
mod pipeline;
pub use actor::run;
pub use jobs::completion_is_current;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Toggle,
    FinishAndSubmit,
    Cancel,
    SetEnabled(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Disabled,
    Idle,
    Recording,
    Transcribing,
    Error,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub enabled: bool,
    pub phase: Phase,
    pub message: String,
}

impl Status {
    pub fn new(enabled: bool, phase: Phase) -> Self {
        Self {
            enabled,
            phase,
            message: match phase {
                Phase::Disabled => "Diktování vypnuté",
                Phase::Idle => "Diktování připravené",
                Phase::Recording => "🔴 Nahrávám diktování",
                Phase::Transcribing => "Přepisuji lokálně…",
                Phase::Error => "Diktování nedostupné",
            }
            .into(),
        }
    }
}
