use std::{
    collections::VecDeque,
    fmt,
    num::NonZeroUsize,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{self, Receiver, Sender};
use curl::{
    easy::{Easy2, Handler},
    multi::{Easy2Handle, Multi, MultiWaker},
};
use parking_lot::Mutex;
use tracing::debug;

use crate::dns_adapter::{
    CurlDnsOwnerCompletion, CurlDnsOwnerResidence, CurlDnsReady, CurlDnsResolution,
};

const DEFAULT_RUNTIME_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Origin key used by the curl scheduler for per-origin active transfer caps.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CurlOriginKey {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
}

/// Configuration for a multi-request curl runtime.
#[derive(Debug, Clone)]
pub struct CurlMultiRuntimeConfig {
    pub max_active: NonZeroUsize,
    /// Scheduler-side per-origin active transfer cap.
    ///
    /// Keep this separate from `max_host_connections`: the latter is a curl
    /// transport connection-pool cap and should not throttle HTTP/2 streams.
    pub max_host_active: Option<NonZeroUsize>,
    /// libcurl per-host connection cap, matching Chromium's HTTP/1 socket-pool
    /// concept when configured by the higher fetch runtime.
    pub max_host_connections: Option<NonZeroUsize>,
    pub max_total_connections: Option<NonZeroUsize>,
    pub max_concurrent_streams: Option<NonZeroUsize>,
    pub poll_interval: Duration,
    pub multiplex: bool,
    pub thread_name: String,
}

impl Default for CurlMultiRuntimeConfig {
    fn default() -> Self {
        Self {
            max_active: NonZeroUsize::new(8).expect("default active transfer cap is non-zero"),
            max_host_active: None,
            max_host_connections: None,
            max_total_connections: None,
            max_concurrent_streams: None,
            poll_interval: DEFAULT_RUNTIME_POLL_INTERVAL,
            multiplex: true,
            thread_name: "lm-curl-multi".to_owned(),
        }
    }
}

impl CurlMultiRuntimeConfig {
    pub fn validate(&self) -> Result<()> {
        if self.poll_interval.is_zero() {
            return Err(anyhow!("curl multi runtime poll interval must be non-zero"));
        }
        if self.thread_name.is_empty() {
            return Err(anyhow!("curl multi runtime thread name must not be empty"));
        }
        Ok(())
    }
}

/// A configured curl transfer plus scheduler metadata.
pub struct CurlMultiJob<H: Handler, C> {
    pub easy: Easy2<H>,
    pub context: C,
    pub origin: Option<CurlOriginKey>,
    /// DNS ownership chosen by the caller before this transfer enters curl.
    ///
    /// A curl-managed policy preserves libcurl's resolver behavior. A shared
    /// origin policy parks the transfer outside the curl multi handle set until
    /// the bounded system resolver publishes an answer.
    pub dns_resolution: CurlDnsResolution,
    /// Higher values start before lower values when jobs are queued.
    pub priority: u8,
    pub label: String,
}

impl<H: Handler, C> fmt::Debug for CurlMultiJob<H, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CurlMultiJob")
            .field("origin", &self.origin)
            .field("dns_resolution", &self.dns_resolution)
            .field("priority", &self.priority)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// Completion emitted by `CurlMultiRuntime`.
pub struct CurlMultiCompletion<H: Handler, C> {
    pub easy: Option<Easy2<H>>,
    pub context: C,
    pub result: Result<()>,
}

impl<H: Handler, C> fmt::Debug for CurlMultiCompletion<H, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CurlMultiCompletion")
            .field("has_easy", &self.easy.is_some())
            .field("result", &self.result.as_ref().map(|_| ()))
            .finish_non_exhaustive()
    }
}

/// Error returned when a job cannot be submitted and is returned to the caller.
pub struct CurlSubmitError<H: Handler, C> {
    pub job: CurlMultiJob<H, C>,
    pub error: anyhow::Error,
}

