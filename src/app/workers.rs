//! Own critical workers through startup errors and bounded graceful shutdown.
use std::{future::Future, time::Duration};
use tokio::task::JoinHandle;

pub(super) struct Worker {
    task: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn spawn(future: impl Future<Output = ()> + Send + 'static) -> Self {
        Self {
            task: Some(tokio::spawn(future)),
        }
    }
    async fn shutdown(mut self, timeout: Duration) {
        if let Some(task) = &mut self.task
            && tokio::time::timeout(timeout, &mut *task).await.is_err()
        {
            task.abort();
            let _ = task.await;
        }
        self.task.take();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
pub(super) async fn shutdown(worker: Option<Worker>, timeout: Duration) {
    if let Some(worker) = worker {
        worker.shutdown(timeout).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;
    struct Cleanup(Option<oneshot::Sender<()>>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
        }
    }
    #[tokio::test]
    async fn startup_drop_graceful_stop_and_deadline_all_release_owned_worker_resources() {
        for mode in 0..3 {
            let (ready, started) = oneshot::channel();
            let (finish, finished) = oneshot::channel();
            let (stop, stopped) = oneshot::channel();
            let worker = Worker::spawn(async move {
                let _cleanup = Cleanup(Some(finish));
                let _ = ready.send(());
                let _ = stopped.await;
            });
            started.await.unwrap();
            match mode {
                0 => drop(worker),
                1 => {
                    stop.send(()).unwrap();
                    worker.shutdown(Duration::from_secs(1)).await;
                }
                _ => worker.shutdown(Duration::ZERO).await,
            }
            tokio::time::timeout(Duration::from_secs(1), finished)
                .await
                .unwrap()
                .unwrap();
        }
    }
}
