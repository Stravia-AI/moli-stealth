use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, OnceLock},
    thread,
};

use tokio::sync::{mpsc, oneshot};

const PROTOCOL_LOCAL_EXECUTOR_STACK_SIZE: usize = 16 * 1024 * 1024;

type ProtocolLocalAdmission = Box<dyn FnOnce() + Send + 'static>;

/// Process-level sequence for protocol actors that contain `!Send` futures.
///
/// CDP, BiDi, and Classic state remain separate actor tasks. They share one OS
/// thread only because their schedulers use `spawn_local`; an actor that awaits
/// Page, socket, or timer progress yields this sequence to every other actor.
/// Page JavaScript and renderer work continue on their existing owner lanes.
struct ProtocolLocalExecutor {
    admission_tx: mpsc::UnboundedSender<ProtocolLocalAdmission>,
}

impl ProtocolLocalExecutor {
    fn shared() -> Result<&'static Self, Arc<str>> {
        static EXECUTOR: OnceLock<Result<ProtocolLocalExecutor, Arc<str>>> = OnceLock::new();
        match EXECUTOR.get_or_init(Self::start) {
            Ok(executor) => Ok(executor),
            Err(error) => Err(Arc::clone(error)),
        }
    }

    fn start() -> Result<Self, Arc<str>> {
        let (admission_tx, admission_rx) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        thread::Builder::new()
            .name("lm-protocol-seq".to_owned())
            // Keep the previously validated protocol stack budget, but reserve
            // it once per process instead of once per CDP/BiDi/Classic actor.
            .stack_size(PROTOCOL_LOCAL_EXECUTOR_STACK_SIZE)
            .spawn(move || {
                let mut runtime_builder = tokio::runtime::Builder::new_current_thread();
                runtime_builder
                    .max_blocking_threads(
                        crate::runtime_thread_budget::tokio_runtime_thread_budget(),
                    )
                    .enable_all();
                let runtime = match runtime_builder.build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!(
                            "failed to build protocol local runtime: {error}"
                        )));
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                let _ = ready_tx.send(Ok(()));
                local.block_on(&runtime, run_protocol_local_executor(admission_rx));
            })
            .map_err(|error| {
                Arc::from(format!("failed to spawn protocol local executor: {error}"))
            })?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { admission_tx }),
            Ok(Err(error)) => Err(Arc::from(error)),
            Err(error) => Err(Arc::from(format!(
                "protocol local executor stopped during startup: {error}"
            ))),
        }
    }
}

async fn run_protocol_local_executor(
    mut admission_rx: mpsc::UnboundedReceiver<ProtocolLocalAdmission>,
) {
    while let Some(admission) = admission_rx.recv().await {
        if catch_unwind(AssertUnwindSafe(admission)).is_err() {
            tracing::error!("protocol local task factory panicked");
        }
    }
}

/// Starts one independently-owned protocol actor on the shared local sequence.
///
/// `factory` is `Send` because it crosses from the HTTP/Tokio caller to the
/// protocol sequence. The future it creates intentionally need not be `Send`;
/// it is constructed and polled only after admission on `lm-protocol-seq`.
pub(super) fn spawn_protocol_local_task<T, F, Fut>(
    label: &'static str,
    factory: F,
) -> oneshot::Receiver<T>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + 'static,
{
    let (result_tx, result_rx) = oneshot::channel();
    let admission: ProtocolLocalAdmission = Box::new(move || {
        let actor = tokio::task::spawn_local(factory());
        tokio::task::spawn_local(async move {
            match actor.await {
                Ok(result) => {
                    let _ = result_tx.send(result);
                }
                Err(error) => {
                    tracing::error!(label, ?error, "protocol local actor failed");
                }
            }
        });
    });
    match ProtocolLocalExecutor::shared() {
        Ok(executor) => {
            if executor.admission_tx.send(admission).is_err() {
                tracing::error!(label, "protocol local executor is closed");
            }
        }
        Err(error) => {
            tracing::error!(label, %error, "protocol local executor is unavailable");
        }
    }
    result_rx
}

#[cfg(test)]
mod tests {
    use std::thread::ThreadId;

    use tokio::time::{Duration, timeout};

    use super::*;

    async fn task_thread_id() -> ThreadId {
        spawn_protocol_local_task("test-thread-id", || async { thread::current().id() })
            .await
            .expect("protocol local task should complete")
    }

    #[tokio::test]
    async fn independent_tasks_share_one_protocol_sequence() {
        let first = task_thread_id().await;
        let second = task_thread_id().await;

        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn suspended_actor_does_not_block_another_protocol_actor() {
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let suspended = spawn_protocol_local_task("test-suspended", move || async move {
            let thread_id = thread::current().id();
            let _ = started_tx.send(thread_id);
            let _ = release_rx.await;
            thread_id
        });
        let suspended_thread = started_rx.await.expect("suspended actor should start");

        let ready_thread = timeout(Duration::from_secs(1), task_thread_id())
            .await
            .expect("another actor should run while the first is suspended");
        assert_eq!(suspended_thread, ready_thread);

        let _ = release_tx.send(());
        assert_eq!(
            suspended.await.expect("suspended actor should finish"),
            suspended_thread
        );
    }

    #[tokio::test]
    async fn panicking_actor_does_not_retire_the_shared_sequence() {
        let failed = spawn_protocol_local_task("test-panic", || async {
            panic!("intentional protocol actor panic");
        });
        assert!(failed.await.is_err());

        timeout(Duration::from_secs(1), task_thread_id())
            .await
            .expect("shared sequence should survive one actor panic");
    }
}