impl<H: Handler, C> fmt::Debug for CurlSubmitError<H, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CurlSubmitError")
            .field("job", &self.job)
            .field("error", &self.error)
            .finish()
    }
}

/// Cloneable handle for a single libcurl multi owner thread.
#[derive(Debug)]
pub struct CurlMultiRuntime<H: Handler + Send + 'static, C: Send + 'static> {
    inner: Arc<CurlMultiRuntimeInner<H, C>>,
}

impl<H: Handler + Send + 'static, C: Send + 'static> Clone for CurlMultiRuntime<H, C> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[derive(Debug)]
struct CurlMultiRuntimeInner<H: Handler + Send + 'static, C: Send + 'static> {
    command_tx: Sender<CurlRuntimeCommand<H, C>>,
    owner_waker: MultiWaker,
    shutdown_requested: Arc<AtomicBool>,
    #[cfg(test)]
    owner_started: Arc<AtomicBool>,
    owner_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

#[derive(Debug)]
enum CurlRuntimeCommand<H: Handler, C> {
    Request(CurlMultiJob<H, C>),
    Shutdown,
}

enum CurlOwnerEvent<H: Handler, C> {
    Command(std::result::Result<CurlRuntimeCommand<H, C>, crossbeam_channel::RecvError>),
    Dns(std::result::Result<CurlDnsOwnerCompletion, crossbeam_channel::RecvError>),
}

impl<H: Handler + Send + 'static, C: Send + 'static> CurlMultiRuntime<H, C> {
    pub fn new(
        config: CurlMultiRuntimeConfig,
    ) -> Result<(Self, Receiver<CurlMultiCompletion<H, C>>)> {
        config.validate()?;
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (completion_tx, completion_rx) = crossbeam_channel::unbounded();
        let (waker_tx, waker_rx) = crossbeam_channel::bounded(1);
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        let owner_started = Arc::new(AtomicBool::new(false));
        let owner = CurlRuntimeOwner {
            config,
            command_rx,
            completion_tx,
            waker_tx,
            shutdown_requested: Arc::clone(&shutdown_requested),
            #[cfg(test)]
            owner_started: Arc::clone(&owner_started),
        };
        let thread_name = owner.config.thread_name.clone();
        let owner_handle = thread::Builder::new()
            .name(thread_name)
            .spawn(move || owner.run())
            .context("failed to spawn curl multi runtime owner thread")?;
        let owner_waker = waker_rx
            .recv()
            .context("curl multi runtime owner did not publish a waker")?;
        let runtime = Self {
            inner: Arc::new(CurlMultiRuntimeInner {
                command_tx,
                owner_waker,
                shutdown_requested,
                #[cfg(test)]
                owner_started,
                owner_handle: Mutex::new(Some(owner_handle)),
            }),
        };
        Ok((runtime, completion_rx))
    }

    pub fn submit(
        &self,
        job: CurlMultiJob<H, C>,
    ) -> std::result::Result<(), CurlSubmitError<H, C>> {
        if self.inner.shutdown_requested.load(Ordering::SeqCst) {
            return Err(CurlSubmitError {
                job,
                error: anyhow!("curl multi runtime is shutting down"),
            });
        }
        match self.inner.command_tx.send(CurlRuntimeCommand::Request(job)) {
            Ok(()) => {
                let _ = self.inner.owner_waker.wakeup();
                Ok(())
            }
            Err(error) => {
                let CurlRuntimeCommand::Request(job) = error.into_inner() else {
                    unreachable!("submit only sends request commands");
                };
                Err(CurlSubmitError {
                    job,
                    error: anyhow!("curl multi runtime is shutting down"),
                })
            }
        }
    }

    pub fn shutdown(&self) {
        self.inner.shutdown();
    }

