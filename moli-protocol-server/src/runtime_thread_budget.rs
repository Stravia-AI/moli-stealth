//! Product thread budgets for Tokio runtimes owned by the CLI/protocol layer.
//!
//! Tokio's asynchronous workers and its blocking pool are separate budgets.
//! Capping only `worker_threads` still leaves `spawn_blocking` free to create a
//! large host-dependent burst, so every production runtime in this crate uses
//! the same CPU-aware upper bound for both kinds of worker.

const MAX_TOKIO_RUNTIME_THREADS: usize = 4;

/// Returns the product limit for both Tokio async and blocking workers.
pub fn tokio_runtime_thread_budget() -> usize {
    std::thread::available_parallelism()
        .map(tokio_runtime_thread_budget_for_parallelism)
        .unwrap_or(1)
}

fn tokio_runtime_thread_budget_for_parallelism(parallelism: std::num::NonZeroUsize) -> usize {
    parallelism.get().min(MAX_TOKIO_RUNTIME_THREADS)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::*;

    #[test]
    fn runtime_budget_tracks_small_hosts_and_caps_large_hosts() {
        assert_eq!(
            tokio_runtime_thread_budget_for_parallelism(NonZeroUsize::new(1).unwrap()),
            1
        );
        assert_eq!(
            tokio_runtime_thread_budget_for_parallelism(NonZeroUsize::new(3).unwrap()),
            3
        );
        assert_eq!(
            tokio_runtime_thread_budget_for_parallelism(NonZeroUsize::new(4).unwrap()),
            4
        );
        assert_eq!(
            tokio_runtime_thread_budget_for_parallelism(NonZeroUsize::new(32).unwrap()),
            4
        );
    }
}
