use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::Notify;

#[derive(Debug, Default)]
struct FetchLifecycleState {
    cancel_requested: AtomicBool,
    declared_body_complete: AtomicBool,
    terminal: AtomicBool,
    cancelled: Notify,
}

#[derive(Debug, Clone, Default)]
pub struct FetchCancelHandle {
    state: Arc<FetchLifecycleState>,
}

impl FetchCancelHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if !self.state.cancel_requested.swap(true, Ordering::SeqCst) {
            self.state.cancelled.notify_waiters();
        }
    }

    pub(crate) async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.state.cancelled.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.cancel_requested.load(Ordering::SeqCst)
    }

    /// Returns whether transport facts already determine this response's
    /// terminal result, so a later consumer cancellation must not replace it.
    pub fn response_completion_is_committed(&self) -> bool {
        self.state.declared_body_complete.load(Ordering::Acquire)
            || self.state.terminal.load(Ordering::Acquire)
    }

    pub(crate) fn reset_response_progress(&self) {
        self.state
            .declared_body_complete
            .store(false, Ordering::Release);
        self.state.terminal.store(false, Ordering::Release);
    }

    pub(crate) fn mark_declared_response_body_complete(&self) {
        self.state
            .declared_body_complete
            .store(true, Ordering::Release);
    }

    pub(crate) fn mark_response_terminal(&self) {
        self.state.terminal.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::FetchCancelHandle;

    #[test]
    fn response_completion_commit_is_shared_and_resettable() {
        let handle = FetchCancelHandle::new();
        let observer = handle.clone();
        assert!(!observer.response_completion_is_committed());

        handle.mark_declared_response_body_complete();
        assert!(observer.response_completion_is_committed());

        handle.reset_response_progress();
        assert!(!observer.response_completion_is_committed());

        handle.mark_response_terminal();
        assert!(observer.response_completion_is_committed());
    }
}