    #[cfg(test)]
    pub fn owner_count_for_testing(&self) -> usize {
        usize::from(self.inner.owner_started.load(Ordering::SeqCst))
    }
}

impl<H: Handler + Send + 'static, C: Send + 'static> Drop for CurlMultiRuntimeInner<H, C> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl<H: Handler + Send + 'static, C: Send + 'static> CurlMultiRuntimeInner<H, C> {
    fn shutdown(&self) {
        if self.shutdown_requested.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.command_tx.send(CurlRuntimeCommand::Shutdown);
        let _ = self.owner_waker.wakeup();
        let Some(owner_handle) = self.owner_handle.lock().take() else {
            return;
        };
        let _ = owner_handle.join();
    }
}

struct CurlRuntimeOwner<H: Handler + Send + 'static, C: Send + 'static> {
    config: CurlMultiRuntimeConfig,
    command_rx: Receiver<CurlRuntimeCommand<H, C>>,
    completion_tx: Sender<CurlMultiCompletion<H, C>>,
    waker_tx: Sender<MultiWaker>,
    shutdown_requested: Arc<AtomicBool>,
    #[cfg(test)]
    owner_started: Arc<AtomicBool>,
}

impl<H: Handler + Send + 'static, C: Send + 'static> CurlRuntimeOwner<H, C> {
    fn run(self) {
        #[cfg(test)]
        self.owner_started.store(true, Ordering::SeqCst);
        let mut multi = make_runtime_multi(&self.config);
        let _ = self.waker_tx.send(multi.waker());
        let mut state = CurlOwnerState::default();

        loop {
            self.drain_commands(&mut state, &mut multi);
            self.drain_dns_completions(&mut state);
            self.start_eligible_jobs(&mut state, &mut multi);
            self.process_completed_transfers(&mut state, &mut multi);

            if state.closed
                && state.pending.is_empty()
                && state.dns.is_empty()
                && state.active.is_empty()
            {
                return;
            }

            if state.active.is_empty() && state.pending.is_empty() {
                self.wait_for_next_owner_event(&mut state, &mut multi);
            } else if !state.active.is_empty() {
                self.wait_for_curl_progress(&multi);
            }
        }
    }

    fn drain_commands(&self, state: &mut CurlOwnerState<H, C>, multi: &mut Multi) {
        loop {
            match self.command_rx.try_recv() {
                Ok(command) => self.handle_command(state, multi, command),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.close(state, multi);
                    break;
                }
            }
        }
    }

    fn wait_for_next_owner_event(&self, state: &mut CurlOwnerState<H, C>, multi: &mut Multi) {
        if state.closed {
            return;
        }
        if state.dns.is_empty() {
            match self.command_rx.recv() {
                Ok(command) => self.handle_command(state, multi, command),
                Err(_) => self.close(state, multi),
            }
            return;
        }
        let event = crossbeam_channel::select! {
            recv(self.command_rx) -> command => CurlOwnerEvent::Command(command),
            recv(state.dns.completion_receiver()) -> completion => CurlOwnerEvent::Dns(completion),
        };
        match event {
            CurlOwnerEvent::Command(command) => match command {
                Ok(command) => self.handle_command(state, multi, command),
                Err(_) => self.close(state, multi),
            },
            CurlOwnerEvent::Dns(completion) => {
                if let Ok(completion) = completion {
                    self.claim_dns_completion(state, completion);
                }
            }
        }
    }

    fn handle_command(
        &self,
        state: &mut CurlOwnerState<H, C>,
        multi: &mut Multi,
        command: CurlRuntimeCommand<H, C>,
    ) {
        match command {
            CurlRuntimeCommand::Request(job) if state.closed => {
                self.send_completion(CurlMultiCompletion {
                    easy: Some(job.easy),
                    context: job.context,
                    result: Err(anyhow!("curl multi runtime is shutting down")),
                });
            }
            CurlRuntimeCommand::Request(job) => enqueue_pending_job(&mut state.pending, job),
            CurlRuntimeCommand::Shutdown => self.close(state, multi),
        }
    }

