//! Command-line entry point for Moli tooling.
//!
//! This binary wires the `moli` support crate and the public
//! `moli` facade into CLI commands such as page fetches and the embedded
//! protocol server.

use anyhow::Result;

#[cfg(all(feature = "jemalloc", not(target_os = "windows")))]
mod allocator;

fn main() -> Result<()> {
    moli_process_signal::install_immediate_exit_handlers()?;
    let runtime_thread_budget = moli::runtime_thread_budget::tokio_runtime_thread_budget();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(runtime_thread_budget)
        .max_blocking_threads(runtime_thread_budget)
        .enable_all()
        .build()?
        .block_on(moli::app::run_from_env())
}
