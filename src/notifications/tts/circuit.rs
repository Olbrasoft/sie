//! In-memory provider cooldown policy, independent of provider transport.
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Circuit {
    failures: u32,
    pub(super) open_until: Option<Instant>,
    rounds: u32,
}

impl Circuit {
    pub(super) fn available(&self) -> bool {
        self.open_until.is_none_or(|at| at <= Instant::now())
    }
    pub(super) fn success(&mut self) {
        *self = Self::default();
    }
    pub(super) fn fail(&mut self) {
        self.failures += 1;
        if self.failures >= 3 {
            self.rounds = self.rounds.saturating_add(1).min(4);
            self.open_until = Some(
                Instant::now()
                    + Duration::from_secs((300u64 * 2u64.pow(self.rounds - 1)).min(3600)),
            );
            self.failures = 0;
        }
    }
}