    fn close(&self, state: &mut CurlOwnerState<H, C>, multi: &mut Multi) {
        if state.closed {
            return;
        }
        state.closed = true;
        self.shutdown_requested.store(true, Ordering::SeqCst);
        while let Some(pending) = state.pending.pop_front() {
            let job = pending.job;
            self.send_completion(CurlMultiCompletion {
                easy: Some(job.easy),
                context: job.context,
                result: Err(anyhow!("curl multi runtime is shutting down")),
            });
        }
        for pending in state.dns.drain() {
            let job = pending.job;
            self.send_completion(CurlMultiCompletion {
                easy: Some(job.easy),
                context: job.context,
                result: Err(anyhow!(
                    "curl multi runtime DNS request cancelled during shutdown"
                )),
            });
        }
        while let Some(active) = state.active.pop() {
            let easy = multi.remove2(active.handle).ok();
            self.send_completion(CurlMultiCompletion {
                easy,
                context: active.context,
                result: Err(anyhow!(
                    "curl multi runtime request cancelled during shutdown"
                )),
            });
        }
    }

    fn start_eligible_jobs(&self, state: &mut CurlOwnerState<H, C>, multi: &mut Multi) {
        loop {
            if state.closed || state.active.len() >= self.config.max_active.get() {
                return;
            }
            let Some(index) = state.pending.iter().position(|pending| {
                job_is_eligible(
                    pending.job.origin.as_ref(),
                    state,
                    self.config.max_host_active,
                )
            }) else {
                return;
            };
            let pending = state
                .pending
                .remove(index)
                .expect("pending curl job index should exist");
            let dns_target = pending.job.dns_resolution.target().cloned();
            match dns_target {
                Some(target) => state.dns.start(pending, target, multi.waker()),
                None => self.start_job(state, multi, pending),
            }
        }
    }

    fn drain_dns_completions(&self, state: &mut CurlOwnerState<H, C>) {
        while let Some(ready) = state.dns.try_claim_next() {
            self.handle_dns_completion(state, ready);
        }
    }

    fn claim_dns_completion(
        &self,
        state: &mut CurlOwnerState<H, C>,
        completion: CurlDnsOwnerCompletion,
    ) {
        let Some(ready) = state.dns.claim(completion) else {
            return;
        };
        self.handle_dns_completion(state, ready);
    }

    fn handle_dns_completion(
        &self,
        state: &mut CurlOwnerState<H, C>,
        ready: CurlDnsReady<CurlPendingJob<H, C>>,
    ) {
        let mut pending = ready.pending;
        if state.closed {
            let job = pending.job;
            self.send_completion(CurlMultiCompletion {
                easy: Some(job.easy),
                context: job.context,
                result: Err(anyhow!("curl multi runtime is shutting down")),
            });
            return;
        }
        match ready.result {
            Ok(addresses) => {
                if let Err(error) = pending
                    .job
                    .dns_resolution
                    .install(&mut pending.job.easy, addresses.as_ref())
                {
                    let job = pending.job;
                    self.send_completion(CurlMultiCompletion {
                        easy: Some(job.easy),
                        context: job.context,
                        result: Err(error),
                    });
                    return;
                }
                enqueue_existing_pending_job(&mut state.pending, pending);
            }
            Err(error) => {
                let job = pending.job;
                self.send_completion(CurlMultiCompletion {
                    easy: Some(job.easy),
                    context: job.context,
                    result: Err(anyhow!(error.to_string())),
                });
            }
        }
    }

