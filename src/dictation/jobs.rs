//! Own inference jobs, cancellation generations and deferred submission.
use super::pipeline::{Completed, Pipeline};
use crate::{config::DictationConfig, desktop::FocusTarget};
use anyhow::Result;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::task::{JoinError, JoinSet};
use zbus::Connection;

struct AbortOnDrop {
    flag: Arc<AtomicBool>,
    armed: bool,
}
impl AbortOnDrop {
    fn new(flag: Arc<AtomicBool>) -> Self {
        Self { flag, armed: true }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.flag.store(true, Ordering::SeqCst);
        }
    }
}

pub fn completion_is_current(active: Option<u64>, completed: u64, cancelled: bool) -> bool {
    active == Some(completed) && !cancelled
}

struct ActiveJob {
    id: u64,
    cancelled: Arc<AtomicBool>,
    submit_requested: bool,
}

impl ActiveJob {
    fn new(id: u64, cancelled: Arc<AtomicBool>, submit_requested: bool) -> Self {
        Self {
            id,
            cancelled,
            submit_requested,
        }
    }

    fn request_submit(&mut self) {
        self.submit_requested = true;
    }

    fn accepts(&self, completed: u64) -> bool {
        completion_is_current(
            Some(self.id),
            completed,
            self.cancelled.load(Ordering::SeqCst),
        )
    }
}

pub(super) enum Completion {
    Pending,
    Stale,
    Finished(Result<Completed>),
}
pub(super) struct Jobs {
    active: Option<ActiveJob>,
    generation: u64,
    tasks: JoinSet<(u64, Result<Completed>)>,
    pipeline: Pipeline,
    connection: Connection,
}
impl Jobs {
    pub fn new(config: DictationConfig, connection: Connection) -> Self {
        Self {
            active: None,
            generation: 0,
            tasks: JoinSet::new(),
            pipeline: Pipeline::new(config, connection.clone()),
            connection,
        }
    }
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }
    pub fn has_pending(&self) -> bool {
        !self.tasks.is_empty()
    }
    pub fn request_submit(&mut self) {
        if let Some(job) = &mut self.active {
            job.request_submit();
            tracing::info!(
                job = job.id,
                "Top side button queued current dictation submission"
            );
        }
    }
    pub fn cancel(&mut self) {
        if let Some(job) = self.active.take() {
            job.cancelled.store(true, Ordering::SeqCst);
        }
    }
    pub fn start(&mut self, pcm: Vec<i16>, duration: i64, target: FocusTarget, submit: bool) {
        self.generation += 1;
        let id = self.generation;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.active = Some(ActiveJob::new(id, cancelled.clone(), submit));
        let pipeline = self.pipeline.clone();
        self.tasks.spawn(async move {
            let mut abort = AbortOnDrop::new(cancelled.clone());
            let result = pipeline.complete(pcm, duration, target, cancelled).await;
            abort.disarm();
            (id, result)
        });
    }
    pub async fn next(&mut self) -> Option<Result<(u64, Result<Completed>), JoinError>> {
        self.tasks.join_next().await
    }
    pub fn completed(&mut self, result: Result<(u64, Result<Completed>), JoinError>) -> Completion {
        let (id, result) = match result {
            Ok(value) => value,
            Err(error) => {
                self.cancel();
                return Completion::Finished(Err(error.into()));
            }
        };
        let Some(job) = self.active.as_ref().filter(|job| job.accepts(id)) else {
            return Completion::Stale;
        };
        if let Ok(mut completed) = result {
            if job.submit_requested
                && let Some(receipt) = completed.receipt.take()
            {
                let cancelled = job.cancelled.clone();
                let connection = self.connection.clone();
                self.tasks.spawn(async move {
                    let mut abort = AbortOnDrop::new(cancelled.clone());
                    let result = receipt
                        .submit(&connection, &cancelled)
                        .await
                        .map(|()| completed);
                    abort.disarm();
                    (id, result)
                });
                return Completion::Pending;
            }
            self.active = None;
            Completion::Finished(Ok(completed))
        } else {
            self.active = None;
            Completion::Finished(result)
        }
    }
    pub async fn shutdown(mut self) {
        self.cancel();
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
    }
}
impl Drop for Jobs {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_submission_coalesces_and_cannot_cross_a_canceled_or_later_job() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut first = ActiveJob::new(7, cancelled.clone(), false);
        first.request_submit();
        first.request_submit();
        assert!(first.submit_requested);
        assert!(first.accepts(7));
        assert!(!first.accepts(8));
        cancelled.store(true, Ordering::SeqCst);
        assert!(!first.accepts(7));
        let next = ActiveJob::new(8, Arc::new(AtomicBool::new(false)), false);
        assert!(!next.submit_requested);
        assert!(!next.accepts(7));
        assert!(next.accepts(8));
    }
    #[test]
    fn canceled_and_stale_transcriptions_cannot_complete_a_new_session() {
        assert!(completion_is_current(Some(7), 7, false));
        assert!(!completion_is_current(Some(7), 7, true));
        assert!(!completion_is_current(None, 7, false));
        assert!(!completion_is_current(Some(8), 7, false));
    }

    #[test]
    fn shutdown_aborts_inference_but_normal_completion_does_not_cancel_it() {
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut guard = AbortOnDrop::new(cancelled.clone());
            guard.disarm();
        }
        assert!(!cancelled.load(Ordering::SeqCst));
        {
            let _guard = AbortOnDrop::new(cancelled.clone());
        }
        assert!(cancelled.load(Ordering::SeqCst));
    }
    async fn fixture() -> (crate::test_bus::Bus, Jobs) {
        let bus = crate::test_bus::Bus::new();
        let jobs = Jobs::new(DictationConfig::default(), bus.connection().await);
        (bus, jobs)
    }
    #[tokio::test]
    async fn stale_or_canceled_completions_cannot_clear_a_new_job_or_carry_submission() {
        let (_bus, mut jobs) = fixture().await;
        let cancelled = Arc::new(AtomicBool::new(false));
        jobs.active = Some(ActiveJob::new(8, cancelled.clone(), false));
        assert!(matches!(
            jobs.completed(Ok((
                7,
                Ok(Completed {
                    characters: 1,
                    receipt: None
                })
            ))),
            Completion::Stale
        ));
        assert!(jobs.is_active());
        jobs.request_submit();
        jobs.cancel();
        assert!(cancelled.load(Ordering::SeqCst));
        assert!(matches!(
            jobs.completed(Ok((
                8,
                Ok(Completed {
                    characters: 1,
                    receipt: None
                })
            ))),
            Completion::Stale
        ));
        jobs.request_submit();
        jobs.active = Some(ActiveJob::new(9, Arc::new(AtomicBool::new(false)), false));
        assert!(!jobs.active.as_ref().unwrap().submit_requested);
        assert!(matches!(
            jobs.completed(Ok((
                9,
                Ok(Completed {
                    characters: 0,
                    receipt: None
                })
            ))),
            Completion::Finished(Ok(_))
        ));
        assert!(!jobs.is_active());
        jobs.shutdown().await;
    }
    #[tokio::test]
    async fn dropping_the_job_owner_cancels_inference_even_before_an_async_task_finishes() {
        let (_bus, mut jobs) = fixture().await;
        let cancelled = Arc::new(AtomicBool::new(false));
        jobs.active = Some(ActiveJob::new(1, cancelled.clone(), false));
        drop(jobs);
        assert!(cancelled.load(Ordering::SeqCst));
    }
}