    fn start_job(
        &self,
        state: &mut CurlOwnerState<H, C>,
        multi: &mut Multi,
        pending: CurlPendingJob<H, C>,
    ) {
        let queued_for = pending.enqueued_at.elapsed();
        let job = pending.job;
        let label = job.label.clone();
        match multi
            .add2(job.easy)
            .with_context(|| anyhow!("failed to add curl easy handle for {label}"))
        {
            Ok(handle) => {
                if curl_runtime_trace_enabled() {
                    let origin = job.origin.as_ref();
                    tracing::info!(
                        target: "moli_cdp_nav_timing",
                        label = %label,
                        origin_scheme = origin.map(|origin| origin.scheme.as_str()).unwrap_or(""),
                        origin_host = origin.map(|origin| origin.host.as_str()).unwrap_or(""),
                        origin_port = ?origin.and_then(|origin| origin.port),
                        origin = ?job.origin,
                        priority = job.priority,
                        queued_ms = queued_for.as_millis(),
                        active_before = state.active.len(),
                        active_same_origin_before = origin
                            .map(|origin| active_origin_count(state.active.as_slice(), origin))
                            .unwrap_or(0),
                        pending_after = state.pending.len(),
                        pending_same_origin_after = origin
                            .map(|origin| pending_origin_count(&state.pending, origin))
                            .unwrap_or(0),
                        max_active = self.config.max_active.get(),
                        max_host_active = ?self.config.max_host_active.map(NonZeroUsize::get),
                        max_host_connections = ?self.config.max_host_connections.map(NonZeroUsize::get),
                        max_total_connections = ?self.config.max_total_connections.map(NonZeroUsize::get),
                        max_concurrent_streams = ?self.config.max_concurrent_streams.map(NonZeroUsize::get),
                        multiplex = self.config.multiplex,
                        stage = "curl_runtime_job_start",
                    );
                }
                state.active.push(CurlActiveTransfer {
                    handle,
                    context: job.context,
                    origin: job.origin,
                    priority: job.priority,
                    label,
                    started_at: Instant::now(),
                    queued_for,
                });
            }
            Err(error) => self.send_completion(CurlMultiCompletion {
                easy: None,
                context: job.context,
                result: Err(error),
            }),
        }
    }

    fn process_completed_transfers(&self, state: &mut CurlOwnerState<H, C>, multi: &mut Multi) {
        if let Err(error) = multi.perform() {
            debug!("curl multi runtime perform failed: {error}");
        }
        let completed = completed_transfer_indices(multi, &state.active);
        for (active, result) in take_completed_in_notification_order(&mut state.active, completed) {
            let easy = match multi.remove2(active.handle) {
                Ok(easy) => Some(easy),
                Err(error) => {
                    self.send_completion(CurlMultiCompletion {
                        easy: None,
                        context: active.context,
                        result: Err(anyhow!(
                            "failed to remove curl easy handle for {}: {error}",
                            active.label
                        )),
                    });
                    continue;
                }
            };
            if curl_runtime_trace_enabled() {
                let origin = active.origin.as_ref();
                match &result {
                    Ok(()) => {
                        tracing::info!(
                            target: "moli_cdp_nav_timing",
                            label = %active.label,
                            origin_scheme = origin.map(|origin| origin.scheme.as_str()).unwrap_or(""),
                            origin_host = origin.map(|origin| origin.host.as_str()).unwrap_or(""),
                            origin_port = ?origin.and_then(|origin| origin.port),
                            origin = ?active.origin,
                            priority = active.priority,
                            ok = true,
                            active_ms = active.started_at.elapsed().as_millis(),
                            queued_ms = active.queued_for.as_millis(),
                            active_remaining = state.active.len(),
                            active_same_origin_remaining = origin
                                .map(|origin| active_origin_count(state.active.as_slice(), origin))
                                .unwrap_or(0),
                            pending_after = state.pending.len(),
                            pending_same_origin_after = origin
                                .map(|origin| pending_origin_count(&state.pending, origin))
                                .unwrap_or(0),
                            stage = "curl_runtime_job_done",
                        );
                    }
                    Err(error) => {
                        tracing::info!(
                            target: "moli_cdp_nav_timing",
                            label = %active.label,
                            origin_scheme = origin.map(|origin| origin.scheme.as_str()).unwrap_or(""),
                            origin_host = origin.map(|origin| origin.host.as_str()).unwrap_or(""),
                            origin_port = ?origin.and_then(|origin| origin.port),
                            origin = ?active.origin,
                            priority = active.priority,
                            ok = false,
                            error = %error,
                            active_ms = active.started_at.elapsed().as_millis(),
                            queued_ms = active.queued_for.as_millis(),
                            active_remaining = state.active.len(),
                            active_same_origin_remaining = origin
                                .map(|origin| active_origin_count(state.active.as_slice(), origin))
                                .unwrap_or(0),
                            pending_after = state.pending.len(),
                            pending_same_origin_after = origin
                                .map(|origin| pending_origin_count(&state.pending, origin))
                                .unwrap_or(0),
                            stage = "curl_runtime_job_done",
                        );
                    }
                }
            }
            let result = result.with_context(|| {
                anyhow!(
                    "curl request failed for {} after active={}ms queued={}ms",
                    active.label,
                    active.started_at.elapsed().as_millis(),
                    active.queued_for.as_millis()
                )
            });
            self.send_completion(CurlMultiCompletion {
                easy,
                context: active.context,
                result,
            });
        }
    }

    fn wait_for_curl_progress(&self, multi: &Multi) {
        let wait_timeout = runtime_wait_timeout(multi, self.config.poll_interval)
            .unwrap_or(self.config.poll_interval);
        if wait_timeout.is_zero() {
            return;
        }
        if let Err(error) = multi.poll(&mut [], wait_timeout) {
            debug!("curl multi runtime poll failed: {error}");
        }
    }

    fn send_completion(&self, completion: CurlMultiCompletion<H, C>) {
        let _ = self.completion_tx.send(completion);
    }
}

struct CurlOwnerState<H: Handler, C> {
    closed: bool,
    pending: VecDeque<CurlPendingJob<H, C>>,
    dns: CurlDnsOwnerResidence<CurlPendingJob<H, C>>,
    active: Vec<CurlActiveTransfer<H, C>>,
}

impl<H: Handler, C> Default for CurlOwnerState<H, C> {
    fn default() -> Self {
        Self {
            closed: false,
            pending: VecDeque::new(),
            dns: CurlDnsOwnerResidence::default(),
            active: Vec::new(),
        }
    }
}

struct CurlActiveTransfer<H: Handler, C> {
    handle: Easy2Handle<H>,
    context: C,
    origin: Option<CurlOriginKey>,
    priority: u8,
    label: String,
    started_at: Instant,
    queued_for: Duration,
}

struct CurlPendingJob<H: Handler, C> {
    job: CurlMultiJob<H, C>,
    enqueued_at: Instant,
}

fn enqueue_pending_job<H: Handler, C>(
    pending: &mut VecDeque<CurlPendingJob<H, C>>,
    job: CurlMultiJob<H, C>,
) {
    if curl_runtime_trace_enabled() {
        let origin = job.origin.as_ref();
        tracing::info!(
            target: "moli_cdp_nav_timing",
            label = %job.label,
            origin_scheme = origin.map(|origin| origin.scheme.as_str()).unwrap_or(""),
            origin_host = origin.map(|origin| origin.host.as_str()).unwrap_or(""),
            origin_port = ?origin.and_then(|origin| origin.port),
            origin = ?job.origin,
            priority = job.priority,
            pending_before = pending.len(),
            pending_same_origin_before = origin
                .map(|origin| pending_origin_count(pending, origin))
                .unwrap_or(0),
            stage = "curl_runtime_job_queued",
        );
    }
    let pending_job = CurlPendingJob {
        job,
        enqueued_at: Instant::now(),
    };
    if let Some(index) = pending
        .iter()
        .position(|queued| pending_job.job.priority > queued.job.priority)
    {
        pending.insert(index, pending_job);
    } else {
        pending.push_back(pending_job);
    }
}

fn enqueue_existing_pending_job<H: Handler, C>(
    pending: &mut VecDeque<CurlPendingJob<H, C>>,
    pending_job: CurlPendingJob<H, C>,
) {
    if let Some(index) = pending
        .iter()
        .position(|queued| pending_job.job.priority > queued.job.priority)
    {
        pending.insert(index, pending_job);
    } else {
        pending.push_back(pending_job);
    }
}

fn curl_runtime_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        env_flag_enabled("MOLI_CDP_NAV_TIMING") || env_flag_enabled("MOLI_CURL_RUNTIME_TRACE")
    })
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        let value = value.trim();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

fn job_is_eligible<H: Handler, C>(
    origin: Option<&CurlOriginKey>,
    state: &CurlOwnerState<H, C>,
    max_active_per_host: Option<NonZeroUsize>,
) -> bool {
    match (origin, max_active_per_host) {
        (Some(origin), Some(limit)) => {
            active_origin_count(state.active.as_slice(), origin) < limit.get()
        }
        _ => true,
    }
}

fn active_origin_count<H: Handler, C>(
    active: &[CurlActiveTransfer<H, C>],
    origin: &CurlOriginKey,
) -> usize {
    active
        .iter()
        .filter(|active| active.origin.as_ref() == Some(origin))
        .count()
}

fn pending_origin_count<H: Handler, C>(
    pending: &VecDeque<CurlPendingJob<H, C>>,
    origin: &CurlOriginKey,
) -> usize {
    pending
        .iter()
        .filter(|pending| pending.job.origin.as_ref() == Some(origin))
        .count()
}

fn completed_transfer_indices<H: Handler, C>(
    multi: &Multi,
    active: &[CurlActiveTransfer<H, C>],
) -> Vec<(usize, std::result::Result<(), curl::Error>)> {
    let mut completed = Vec::new();
    multi.messages(|message| {
        for (index, transfer) in active.iter().enumerate() {
            if message.is_for2(&transfer.handle) {
                if let Some(result) = message.result_for2(&transfer.handle) {
                    completed.push((index, result));
                }
                break;
            }
        }
    });
    completed
}

/// Removes a completion batch without exposing the active-vector removal
/// order to clients. `completed` is ordered exactly as libcurl reported its
/// completion messages, while its indices refer to `active` before any member
/// of the batch is removed.
fn take_completed_in_notification_order<T, E>(
    active: &mut Vec<T>,
    completed: Vec<(usize, E)>,
) -> Vec<(T, E)> {
    let original_active_len = active.len();
    let mut removed_indices = Vec::with_capacity(completed.len());
    completed
        .into_iter()
        .map(|(original_index, result)| {
            let removed_before = removed_indices
                .iter()
                .filter(|removed_index| **removed_index < original_index)
                .count();
            debug_assert!(original_index < original_active_len);
            debug_assert!(!removed_indices.contains(&original_index));
            let current_index = original_index - removed_before;
            removed_indices.push(original_index);
            (active.remove(current_index), result)
        })
        .collect()
}

fn make_runtime_multi(config: &CurlMultiRuntimeConfig) -> Multi {
    let mut multi = Multi::new();
    if let Some(max_host_connections) = config.max_host_connections
        && let Err(error) = multi.set_max_host_connections(max_host_connections.get())
    {
        debug!("failed to configure curl multi max_host_connections: {error}");
    }
    if let Some(max_total_connections) = config.max_total_connections {
        let max_total_connections = max_total_connections.get();
        if let Err(error) = multi.set_max_total_connections(max_total_connections) {
            debug!("failed to configure curl multi max_total_connections: {error}");
        }
        if let Err(error) = multi.set_max_connects(max_total_connections) {
            debug!("failed to configure curl multi max_connects: {error}");
        }
    }
    let max_concurrent_streams = config.max_concurrent_streams.map(NonZeroUsize::get);
    if let Some(max_concurrent_streams) = max_concurrent_streams
        && let Err(error) = multi.set_max_concurrent_streams(max_concurrent_streams)
    {
        debug!("failed to configure curl multi max_concurrent_streams: {error}");
    }
    if config.multiplex
        && let Err(error) = multi.pipelining(false, true)
    {
        debug!("failed to enable curl multi multiplexing: {error}");
    }
    multi
}

fn runtime_wait_timeout(multi: &Multi, poll_interval: Duration) -> Result<Duration> {
    let curl_timeout = multi
        .get_timeout()
        .context("failed to read curl multi timeout")?;
    Ok(curl_timeout
        .map(|timeout| timeout.min(poll_interval))
        .unwrap_or(poll_interval))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TestHandler;

    impl Handler for TestHandler {}

    fn test_job(
        label: &str,
        priority: u8,
        origin: Option<CurlOriginKey>,
    ) -> CurlMultiJob<TestHandler, String> {
        CurlMultiJob {
            easy: Easy2::new(TestHandler),
            context: label.to_owned(),
            origin,
            dns_resolution: CurlDnsResolution::curl_managed(),
            priority,
            label: label.to_owned(),
        }
    }

    fn test_origin(host: &str) -> CurlOriginKey {
        CurlOriginKey {
            scheme: "https".to_owned(),
            host: host.to_owned(),
            port: Some(443),
        }
    }

    #[test]
    fn runtime_config_rejects_zero_poll_interval() {
        let config = CurlMultiRuntimeConfig {
            poll_interval: Duration::ZERO,
            ..CurlMultiRuntimeConfig::default()
        };

        let error = config
            .validate()
            .expect_err("zero runtime poll interval should fail")
            .to_string();

        assert!(
            error.contains("poll interval must be non-zero"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn pending_jobs_are_ordered_by_priority() {
        let mut pending = VecDeque::new();

        enqueue_pending_job(&mut pending, test_job("auto-a", 1, None));
        enqueue_pending_job(&mut pending, test_job("low", 0, None));
        enqueue_pending_job(&mut pending, test_job("high-a", 2, None));
        enqueue_pending_job(&mut pending, test_job("auto-b", 1, None));
        enqueue_pending_job(&mut pending, test_job("high-b", 2, None));

        let labels = pending
            .iter()
            .map(|job| job.job.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, ["high-a", "high-b", "auto-a", "auto-b", "low"]);
    }

    #[test]
    fn completed_jobs_preserve_libcurl_notification_order() {
        let mut active = vec!["first", "second", "third", "fourth"];

        // libcurl reported the newest active entry first, followed by the
        // oldest. Vector-index removal must not reverse that observable order.
        let completed = take_completed_in_notification_order(
            &mut active,
            vec![(3, "fourth-result"), (0, "first-result")],
        );

        assert_eq!(
            completed,
            vec![("fourth", "fourth-result"), ("first", "first-result")]
        );
        assert_eq!(active, ["second", "third"]);
    }

    #[test]
    fn per_origin_cap_blocks_only_matching_origin() {
        let capped_origin = test_origin("example.test");
        let other_origin = test_origin("other.test");
        let multi = Multi::new();
        let active = CurlActiveTransfer {
            handle: multi
                .add2(Easy2::new(TestHandler))
                .expect("test handle should add to multi"),
            context: "active".to_owned(),
            origin: Some(capped_origin.clone()),
            priority: 1,
            label: "active".to_owned(),
            started_at: Instant::now(),
            queued_for: Duration::ZERO,
        };
        let state = CurlOwnerState {
            closed: false,
            pending: VecDeque::new(),
            dns: CurlDnsOwnerResidence::default(),
            active: vec![active],
        };
        let cap = NonZeroUsize::new(1);

        assert!(!job_is_eligible(Some(&capped_origin), &state, cap));
        assert!(job_is_eligible(Some(&other_origin), &state, cap));
    }
}
