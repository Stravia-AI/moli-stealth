use std::{
    any::Any,
    backtrace::Backtrace,
    cell::RefCell,
    cmp::Ordering as CmpOrdering,
    collections::{BTreeSet, BinaryHeap, HashMap},
    fmt,
    io::Read,
    marker::PhantomData,
    pin::Pin,
    rc::Rc,
    sync::{
        Arc, Once,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use moli_cookie_jar::{
    SharedBrowserCookieStore, StoredCookieQueryReport, advance_cookie_request_context,
};
use moli_dns_resolver::{DnsCachePartition, DnsResolverService, DnsTarget};
use moli_stealth_net::{
    AuthScheme, ConnectionOptions, ResponseBody as TransportResponseBody, Transport, TransportAuth,
    TransportConfig, TransportError, TransportRequest, TransportResponse, process_fingerprint,
};
use moli_url_policy::ensure_http_network_transport_url;
use parking_lot::Mutex;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{Notify, mpsc, oneshot},
};
use url::Url;

use crate::{
    FetchCancelHandle, FetchConfig, NegotiatedHttpVersion, NetworkFetchFailureContext,
    NetworkFetchFailureRequestContext, NetworkRequestExtraInfo, NetworkResponseExtraInfo,
    RawResponse, RedirectInfo, Request, Response, ResponseHead, StreamingHtmlResponse,
    StreamingRawResponse,
    blocking::{
        CachedStreamingResponseLookup, StreamingHtmlResponseStart, TargetAddressResolution,
        cached_streaming_response_body_exceeds_response_limit, cached_streaming_response_is_stale,
        cookie_access_report_for_request, cookie_header_from_report,
        create_streaming_cache_body_writer_for_response_parts, finish_streaming_cached_response,
        load_cached_streaming_response_lookup, merge_cached_not_modified_streaming_response_lookup,
        network_request_extra_info_from_headers, next_followed_redirect_url_from_parts,
        outgoing_request_headers_for_url, remove_cached_response,
        response_headers_forbid_cache_storage, store_response_cookies, target_address_resolution,
        validate_allowed_target_ips, validation_headers_for_cached_streaming_response_lookup,
    },
    client_hints::{
        ClientHintResponseAction, SharedClientHintPreferences, SharedNavigationClientHintRestarts,
        prepare_client_hint_request,
    },
};

const DEFAULT_RUNTIME_TRANSFERS: usize = 256;
const STREAM_QUEUE_CHUNKS: usize = 8;
static NEXT_FETCH_RUNTIME_ID: AtomicU64 = AtomicU64::new(0);
static INSTALL_FETCH_RUNTIME_PANIC_HOOK: Once = Once::new();

thread_local! {
    static FETCH_RUNTIME_PANIC_CAPTURE: RefCell<Option<Arc<Mutex<Option<FetchRuntimePanicEvidence>>>>> =
        const { RefCell::new(None) };
}

#[derive(Clone, Debug)]
struct FetchRuntimePanicEvidence {
    location: Option<String>,
    backtrace: String,
}

fn install_fetch_runtime_panic_hook() {
    INSTALL_FETCH_RUNTIME_PANIC_HOOK.call_once(|| {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let _ = FETCH_RUNTIME_PANIC_CAPTURE.try_with(|capture| {
                let Ok(capture) = capture.try_borrow() else {
                    return;
                };
                let Some(capture) = capture.as_ref() else {
                    return;
                };
                *capture.lock() = Some(FetchRuntimePanicEvidence {
                    location: panic_info.location().map(|location| {
                        format!(
                            "{}:{}:{}",
                            location.file(),
                            location.line(),
                            location.column()
                        )
                    }),
                    backtrace: Backtrace::force_capture().to_string(),
                });
            });
            previous_hook(panic_info);
        }));
    });
}

struct FetchRuntimePanicCaptureGuard {
    previous: Option<Arc<Mutex<Option<FetchRuntimePanicEvidence>>>>,
}

impl FetchRuntimePanicCaptureGuard {
    fn enter(capture: Arc<Mutex<Option<FetchRuntimePanicEvidence>>>) -> Self {
        let previous = FETCH_RUNTIME_PANIC_CAPTURE.with(|active| active.replace(Some(capture)));
        Self { previous }
    }
}

impl Drop for FetchRuntimePanicCaptureGuard {
    fn drop(&mut self) {
        FETCH_RUNTIME_PANIC_CAPTURE.with(|active| active.replace(self.previous.take()));
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FetchRuntimeHandle {
    inner: Arc<FetchRuntimeInner>,
}

#[derive(Debug)]
struct FetchRuntimeInner {
    request_tx: mpsc::UnboundedSender<RuntimeCommand>,
    config: FetchConfig,
    tls_session_cache: moli_stealth_net::TlsSessionCache,
    shutdown_requested: Arc<AtomicBool>,
    #[cfg(test)]
    owner_started: Arc<AtomicBool>,
}

#[derive(Debug)]
pub(crate) struct FetchRuntimeOwner {
    handle: FetchRuntimeHandle,
    owner_thread: Option<thread::JoinHandle<()>>,
    identity: FetchRuntimeIdentity,
    panic_evidence: Arc<Mutex<Option<FetchRuntimePanicEvidence>>>,
    join_report: Option<FetchRuntimeJoinReport>,
    panic_logged: bool,
    #[cfg(test)]
    panic_log_count: Arc<std::sync::atomic::AtomicUsize>,
    _thread_affine: PhantomData<Rc<()>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchRuntimeIdentity {
    runtime_id: u64,
    thread_name: String,
    thread_id: String,
}

impl FetchRuntimeIdentity {
    pub fn runtime_id(&self) -> u64 {
        self.runtime_id
    }
    pub fn thread_name(&self) -> &str {
        &self.thread_name
    }
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchRuntimePanicReport {
    payload: String,
    location: Option<String>,
    backtrace: Option<String>,
}

impl FetchRuntimePanicReport {
    pub fn payload(&self) -> &str {
        &self.payload
    }
    pub fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }
    pub fn backtrace(&self) -> Option<&str> {
        self.backtrace.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FetchRuntimeJoinStatus {
    Clean,
    Panicked(FetchRuntimePanicReport),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchRuntimeJoinReport {
    identity: FetchRuntimeIdentity,
    status: FetchRuntimeJoinStatus,
}

impl FetchRuntimeJoinReport {
    pub fn identity(&self) -> &FetchRuntimeIdentity {
        &self.identity
    }
    pub fn status(&self) -> &FetchRuntimeJoinStatus {
        &self.status
    }
    pub fn is_clean(&self) -> bool {
        matches!(self.status, FetchRuntimeJoinStatus::Clean)
    }
    pub fn panic_report(&self) -> Option<&FetchRuntimePanicReport> {
        match &self.status {
            FetchRuntimeJoinStatus::Clean => None,
            FetchRuntimeJoinStatus::Panicked(report) => Some(report),
        }
    }
}

#[cfg(test)]
type RuntimeTextResponseTx = oneshot::Sender<Result<Response>>;
pub(crate) type RuntimeTextResponseCallback = Box<dyn FnOnce(Result<Response>) + Send + 'static>;
type RuntimeStreamingCompletionTx = oneshot::Sender<Result<()>>;

enum RuntimeResponseTx {
    #[cfg(test)]
    Text(RuntimeTextResponseTx),
    TextCallback(RuntimeTextResponseCallback),
}

impl fmt::Debug for RuntimeResponseTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            #[cfg(test)]
            Self::Text(_) => "RuntimeResponseTx::Text",
            Self::TextCallback(_) => "RuntimeResponseTx::TextCallback",
        })
    }
}

impl RuntimeResponseTx {
    fn send(self, response: Result<RawResponse>) {
        match self {
            #[cfg(test)]
            Self::Text(tx) => {
                let _ = tx.send(response.map(RawResponse::into_lossy_materialized_text_response));
            }
            Self::TextCallback(callback) => {
                callback(response.map(RawResponse::into_lossy_materialized_text_response))
            }
        }
    }
}

pub(crate) struct PendingStreamingHtmlResponse {
    started_rx: oneshot::Receiver<Result<StreamingHtmlResponseStart>>,
    body_rx: mpsc::Receiver<String>,
    cancel_handle: FetchCancelHandle,
    completion_rx: oneshot::Receiver<Result<()>>,
}

impl PendingStreamingHtmlResponse {
    pub(crate) async fn into_response(self) -> Result<StreamingHtmlResponse> {
        let started = self
            .started_rx
            .await
            .map_err(|_| anyhow!("streaming html start channel closed"))??;
        let extra = started.network_request_extra_info.clone();
        Ok(StreamingHtmlResponse::new_with_bounded_head(
            started.into_head(),
            self.body_rx,
            self.cancel_handle,
            self.completion_rx,
        )
        .with_network_request_extra_info(extra))
    }
}

pub struct PendingStreamingRawResponse {
    started_rx: oneshot::Receiver<Result<StreamingHtmlResponseStart>>,
    body_rx: mpsc::Receiver<Vec<u8>>,
    cancel_handle: FetchCancelHandle,
    completion_rx: oneshot::Receiver<Result<()>>,
}

impl PendingStreamingRawResponse {
    pub async fn into_response(self) -> Result<StreamingRawResponse> {
        let started = self
            .started_rx
            .await
            .map_err(|_| anyhow!("streaming raw start channel closed"))??;
        let extra = started.network_request_extra_info.clone();
        Ok(StreamingRawResponse::new_with_bounded_head(
            started.into_head(),
            self.body_rx,
            self.cancel_handle,
            self.completion_rx,
        )
        .with_network_request_extra_info(extra))
    }
}

enum RuntimeCommand {
    Request(Box<QueuedJob>),
    #[cfg(test)]
    PanicForTesting(std::sync::mpsc::Sender<()>),
    Shutdown,
}

enum Delivery {
    Buffered(RuntimeResponseTx),
    Html {
        started: Option<oneshot::Sender<Result<StreamingHtmlResponseStart>>>,
        body: mpsc::Sender<String>,
        utf8_pending: Vec<u8>,
        completion: Option<RuntimeStreamingCompletionTx>,
    },
    Raw {
        started: Option<oneshot::Sender<Result<StreamingHtmlResponseStart>>>,
        body: mpsc::Sender<Vec<u8>>,
        completion: Option<RuntimeStreamingCompletionTx>,
    },
}

struct QueuedJob {
    request: Request,
    cancel: FetchCancelHandle,
    delivery: Delivery,
    deadline: Option<Instant>,
    priority: u8,
    sequence: u64,
    origin: String,
    queue_watcher: Option<tokio::task::AbortHandle>,
    failure_url: Url,
    failure_redirects: Vec<RedirectInfo>,
}

#[derive(Clone, Copy)]
enum QueuedLifecycle {
    Cancelled,
    TimedOut,
}

struct QueuedLifecycleEvent {
    sequence: u64,
    lifecycle: QueuedLifecycle,
}

struct HeapJob(Box<QueuedJob>);
impl PartialEq for HeapJob {
    fn eq(&self, other: &Self) -> bool {
        self.0.sequence == other.0.sequence
    }
}
impl Eq for HeapJob {}
impl PartialOrd for HeapJob {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapJob {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.0
            .priority
            .cmp(&other.0.priority)
            .then_with(|| other.0.sequence.cmp(&self.0.sequence))
    }
}

#[derive(Clone)]
struct RuntimeShared {
    config: FetchConfig,
    cookie_store: SharedBrowserCookieStore,
    client_hint_preferences: SharedClientHintPreferences,
    transport: Transport,
    dns_partition: DnsCachePartition,
    shutdown: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
}

struct Scheduler {
    queued: BinaryHeap<HeapJob>,
    active: usize,
    active_by_origin: HashMap<String, usize>,
    max_active: usize,
    max_origin: Option<usize>,
}

impl Scheduler {
    fn new(config: &FetchConfig) -> Self {
        Self {
            queued: BinaryHeap::new(),
            active: 0,
            active_by_origin: HashMap::new(),
            max_active: config
                .http_max_concurrent()
                .map_or(DEFAULT_RUNTIME_TRANSFERS, |v| v.get() as usize),
            max_origin: config.http_max_host_open().map(|v| v.get() as usize),
        }
    }

    fn can_start(&self, origin: &str) -> bool {
        self.active < self.max_active
            && self
                .max_origin
                .is_none_or(|limit| self.active_by_origin.get(origin).copied().unwrap_or(0) < limit)
    }

    fn take_next(&mut self) -> Option<Box<QueuedJob>> {
        let mut deferred = Vec::new();
        let mut selected = None;
        while let Some(HeapJob(job)) = self.queued.pop() {
            if self.can_start(&job.origin) {
                selected = Some(job);
                break;
            }
            deferred.push(HeapJob(job));
        }
        self.queued.extend(deferred);
        selected
    }

    fn started(&mut self, origin: &str) {
        self.active += 1;
        *self.active_by_origin.entry(origin.to_owned()).or_default() += 1;
    }

    fn take_sequence(&mut self, sequence: u64) -> Option<Box<QueuedJob>> {
        let jobs = std::mem::take(&mut self.queued).into_vec();
        let mut selected = None;
        for HeapJob(job) in jobs {
            if job.sequence == sequence {
                selected = Some(job);
            } else {
                self.queued.push(HeapJob(job));
            }
        }
        selected
    }

    fn completed(&mut self, origin: &str) {
        self.active = self.active.saturating_sub(1);
        if let Some(count) = self.active_by_origin.get_mut(origin) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.active_by_origin.remove(origin);
            }
        }
    }
}

impl FetchRuntimeOwner {
    #[cfg(test)]
    pub(crate) fn new(config: &FetchConfig, cookie_store: SharedBrowserCookieStore) -> Self {
        Self::new_with_client_hint_preferences(
            config,
            cookie_store,
            Arc::new(Mutex::new(
                crate::client_hints::ClientHintPreferences::default(),
            )),
        )
    }

    pub(crate) fn new_with_client_hint_preferences(
        config: &FetchConfig,
        cookie_store: SharedBrowserCookieStore,
        client_hint_preferences: SharedClientHintPreferences,
    ) -> Self {
        let runtime_id = NEXT_FETCH_RUNTIME_ID
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let shutdown_notify = Arc::new(Notify::new());
        #[cfg(test)]
        let owner_started = Arc::new(AtomicBool::new(false));
        let transport = Transport::new(TransportConfig {
            fingerprint: process_fingerprint().clone(),
            tls_verify: config.tls_verify_host(),
            max_connections: config
                .http_max_total_connections()
                .filter(|limit| *limit > 0)
                .map(usize::from),
            max_host_connections: config
                .effective_http_max_host_connections()
                .map(usize::from),
            max_h2_streams: config
                .http2_max_concurrent_streams()
                .filter(|limit| *limit > 0)
                .map(usize::from),
        })
        .expect("failed to initialize fetch transport");
        let tls_session_cache = transport.tls_session_cache();
        let shared = RuntimeShared {
            config: config.clone(),
            cookie_store,
            client_hint_preferences,
            transport,
            dns_partition: DnsCachePartition::fresh(),
            shutdown: Arc::clone(&shutdown_requested),
            shutdown_notify,
        };
        install_fetch_runtime_panic_hook();
        let panic_evidence = Arc::new(Mutex::new(None));
        let thread_evidence = Arc::clone(&panic_evidence);
        #[cfg(test)]
        let thread_owner_started = Arc::clone(&owner_started);
        let owner_handle = thread::Builder::new()
            .name("lm-fetch-semantics".to_owned())
            .spawn(move || {
                let _panic_capture = FetchRuntimePanicCaptureGuard::enter(thread_evidence);
                #[cfg(test)]
                thread_owner_started.store(true, Ordering::SeqCst);
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to create fetch async runtime");
                let local = tokio::task::LocalSet::new();
                runtime.block_on(local.run_until(run_owner(shared, request_rx)));
            })
            .expect("failed to spawn fetch runtime semantic owner thread");
        let identity = FetchRuntimeIdentity {
            runtime_id,
            thread_name: owner_handle
                .thread()
                .name()
                .unwrap_or("unnamed-fetch-runtime")
                .to_owned(),
            thread_id: format!("{:?}", owner_handle.thread().id()),
        };
        Self {
            handle: FetchRuntimeHandle {
                inner: Arc::new(FetchRuntimeInner {
                    request_tx,
                    config: config.clone(),
                    tls_session_cache,
                    shutdown_requested,
                    #[cfg(test)]
                    owner_started,
                }),
            },
            owner_thread: Some(owner_handle),
            identity,
            panic_evidence,
            join_report: None,
            panic_logged: false,
            #[cfg(test)]
            panic_log_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            _thread_affine: PhantomData,
        }
    }

    pub(crate) fn handle(&self) -> FetchRuntimeHandle {
        self.handle.clone()
    }
    #[cfg(test)]
    pub(crate) fn shutdown(mut self) -> FetchRuntimeJoinReport {
        self.request_shutdown();
        self.join()
    }
    pub(crate) fn request_shutdown(&self) {
        self.handle.request_shutdown();
    }
    #[cfg(test)]
    pub(crate) fn panic_log_count_for_testing(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        Arc::clone(&self.panic_log_count)
    }

    pub(crate) fn join(&mut self) -> FetchRuntimeJoinReport {
        self.request_shutdown();
        if let Some(owner_thread) = self.owner_thread.take() {
            let status = match owner_thread.join() {
                Ok(()) => FetchRuntimeJoinStatus::Clean,
                Err(payload) => FetchRuntimeJoinStatus::Panicked(panic_report(
                    payload,
                    self.panic_evidence.lock().clone(),
                )),
            };
            self.join_report = Some(FetchRuntimeJoinReport {
                identity: self.identity.clone(),
                status,
            });
        }
        self.join_report
            .clone()
            .expect("a joined fetch runtime must retain its terminal report")
    }
}

impl std::ops::Deref for FetchRuntimeOwner {
    type Target = FetchRuntimeHandle;
    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}
impl Drop for FetchRuntimeOwner {
    fn drop(&mut self) {
        self.request_shutdown();
        let report = self.join();
        if let Some(panic) = report.panic_report()
            && !self.panic_logged
        {
            self.panic_logged = true;
            #[cfg(test)]
            self.panic_log_count.fetch_add(1, Ordering::SeqCst);
            tracing::error!(
                runtime_id = report.identity().runtime_id(),
                thread_name = report.identity().thread_name(),
                thread_id = report.identity().thread_id(),
                panic_payload = panic.payload(),
                panic_location = panic.location().unwrap_or("unknown"),
                panic_backtrace = panic.backtrace().unwrap_or("unavailable"),
                "fetch runtime semantic owner panicked while being joined"
            );
        }
    }
}

fn panic_report(
    payload: Box<dyn Any + Send + 'static>,
    evidence: Option<FetchRuntimePanicEvidence>,
) -> FetchRuntimePanicReport {
    let payload = if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    };
    let (location, backtrace) = evidence
        .map(|e| (e.location, Some(e.backtrace)))
        .unwrap_or((None, None));
    FetchRuntimePanicReport {
        payload,
        location,
        backtrace,
    }
}

impl FetchRuntimeHandle {
    pub(crate) fn tls_session_cache(&self) -> moli_stealth_net::TlsSessionCache {
        self.inner.tls_session_cache.clone()
    }

    #[cfg(test)]
    pub(crate) fn submit(&self, request: Request) -> Result<oneshot::Receiver<Result<Response>>> {
        self.submit_with_cancel(request, FetchCancelHandle::new())
    }

    #[cfg(test)]
    pub(crate) fn submit_with_cancel(
        &self,
        request: Request,
        cancel: FetchCancelHandle,
    ) -> Result<oneshot::Receiver<Result<Response>>> {
        let (tx, rx) = oneshot::channel();
        self.enqueue(
            request,
            cancel,
            Delivery::Buffered(RuntimeResponseTx::Text(tx)),
        )?;
        Ok(rx)
    }

    pub(crate) fn submit_with_cancel_callback(
        &self,
        request: Request,
        cancel: FetchCancelHandle,
        callback: RuntimeTextResponseCallback,
    ) -> Result<()> {
        self.enqueue(
            request,
            cancel,
            Delivery::Buffered(RuntimeResponseTx::TextCallback(callback)),
        )
    }

    pub(crate) fn submit_html_stream(
        &self,
        request: Request,
    ) -> Result<PendingStreamingHtmlResponse> {
        let (started_tx, started_rx) = oneshot::channel();
        let (body_tx, body_rx) = mpsc::channel(STREAM_QUEUE_CHUNKS);
        let (completion_tx, completion_rx) = oneshot::channel();
        let cancel = FetchCancelHandle::new();
        self.enqueue(
            request,
            cancel.clone(),
            Delivery::Html {
                started: Some(started_tx),
                body: body_tx,
                utf8_pending: Vec::new(),
                completion: Some(completion_tx),
            },
        )?;
        Ok(PendingStreamingHtmlResponse {
            started_rx,
            body_rx,
            cancel_handle: cancel,
            completion_rx,
        })
    }

    pub(crate) fn submit_raw_stream(
        &self,
        request: Request,
        cancel: FetchCancelHandle,
    ) -> Result<PendingStreamingRawResponse> {
        let (started_tx, started_rx) = oneshot::channel();
        let (body_tx, body_rx) = mpsc::channel(STREAM_QUEUE_CHUNKS);
        let (completion_tx, completion_rx) = oneshot::channel();
        self.enqueue(
            request,
            cancel.clone(),
            Delivery::Raw {
                started: Some(started_tx),
                body: body_tx,
                completion: Some(completion_tx),
            },
        )?;
        Ok(PendingStreamingRawResponse {
            started_rx,
            body_rx,
            cancel_handle: cancel,
            completion_rx,
        })
    }

    fn enqueue(
        &self,
        request: Request,
        cancel: FetchCancelHandle,
        delivery: Delivery,
    ) -> Result<()> {
        ensure_http_network_transport_url(&request.url)?;
        if self.inner.shutdown_requested.load(Ordering::SeqCst) {
            return Err(anyhow!("fetch runtime is shutting down"));
        }
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let timeout = request.effective_request_timeout(&self.inner.config);
        let deadline = (!timeout.is_zero()).then(|| Instant::now() + timeout);
        let failure_url = request.url.clone();
        let job = Box::new(QueuedJob {
            origin: origin_key_for_url(&request.url),
            priority: request_fetch_priority_rank(&request),
            sequence: SEQUENCE.fetch_add(1, Ordering::Relaxed),
            request,
            cancel,
            delivery,
            deadline,
            queue_watcher: None,
            failure_url,
            failure_redirects: Vec::new(),
        });
        self.inner
            .request_tx
            .send(RuntimeCommand::Request(job))
            .map_err(|_| anyhow!("fetch runtime is shutting down"))
    }

    #[cfg(test)]
    pub(crate) fn owner_count_for_testing(&self) -> usize {
        usize::from(self.inner.owner_started.load(Ordering::SeqCst))
    }
    #[cfg(test)]
    pub(crate) fn panic_owner_for_testing(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.inner
            .request_tx
            .send(RuntimeCommand::PanicForTesting(tx))
            .expect("fetch runtime owner should accept panic command");
        rx.recv()
            .expect("fetch runtime owner should admit panic command");
    }
    pub(crate) fn request_shutdown(&self) {
        if !self.inner.shutdown_requested.swap(true, Ordering::SeqCst) {
            let _ = self.inner.request_tx.send(RuntimeCommand::Shutdown);
        }
    }
}

async fn run_owner(shared: RuntimeShared, mut commands: mpsc::UnboundedReceiver<RuntimeCommand>) {
    let mut scheduler = Scheduler::new(&shared.config);
    let mut tasks = tokio::task::JoinSet::<String>::new();
    let mut queue_watchers = tokio::task::JoinSet::<QueuedLifecycleEvent>::new();
    let mut closed = false;
    loop {
        if !closed {
            while let Some(mut job) = scheduler.take_next() {
                if let Some(watcher) = job.queue_watcher.take() {
                    watcher.abort();
                }
                if job.cancel.is_cancelled() || job.deadline.is_some_and(|d| Instant::now() >= d) {
                    let error = if job.cancel.is_cancelled() {
                        cancellation_error("fetch runtime request cancelled")
                    } else {
                        anyhow::Error::new(TransportError::Timeout)
                    };
                    fail_delivery(
                        job.delivery,
                        network_fetch_failure_for_request(
                            &job.request,
                            &job.failure_url,
                            &job.failure_redirects,
                            error,
                        ),
                    );
                    continue;
                }
                let origin = job.origin.clone();
                scheduler.started(&origin);
                let task_shared = shared.clone();
                tasks.spawn_local(async move {
                    execute_job(task_shared, job).await;
                    origin
                });
            }
        }
        if closed && scheduler.active == 0 {
            queue_watchers.abort_all();
            break;
        }
        tokio::select! {
            command = commands.recv(), if !closed => match command {
                Some(RuntimeCommand::Request(mut job)) => {
                    let sequence = job.sequence;
                    let cancel = job.cancel.clone();
                    let deadline = job.deadline;
                    let watcher = queue_watchers.spawn_local(async move {
                        let lifecycle = if let Some(deadline) = deadline {
                            tokio::select! {
                                _ = cancel.cancelled() => QueuedLifecycle::Cancelled,
                                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => QueuedLifecycle::TimedOut,
                            }
                        } else {
                            cancel.cancelled().await;
                            QueuedLifecycle::Cancelled
                        };
                        QueuedLifecycleEvent { sequence, lifecycle }
                    });
                    job.queue_watcher = Some(watcher);
                    scheduler.queued.push(HeapJob(job));
                },
                #[cfg(test)]
                Some(RuntimeCommand::PanicForTesting(admitted)) => { let _ = admitted.send(()); panic!("deterministic fetch runtime panic"); }
                Some(RuntimeCommand::Shutdown) | None => {
                    closed = true;
                    shared.shutdown.store(true, Ordering::SeqCst);
                    shared.shutdown_notify.notify_waiters();
                    queue_watchers.abort_all();
                    while let Some(HeapJob(job)) = scheduler.queued.pop() {
                        fail_delivery(job.delivery, cancellation_error("fetch runtime request cancelled during shutdown"));
                    }
                }
            },
            Some(completed) = tasks.join_next(), if scheduler.active > 0 => match completed {
                Ok(origin) => scheduler.completed(&origin),
                Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
                Err(error) => panic!("fetch runtime task failed: {error}"),
            },
            Some(event) = queue_watchers.join_next(), if !queue_watchers.is_empty() => {
                let Ok(event) = event else { continue };
                if let Some(job) = scheduler.take_sequence(event.sequence) {
                    let error = match event.lifecycle {
                        QueuedLifecycle::Cancelled => cancellation_error("fetch runtime request cancelled"),
                        QueuedLifecycle::TimedOut => anyhow::Error::new(TransportError::Timeout),
                    };
                    fail_delivery(job.delivery, network_fetch_failure_for_request(
                        &job.request, &job.failure_url, &job.failure_redirects, error,
                    ));
                }
            }
        }
    }
}

async fn execute_job(shared: RuntimeShared, mut job: Box<QueuedJob>) {
    #[cfg(test)]
    if job.request.request_headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("x-moli-test-panic") && value == "runtime-worker"
    }) {
        let error = anyhow!(
            "fetch runtime owner panicked while handling {} {}: runtime owner panic requested by test",
            job.request.method(),
            job.request.url
        );
        fail_delivery(job.delivery, error);
        return;
    }
    let result = execute_request_with_lifecycle(&shared, &mut job).await;
    if let Err(error) = result {
        let error = network_fetch_failure_for_request(
            &job.request,
            &job.failure_url,
            &job.failure_redirects,
            error,
        );
        fail_delivery(job.delivery, error);
    }
}

async fn execute_request_with_lifecycle(shared: &RuntimeShared, job: &mut QueuedJob) -> Result<()> {
    let shutdown = shared.shutdown_notify.notified();
    tokio::pin!(shutdown);
    check_lifecycle(shared, job)?;
    let cancel = job.cancel.clone();
    let deadline = job.deadline;
    let future = execute_request(shared, job);
    tokio::pin!(future);
    let mut cancellation_enabled = true;

    loop {
        if let Some(deadline) = deadline {
            tokio::select! {
                result = &mut future => return result,
                _ = cancel.cancelled(), if cancellation_enabled => {},
                _ = &mut shutdown => return Err(cancellation_error("fetch runtime request cancelled during shutdown")),
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => return Err(anyhow::Error::new(TransportError::Timeout)),
            }
        } else {
            tokio::select! {
                result = &mut future => return result,
                _ = cancel.cancelled(), if cancellation_enabled => {},
                _ = &mut shutdown => return Err(cancellation_error("fetch runtime request cancelled during shutdown")),
            }
        }
        if cancel.response_completion_is_committed() {
            cancellation_enabled = false;
        } else {
            return Err(cancellation_error("fetch runtime request cancelled"));
        }
    }
}

async fn execute_request(shared: &RuntimeShared, job: &mut QueuedJob) -> Result<()> {
    let mut request = job.request.clone();
    let mut current_url = request.url.clone();
    let mut cookie_context = request.cookie_context.clone();
    let mut redirects = Vec::new();
    let mut redirect_count = 0;
    let mut http1_only = false;
    let mut empty_http_upgrade_attempted = false;
    let client_hint_restarts: SharedNavigationClientHintRestarts =
        Arc::new(Mutex::new(BTreeSet::new()));

    loop {
        job.failure_url = current_url.clone();
        job.failure_redirects = redirects.clone();
        check_lifecycle(shared, job)?;
        let credentials_allowed = request.allows_credentials_for_url(&current_url);
        let cookie_report = if credentials_allowed {
            cookie_access_report_for_request(
                &shared.cookie_store,
                &current_url,
                cookie_context.clone(),
            )?
        } else {
            None
        };
        let cookie_header = cookie_header_from_report(cookie_report.as_ref());
        let prepared = prepare_client_hint_request(
            &shared.client_hint_preferences,
            &client_hint_restarts,
            &shared.config,
            &request,
            &current_url,
        );
        let mut stale_cache = None;
        if let Some(cached) = load_cached_streaming_response_lookup(
            &shared.config,
            &prepared.request,
            &current_url,
            cookie_header.as_deref(),
        )? {
            if !cached_streaming_response_is_stale(&cached) {
                if prepared
                    .response_policy
                    .observe_response(&current_url, &cached.headers)
                    == ClientHintResponseAction::RestartNavigation
                {
                    continue;
                }
                if let Some(next) = next_followed_redirect_url_from_parts(
                    &current_url,
                    cached.status,
                    &cached.headers,
                    redirect_count,
                    request.follow_redirects,
                )? && request.follow_redirects
                {
                    redirects.push(redirect_info(
                        &current_url,
                        next.clone(),
                        cached.status,
                        cached.headers.clone(),
                        cookie_report,
                        Vec::new(),
                        true,
                        None,
                        None,
                    ));
                    cookie_context =
                        advance_cookie_request_context(cookie_context, &request.url, &next);
                    request.apply_redirect_status(cached.status);
                    current_url = next;
                    redirect_count += 1;
                    http1_only = false;
                    continue;
                }
                return deliver_cached(job, cached, cookie_report, redirects).await;
            }
            stale_cache = Some(cached);
        }

        let mut outgoing = outgoing_request_headers_for_url(
            &shared.config,
            &prepared.request,
            &current_url,
            &redirects,
            cookie_header.as_deref(),
        );
        if let Some(stale) = stale_cache.as_ref() {
            outgoing.extend(validation_headers_for_cached_streaming_response_lookup(
                stale,
            ));
        }
        if let Some(web_bot_auth) = shared.config.web_bot_auth() {
            web_bot_auth
                .append_request_headers(&mut outgoing, prepared.request.method(), &current_url)
                .with_context(|| {
                    format!("failed to sign web bot auth request for {current_url}")
                })?;
        }
        let options = connection_options(shared, job, &request, &current_url).await?;
        let auth = request
            .auth()
            .filter(|auth| matches!(auth.target, crate::RequestAuthTarget::Server))
            .map(transport_auth);
        let mut transport_request =
            TransportRequest::new(current_url.clone(), prepared.request.method().to_owned());
        transport_request.headers = outgoing;
        transport_request.body = prepared.request.body_bytes().map(<[u8]>::to_vec);
        transport_request.connection = options;
        transport_request.auth = auth;
        transport_request.http1_only = http1_only;
        transport_request.observer = request
            .network_observation_recorder()
            .map(|recorder| recorder.transport_observer(cookie_report.clone()));
        let fingerprint = process_fingerprint();
        if fingerprint.preset == moli_stealth_net::FingerprintPreset::Chrome152
            && fingerprint.h2.headers_priority.is_none()
        {
            let weight = if request.is_top_level_navigation_request() {
                255
            } else {
                match request_fetch_load_priority(&request) {
                    crate::ResourceLoadPriority::VeryLow => 0,
                    crate::ResourceLoadPriority::Low => 146,
                    crate::ResourceLoadPriority::Medium => 182,
                    crate::ResourceLoadPriority::High => 219,
                    crate::ResourceLoadPriority::VeryHigh => 255,
                }
            };
            transport_request.h2_priority = Some(moli_stealth_net::H2HeadersPriority {
                stream_dependency: 0,
                weight,
                exclusive: true,
            });
        }

        let response = match await_transport(shared, job, transport_request).await {
            Ok(response) => response,
            Err(error) => {
                if !empty_http_upgrade_attempted
                    && current_url.scheme() == "http"
                    && request.is_top_level_navigation_request()
                    && redirects.is_empty()
                    && request_is_replay_safe(&prepared.request)
                    && matches!(
                        error.downcast_ref::<TransportError>(),
                        Some(TransportError::EmptyResponse)
                    )
                {
                    let mut upgraded = current_url.clone();
                    upgraded
                        .set_scheme("https")
                        .map_err(|_| anyhow!("failed to construct HTTPS upgrade URL"))?;
                    redirects.push(RedirectInfo {
                        from_url: current_url.clone(),
                        to_url: upgraded.clone(),
                        status: 307,
                        headers: vec![(
                            "non-authoritative-reason".to_owned(),
                            "HttpsUpgrades".to_owned(),
                        )],
                        network_extra_info_available: false,
                        request_extra_info: None,
                        response_extra_info: None,
                        redirect_has_extra_info: false,
                        request_cookie_report: cookie_report,
                        cookie_set_reports: Vec::new(),
                        from_cache: false,
                        negotiated_http_version: None,
                    });
                    cookie_context =
                        advance_cookie_request_context(cookie_context, &request.url, &upgraded);
                    current_url = upgraded;
                    empty_http_upgrade_attempted = true;
                    http1_only = false;
                    continue;
                }
                if !http1_only
                    && matches!(
                        error.downcast_ref::<TransportError>(),
                        Some(TransportError::Http2(_))
                    )
                    && request_is_replay_safe(&prepared.request)
                {
                    http1_only = true;
                    continue;
                }
                if let Some(TransportError::ProxyConnect(proxy)) =
                    error.downcast_ref::<TransportError>()
                {
                    if let Some(recorder) = request.network_observation_recorder() {
                        recorder.record_failed_proxy_connect_terminal();
                    }
                    return deliver_proxy_failure(
                        job,
                        current_url.clone(),
                        proxy.clone(),
                        cookie_report,
                        redirects,
                    )
                    .await;
                }
                if empty_http_upgrade_attempted && redirects.len() == 1 {
                    // A failed HTTPS probe has not committed the synthetic redirect.
                    job.failure_url = redirects[0].from_url.clone();
                    job.failure_redirects.clear();
                }
                return Err(error);
            }
        };
        let request_extra_info = request.is_top_level_navigation_request().then(|| {
            network_request_extra_info_from_headers(
                &shared.config,
                &response.sent_headers,
                cookie_report.as_ref(),
            )
        });
        attach_next_request_extra_info(
            &mut redirects,
            cookie_report.clone(),
            request_extra_info.as_ref(),
        );
        let negotiated = negotiated_version(response.version);
        let status = response.status;
        let headers = response.headers.clone();
        let cookie_set_reports = if credentials_allowed {
            store_response_cookies(
                &shared.cookie_store,
                &current_url,
                &headers,
                &cookie_context,
            )?
        } else {
            Vec::new()
        };

        if prepared
            .response_policy
            .observe_response(&current_url, &headers)
            == ClientHintResponseAction::RestartNavigation
        {
            redirects.push(critical_client_hint_restart_redirect_info(
                current_url.clone(),
                network_response_extra_info(
                    request_extra_info.expect("Critical-CH applies only to navigation"),
                    status,
                    headers,
                    cookie_set_reports,
                ),
            ));
            continue;
        }

        if status == 304
            && let Some(stale) = stale_cache
        {
            if cached_streaming_response_body_exceeds_response_limit(&shared.config, &stale) {
                remove_cached_response(
                    &shared.config,
                    &prepared.request,
                    &current_url,
                    cookie_header.as_deref(),
                )?;
                continue;
            }
            let remove_after = response_headers_forbid_cache_storage(&headers);
            let merged = merge_cached_not_modified_streaming_response_lookup(
                &shared.config,
                &prepared.request,
                &current_url,
                cookie_header.as_deref(),
                stale,
                &headers,
            )?;
            if remove_after {
                remove_cached_response(
                    &shared.config,
                    &prepared.request,
                    &current_url,
                    cookie_header.as_deref(),
                )?;
            }
            return deliver_cached(job, merged, cookie_report, redirects).await;
        }

        if let Some(next) = next_followed_redirect_url_from_parts(
            &current_url,
            status,
            &headers,
            redirect_count,
            request.follow_redirects,
        )? && request.follow_redirects
        {
            cache_and_drain_redirect(
                shared,
                job,
                response,
                &prepared.request,
                &current_url,
                cookie_header.as_deref(),
                status,
                &headers,
            )
            .await?;
            redirects.push(redirect_info(
                &current_url,
                next.clone(),
                status,
                headers,
                cookie_report,
                cookie_set_reports,
                false,
                negotiated,
                request_extra_info,
            ));
            cookie_context = advance_cookie_request_context(cookie_context, &request.url, &next);
            request.apply_redirect_status(status);
            current_url = next;
            redirect_count += 1;
            http1_only = false;
            continue;
        }

        return deliver_network(
            shared,
            job,
            response,
            ResponseHead {
                final_url: current_url.clone(),
                status,
                headers: headers.clone(),
                request_cookie_report: cookie_report,
                cookie_set_reports,
                redirected: !redirects.is_empty(),
                redirect_chain: redirects,
                from_cache: false,
                negotiated_http_version: negotiated,
            },
            request_extra_info,
            &prepared.request,
            cookie_header.as_deref(),
        )
        .await;
    }
}

async fn await_transport(
    shared: &RuntimeShared,
    job: &QueuedJob,
    request: TransportRequest,
) -> Result<TransportResponse> {
    let shutdown = shared.shutdown_notify.notified();
    tokio::pin!(shutdown);
    check_lifecycle(shared, job)?;
    let future = shared.transport.execute(request);
    tokio::pin!(future);
    if let Some(deadline) = job.deadline {
        let deadline = tokio::time::Instant::from_std(deadline);
        tokio::select! {
            result = &mut future => result.map_err(anyhow::Error::new),
            _ = job.cancel.cancelled() => Err(cancellation_error("fetch runtime request cancelled")),
            _ = &mut shutdown => Err(cancellation_error("fetch runtime request cancelled during shutdown")),
            _ = tokio::time::sleep_until(deadline) => Err(anyhow::Error::new(TransportError::Timeout)),
        }
    } else {
        tokio::select! {
            result = &mut future => result.map_err(anyhow::Error::new),
            _ = job.cancel.cancelled() => Err(cancellation_error("fetch runtime request cancelled")),
            _ = &mut shutdown => Err(cancellation_error("fetch runtime request cancelled during shutdown")),
        }
    }
}

async fn connection_options(
    shared: &RuntimeShared,
    job: &QueuedJob,
    request: &Request,
    url: &Url,
) -> Result<ConnectionOptions> {
    let config = &shared.config;
    let proxy_auth = request
        .auth()
        .filter(|auth| {
            matches!(
                auth.target,
                crate::RequestAuthTarget::Proxy | crate::RequestAuthTarget::ProxyHeader
            )
        })
        .map(transport_auth);
    let mut options = ConnectionOptions {
        resolved_addresses: None,
        tls_session_cache: None,
        proxy: config.http_proxy().map(str::to_owned),
        no_proxy: config.http_no_proxy().map(str::to_owned),
        proxy_bearer_token: config.proxy_bearer_token().map(str::to_owned),
        proxy_auth,
        connect_timeout: effective_connect_timeout(config, request),
    };
    let selected_proxy = moli_stealth_net::connection::proxy_url(url, &options)?;
    let uses_local_origin_dns = selected_proxy
        .as_ref()
        .is_none_or(|proxy| matches!(proxy.scheme(), "socks4" | "socks5"));
    let resolution = target_address_resolution(config, url)?;
    if !uses_local_origin_dns {
        if (config.block_private_networks() || !config.block_cidrs().is_empty())
            && let TargetAddressResolution::Dns { host, port } = resolution
        {
            resolve_runtime_target_ips(shared, job, url, host, port).await?;
        }
        return Ok(options);
    }

    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("request URL has no port: `{url}`"))?;
    options.resolved_addresses = Some(match resolution {
        TargetAddressResolution::Approved(addresses) => addresses
            .into_iter()
            .map(|address| std::net::SocketAddr::new(address, port))
            .collect(),
        TargetAddressResolution::Dns { host, port } => {
            resolve_runtime_target_ips(shared, job, url, host, port)
                .await?
                .iter()
                .copied()
                .map(|address| std::net::SocketAddr::new(address, port))
                .collect()
        }
    });
    Ok(options)
}

async fn resolve_runtime_target_ips(
    shared: &RuntimeShared,
    job: &QueuedJob,
    request_url: &Url,
    host: String,
    port: u16,
) -> Result<Arc<[std::net::IpAddr]>> {
    let resolver = DnsResolverService::shared().map_err(|error| anyhow!(error.to_string()))?;
    let shutdown = shared.shutdown_notify.notified();
    tokio::pin!(shutdown);
    check_lifecycle(shared, job)?;

    let (completion_tx, completion_rx) = oneshot::channel();
    resolver.resolve(
        shared.dns_partition,
        DnsTarget::new(host.clone(), port),
        move |result| {
            let _ = completion_tx.send(result);
        },
    );
    let result = if let Some(deadline) = job.deadline {
        tokio::select! {
            result = completion_rx => result.context("shared DNS resolver dropped completion")?,
            _ = job.cancel.cancelled() => return Err(cancellation_error("fetch runtime request cancelled")),
            _ = &mut shutdown => return Err(cancellation_error("fetch runtime request cancelled during shutdown")),
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => return Err(anyhow::Error::new(TransportError::Timeout)),
        }
    } else {
        tokio::select! {
            result = completion_rx => result.context("shared DNS resolver dropped completion")?,
            _ = job.cancel.cancelled() => return Err(cancellation_error("fetch runtime request cancelled")),
            _ = &mut shutdown => return Err(cancellation_error("fetch runtime request cancelled during shutdown")),
        }
    };
    check_lifecycle(shared, job)?;
    let addresses = result
        .map_err(|error| anyhow!(error.to_string()))
        .with_context(|| format!("failed to resolve request host `{host}` for `{request_url}`"))?;
    validate_allowed_target_ips(&shared.config, request_url, &addresses)?;
    Ok(addresses)
}

fn transport_auth(auth: &crate::RequestAuth) -> TransportAuth {
    TransportAuth {
        scheme: match auth.scheme {
            crate::RequestAuthScheme::Basic => AuthScheme::Basic,
            crate::RequestAuthScheme::Digest => AuthScheme::Digest,
            crate::RequestAuthScheme::Negotiate => AuthScheme::Negotiate,
            crate::RequestAuthScheme::Ntlm => AuthScheme::Ntlm,
        },
        username: auth.username.clone(),
        password: auth.password.clone(),
    }
}

struct DecodedBody {
    source: DecodedBodySource,
}

enum DecodedBodySource {
    Identity(TransportResponseBody),
    Encoded {
        reader: Pin<Box<dyn AsyncRead + Send>>,
        producer: Option<tokio::task::JoinHandle<Result<()>>>,
        source_was_empty: Option<Arc<AtomicBool>>,
    },
}

impl DecodedBody {
    fn new(body: TransportResponseBody, headers: &[(String, String)]) -> Self {
        Self::new_with_empty_source_policy(body, headers, false)
    }

    fn for_followed_redirect(body: TransportResponseBody, headers: &[(String, String)]) -> Self {
        Self::new_with_empty_source_policy(body, headers, true)
    }

    fn new_with_empty_source_policy(
        mut body: TransportResponseBody,
        headers: &[(String, String)],
        accept_empty_source: bool,
    ) -> Self {
        let encoding = headers
            .iter()
            .rev()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-encoding"))
            .map(|(_, value)| value.trim().to_ascii_lowercase());
        let Some(encoding) =
            encoding.filter(|value| matches!(value.as_str(), "gzip" | "deflate" | "br" | "zstd"))
        else {
            return Self {
                source: DecodedBodySource::Identity(body),
            };
        };
        let (mut writer, reader) = tokio::io::duplex(32 * 1024);
        let source_was_empty = accept_empty_source.then(|| Arc::new(AtomicBool::new(false)));
        let producer_source_was_empty = source_was_empty.clone();
        let producer = tokio::spawn(async move {
            let mut saw_chunk = false;
            loop {
                match body.chunk().await.map_err(anyhow::Error::new)? {
                    Some(chunk) => {
                        saw_chunk |= !chunk.is_empty();
                        writer
                            .write_all(&chunk)
                            .await
                            .context("failed to feed response decoder")?;
                    }
                    None => {
                        if let Some(source_was_empty) = producer_source_was_empty.as_ref() {
                            source_was_empty.store(!saw_chunk, Ordering::Release);
                        }
                        break;
                    }
                }
            }
            writer
                .shutdown()
                .await
                .context("failed to finish response decoder input")
        });
        let reader = BufReader::new(reader);
        let reader: Pin<Box<dyn AsyncRead + Send>> = match encoding.as_str() {
            "gzip" => Box::pin(async_compression::tokio::bufread::GzipDecoder::new(reader)),
            "deflate" => Box::pin(async_compression::tokio::bufread::ZlibDecoder::new(reader)),
            "br" => Box::pin(async_compression::tokio::bufread::BrotliDecoder::new(
                reader,
            )),
            "zstd" => Box::pin(async_compression::tokio::bufread::ZstdDecoder::new(reader)),
            _ => unreachable!(),
        };
        Self {
            source: DecodedBodySource::Encoded {
                reader,
                producer: Some(producer),
                source_was_empty,
            },
        }
    }

    async fn chunk(&mut self) -> Result<Option<bytes::Bytes>> {
        match &mut self.source {
            DecodedBodySource::Identity(body) => body.chunk().await.map_err(anyhow::Error::new),
            DecodedBodySource::Encoded {
                reader,
                producer,
                source_was_empty,
            } => {
                let mut chunk = vec![0; 16 * 1024];
                let count = match reader.read(&mut chunk).await {
                    Ok(count) => count,
                    Err(_)
                        if source_was_empty
                            .as_ref()
                            .is_some_and(|empty| empty.load(Ordering::Acquire)) =>
                    {
                        if let Some(producer) = producer.take() {
                            producer.await.map_err(|error| {
                                anyhow!("response decoder producer failed: {error}")
                            })??;
                        }
                        return Ok(None);
                    }
                    Err(error) => return Err(error).context("failed to decode response body"),
                };
                if count != 0 {
                    chunk.truncate(count);
                    return Ok(Some(bytes::Bytes::from(chunk)));
                }
                if let Some(producer) = producer.take() {
                    if producer.is_finished() {
                        producer.await.map_err(|error| {
                            anyhow!("response decoder producer failed: {error}")
                        })??;
                    } else {
                        // A decoder may reach the end of its compressed member while
                        // the wire body still has trailing bytes. It will no longer
                        // read the duplex stream, so retaining the producer here can
                        // deadlock once that bounded stream fills.
                        producer.abort();
                    }
                }
                Ok(None)
            }
        }
    }
}

impl Drop for DecodedBody {
    fn drop(&mut self) {
        if let DecodedBodySource::Encoded {
            producer: Some(producer),
            ..
        } = &self.source
        {
            producer.abort();
        }
    }
}

async fn deliver_network(
    shared: &RuntimeShared,
    job: &mut QueuedJob,
    response: TransportResponse,
    head: ResponseHead,
    mut extra: Option<NetworkRequestExtraInfo>,
    effective_request: &Request,
    cookie_header: Option<&str>,
) -> Result<()> {
    job.cancel.reset_response_progress();
    let mut cache_writer = match create_streaming_cache_body_writer_for_response_parts(
        &shared.config,
        effective_request,
        &head.final_url,
        cookie_header,
        head.status,
        &head.headers,
    ) {
        Ok(writer) => writer,
        Err(error) => {
            tracing::debug!(url=%head.final_url, "failed to create response cache writer: {error}");
            None
        }
    };
    let declared_body_length = (!head
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-encoding")))
    .then(|| {
        head.headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
    })
    .flatten()
    .and_then(|(_, value)| value.parse::<usize>().ok());
    if shared
        .config
        .http_max_response_size()
        .zip(declared_body_length)
        .is_some_and(|(limit, declared)| declared > limit)
    {
        return Err(anyhow!(
            "response exceeded configured limit of {} bytes for {}",
            shared.config.http_max_response_size().unwrap(),
            head.final_url
        ));
    }
    if declared_body_length == Some(0) || matches!(head.status, 101 | 204 | 205 | 304) {
        job.cancel.mark_declared_response_body_complete();
    }
    let mut body = DecodedBody::new(response.body, &head.headers);
    start_delivery(&mut job.delivery, &head, &mut extra)?;
    let discard_body = matches!(job.delivery, Delivery::Raw { .. })
        && !effective_request.follow_redirects
        && matches!(head.status, 301 | 302 | 303 | 307 | 308)
        && head
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("location"));
    let mut received = 0usize;
    let mut buffered = Vec::new();
    while let Some(chunk) = next_decoded_chunk(shared, job, &mut body).await? {
        received = received
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow::Error::new(TransportError::TooLarge))?;
        if declared_body_length == Some(received) {
            job.cancel.mark_declared_response_body_complete();
        }
        if shared
            .config
            .http_max_response_size()
            .is_some_and(|limit| received > limit)
        {
            return Err(anyhow!(
                "response exceeded configured limit of {} bytes for {}",
                shared.config.http_max_response_size().unwrap(),
                head.final_url
            ));
        }
        if let Some(writer) = cache_writer.as_mut()
            && let Err(error) = writer.write_all(&chunk)
        {
            tracing::debug!(url=%head.final_url, "failed to append response body to cache: {error}");
            cache_writer = None;
        }
        if !discard_body {
            send_chunk(&mut job.delivery, &chunk, &mut buffered).await?;
        }
    }
    flush_text_tail(&mut job.delivery).await?;
    job.cancel.mark_response_terminal();
    if let Some(writer) = cache_writer
        && let Err(error) = finish_streaming_cached_response(
            &shared.config,
            effective_request,
            &head.final_url,
            cookie_header,
            &head.final_url,
            head.status,
            &head.headers,
            false,
            writer,
        )
    {
        tracing::debug!(url=%head.final_url, "failed to store response in cache: {error}");
    }
    finish_delivery(&mut job.delivery, head, buffered, extra, Ok(()));
    Ok(())
}

async fn next_decoded_chunk(
    shared: &RuntimeShared,
    job: &QueuedJob,
    body: &mut DecodedBody,
) -> Result<Option<bytes::Bytes>> {
    let shutdown = shared.shutdown_notify.notified();
    tokio::pin!(shutdown);
    check_lifecycle(shared, job)?;
    let future = body.chunk();
    tokio::pin!(future);
    if let Some(deadline) = job.deadline {
        tokio::select! {
            result = &mut future => result,
            _ = job.cancel.cancelled(), if !job.cancel.response_completion_is_committed() => Err(cancellation_error("fetch runtime request cancelled")),
            _ = &mut shutdown => Err(cancellation_error("fetch runtime request cancelled during shutdown")),
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err(anyhow::Error::new(TransportError::Timeout)),
        }
    } else {
        tokio::select! {
            result = &mut future => result,
            _ = job.cancel.cancelled(), if !job.cancel.response_completion_is_committed() => Err(cancellation_error("fetch runtime request cancelled")),
            _ = &mut shutdown => Err(cancellation_error("fetch runtime request cancelled during shutdown")),
        }
    }
}

async fn cache_and_drain_redirect(
    shared: &RuntimeShared,
    job: &QueuedJob,
    response: TransportResponse,
    request: &Request,
    request_url: &Url,
    cookie_header: Option<&str>,
    status: u16,
    headers: &[(String, String)],
) -> Result<()> {
    let mut writer = match create_streaming_cache_body_writer_for_response_parts(
        &shared.config,
        request,
        request_url,
        cookie_header,
        status,
        headers,
    ) {
        Ok(writer) => writer,
        Err(error) => {
            tracing::debug!(url=%request_url, "failed to create redirect cache writer: {error}");
            None
        }
    };
    let mut body = DecodedBody::for_followed_redirect(response.body, headers);
    let mut received = 0usize;
    while let Some(chunk) = next_decoded_chunk(shared, job, &mut body).await? {
        received = received
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow::Error::new(TransportError::TooLarge))?;
        if shared
            .config
            .http_max_response_size()
            .is_some_and(|limit| received > limit)
        {
            return Err(anyhow!(
                "redirect response exceeded configured limit of {} bytes",
                shared.config.http_max_response_size().unwrap()
            ));
        }
        if let Some(cache) = writer.as_mut()
            && let Err(error) = cache.write_all(&chunk)
        {
            tracing::debug!(url=%request_url, "failed to append redirect body to cache: {error}");
            writer = None;
        }
    }
    if let Some(writer) = writer
        && let Err(error) = finish_streaming_cached_response(
            &shared.config,
            request,
            request_url,
            cookie_header,
            request_url,
            status,
            headers,
            false,
            writer,
        )
    {
        tracing::debug!(url=%request_url, "failed to store redirect response in cache: {error}");
    }
    Ok(())
}

async fn deliver_proxy_failure(
    job: &mut QueuedJob,
    final_url: Url,
    proxy: moli_stealth_net::ProxyResponse,
    cookie_report: Option<StoredCookieQueryReport>,
    redirects: Vec<RedirectInfo>,
) -> Result<()> {
    let head = ResponseHead {
        final_url,
        status: proxy.status,
        headers: proxy.headers,
        request_cookie_report: cookie_report,
        cookie_set_reports: Vec::new(),
        redirected: !redirects.is_empty(),
        redirect_chain: redirects,
        from_cache: false,
        negotiated_http_version: Some(NegotiatedHttpVersion::Http11),
    };
    let mut extra = None;
    start_delivery(&mut job.delivery, &head, &mut extra)?;
    let mut buffered = Vec::new();
    send_chunk(&mut job.delivery, &proxy.body, &mut buffered).await?;
    flush_text_tail(&mut job.delivery).await?;
    job.cancel.mark_response_terminal();
    finish_delivery(&mut job.delivery, head, buffered, None, Ok(()));
    Ok(())
}

async fn deliver_cached(
    job: &mut QueuedJob,
    cached: CachedStreamingResponseLookup,
    cookie_report: Option<StoredCookieQueryReport>,
    redirects: Vec<RedirectInfo>,
) -> Result<()> {
    let final_url =
        Url::parse(&cached.final_url).context("failed to parse cached response final URL")?;
    let head = ResponseHead {
        final_url,
        status: cached.status,
        headers: cached.headers,
        request_cookie_report: cookie_report,
        cookie_set_reports: Vec::new(),
        redirected: !redirects.is_empty(),
        redirect_chain: redirects,
        from_cache: true,
        negotiated_http_version: None,
    };
    let mut extra = None;
    start_delivery(&mut job.delivery, &head, &mut extra)?;
    if matches!(job.delivery, Delivery::Raw { .. })
        && !job.request.follow_redirects
        && matches!(head.status, 301 | 302 | 303 | 307 | 308)
        && head
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("location"))
    {
        job.cancel.mark_response_terminal();
        finish_delivery(&mut job.delivery, head, Vec::new(), None, Ok(()));
        return Ok(());
    }
    let mut body = cached.body;
    let (chunk_tx, mut chunk_rx) = mpsc::channel::<Result<Vec<u8>>>(STREAM_QUEUE_CHUNKS);
    let reader = tokio::task::spawn_blocking(move || {
        let mut chunk = vec![0; 16 * 1024];
        loop {
            let count = match body
                .read(&mut chunk)
                .context("failed to read cached response body")
            {
                Ok(count) => count,
                Err(error) => {
                    let _ = chunk_tx.blocking_send(Err(error));
                    return;
                }
            };
            if count == 0 {
                return;
            }
            if chunk_tx.blocking_send(Ok(chunk[..count].to_vec())).is_err() {
                return;
            }
        }
    });
    let mut buffered = Vec::new();
    while let Some(chunk) = chunk_rx.recv().await {
        if job.cancel.is_cancelled() {
            return Err(cancellation_error("fetch runtime request cancelled"));
        }
        send_chunk(&mut job.delivery, &chunk?, &mut buffered).await?;
    }
    reader.await.context("cached response reader task failed")?;
    flush_text_tail(&mut job.delivery).await?;
    job.cancel.mark_response_terminal();
    finish_delivery(&mut job.delivery, head, buffered, None, Ok(()));
    Ok(())
}

fn start_delivery(
    delivery: &mut Delivery,
    head: &ResponseHead,
    extra: &mut Option<NetworkRequestExtraInfo>,
) -> Result<()> {
    match delivery {
        Delivery::Buffered(_) => {}
        Delivery::Html { started, .. } | Delivery::Raw { started, .. } => {
            let start = StreamingHtmlResponseStart {
                final_url: head.final_url.clone(),
                status: head.status,
                headers: head.headers.clone(),
                request_cookie_report: head.request_cookie_report.clone(),
                cookie_set_reports: head.cookie_set_reports.clone(),
                redirected: head.redirected,
                redirect_chain: head.redirect_chain.clone(),
                from_cache: head.from_cache,
                negotiated_http_version: head.negotiated_http_version,
                network_request_extra_info: extra.take(),
            };
            if let Some(tx) = started.take() {
                tx.send(Ok(start)).map_err(|_| {
                    anyhow!("streaming response consumer dropped before response start")
                })?;
            }
        }
    }
    Ok(())
}

async fn send_chunk(delivery: &mut Delivery, chunk: &[u8], buffered: &mut Vec<u8>) -> Result<()> {
    match delivery {
        Delivery::Buffered(_) => buffered.extend_from_slice(chunk),
        Delivery::Html {
            body, utf8_pending, ..
        } => {
            utf8_pending.extend_from_slice(chunk);
            loop {
                match std::str::from_utf8(utf8_pending) {
                    Ok(valid) => {
                        if !valid.is_empty() {
                            body.send(valid.to_owned())
                                .await
                                .map_err(|_| anyhow!("streaming html response consumer dropped"))?;
                        }
                        utf8_pending.clear();
                        break;
                    }
                    Err(error) => {
                        let valid_up_to = error.valid_up_to();
                        match error.error_len() {
                            None => {
                                if valid_up_to > 0 {
                                    let valid =
                                        String::from_utf8_lossy(&utf8_pending[..valid_up_to])
                                            .into_owned();
                                    body.send(valid).await.map_err(|_| {
                                        anyhow!("streaming html response consumer dropped")
                                    })?;
                                    utf8_pending.drain(..valid_up_to);
                                }
                                break;
                            }
                            Some(error_len) => {
                                let end = valid_up_to + error_len;
                                let text =
                                    String::from_utf8_lossy(&utf8_pending[..end]).into_owned();
                                body.send(text).await.map_err(|_| {
                                    anyhow!("streaming html response consumer dropped")
                                })?;
                                utf8_pending.drain(..end);
                            }
                        }
                    }
                }
            }
        }
        Delivery::Raw { body, .. } => body
            .send(chunk.to_vec())
            .await
            .map_err(|_| anyhow!("streaming raw response consumer dropped"))?,
    }
    Ok(())
}

async fn flush_text_tail(delivery: &mut Delivery) -> Result<()> {
    if let Delivery::Html {
        body, utf8_pending, ..
    } = delivery
        && !utf8_pending.is_empty()
    {
        let tail = String::from_utf8_lossy(utf8_pending).into_owned();
        utf8_pending.clear();
        body.send(tail)
            .await
            .map_err(|_| anyhow!("streaming html response consumer dropped"))?;
    }
    Ok(())
}

fn finish_delivery(
    delivery: &mut Delivery,
    head: ResponseHead,
    body: Vec<u8>,
    extra: Option<NetworkRequestExtraInfo>,
    completion: Result<()>,
) {
    match std::mem::replace(
        delivery,
        Delivery::Buffered(RuntimeResponseTx::TextCallback(Box::new(|_| {}))),
    ) {
        Delivery::Buffered(tx) => tx.send(completion.map(|_| {
            RawResponse::from_head_and_body(head, body).with_network_request_extra_info(extra)
        })),
        Delivery::Html {
            completion: Some(tx),
            ..
        }
        | Delivery::Raw {
            completion: Some(tx),
            ..
        } => {
            let _ = tx.send(completion);
        }
        Delivery::Html {
            completion: None, ..
        }
        | Delivery::Raw {
            completion: None, ..
        } => {}
    }
}

fn fail_delivery(delivery: Delivery, error: anyhow::Error) {
    match delivery {
        Delivery::Buffered(tx) => tx.send(Err(error)),
        Delivery::Html {
            started,
            completion,
            ..
        }
        | Delivery::Raw {
            started,
            completion,
            ..
        } => {
            if let Some(started) = started {
                let _ = started.send(Err(error));
                if let Some(done) = completion {
                    let _ = done.send(Err(anyhow!(
                        "streaming request failed before response start"
                    )));
                }
            } else if let Some(done) = completion {
                let _ = done.send(Err(error));
            }
        }
    }
}

fn request_is_replay_safe(request: &Request) -> bool {
    request.body_bytes().is_none()
        && matches!(
            request.method().to_ascii_uppercase().as_str(),
            "GET" | "HEAD" | "OPTIONS" | "TRACE"
        )
}

fn negotiated_version(version: http::Version) -> Option<NegotiatedHttpVersion> {
    match version {
        http::Version::HTTP_09 => Some(NegotiatedHttpVersion::Http09),
        http::Version::HTTP_10 => Some(NegotiatedHttpVersion::Http10),
        http::Version::HTTP_11 => Some(NegotiatedHttpVersion::Http11),
        http::Version::HTTP_2 => Some(NegotiatedHttpVersion::Http2),
        http::Version::HTTP_3 => Some(NegotiatedHttpVersion::Http3),
        _ => None,
    }
}

fn cancellation_error(context: &'static str) -> anyhow::Error {
    anyhow::Error::new(TransportError::Cancelled).context(context)
}

fn check_lifecycle(shared: &RuntimeShared, job: &QueuedJob) -> Result<()> {
    if shared.shutdown.load(Ordering::SeqCst) {
        return Err(cancellation_error(
            "fetch runtime request cancelled during shutdown",
        ));
    }
    if job.cancel.is_cancelled() && !job.cancel.response_completion_is_committed() {
        return Err(cancellation_error("fetch runtime request cancelled"));
    }
    if job
        .deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Err(anyhow::Error::new(TransportError::Timeout));
    }
    Ok(())
}

fn effective_connect_timeout(config: &FetchConfig, request: &Request) -> Option<Duration> {
    if let Some(ms) = config.http_connect_timeout_ms() {
        return Some(Duration::from_millis(ms));
    }
    if !request.is_top_level_navigation_request() {
        Some(Duration::from_secs(10))
    } else {
        None
    }
}

fn origin_key_for_url(url: &Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or(""),
        url.port_or_known_default().unwrap_or(0)
    )
}

fn request_fetch_priority_rank(request: &Request) -> u8 {
    request_fetch_load_priority(request).scheduler_rank()
}

pub(crate) fn request_fetch_load_priority(request: &Request) -> crate::ResourceLoadPriority {
    let hint = request.priority_hints.fetch_priority;
    let base = request_base_resource_priority(request);
    if request.subresource_request_metadata().is_none() {
        let priority = match request.browser_request_metadata() {
            Some(
                crate::BrowserRequestMetadata::AudioWorklet
                | crate::BrowserRequestMetadata::EventSource
                | crate::BrowserRequestMetadata::Fetch
                | crate::BrowserRequestMetadata::JsonModule
                | crate::BrowserRequestMetadata::Manifest
                | crate::BrowserRequestMetadata::StyleModule
                | crate::BrowserRequestMetadata::Xhr,
            ) => crate::RequestResourceType::Raw.default_load_priority(),
            _ => base,
        };
        return apply_subframe_priority_adjustment(
            apply_image_priority_boost(
                apply_fetch_priority_hint(priority, request.resource_type, hint),
                request,
                hint,
            ),
            request.priority_hints.subframe_context,
        );
    }
    let author = apply_fetch_priority_hint(base, request.resource_type, hint);
    let scheduler = request
        .script_scheduler_priority()
        .map(script_scheduler_priority)
        .unwrap_or(author);
    apply_subframe_priority_adjustment(
        apply_image_priority_boost(author.max(scheduler), request, hint),
        request.priority_hints.subframe_context,
    )
}

fn request_base_resource_priority(request: &Request) -> crate::ResourceLoadPriority {
    if request.priority_hints.link_preload
        && request.resource_type == crate::RequestResourceType::Font
    {
        crate::ResourceLoadPriority::High
    } else {
        request.resource_type.default_load_priority()
    }
}

fn apply_fetch_priority_hint(
    priority: crate::ResourceLoadPriority,
    resource_type: crate::RequestResourceType,
    hint: Option<crate::FetchPriorityHint>,
) -> crate::ResourceLoadPriority {
    match hint {
        Some(crate::FetchPriorityHint::High) => priority.max(crate::ResourceLoadPriority::High),
        Some(crate::FetchPriorityHint::Low)
            if resource_type == crate::RequestResourceType::CssStyleSheet
                && priority == crate::ResourceLoadPriority::VeryHigh =>
        {
            crate::ResourceLoadPriority::High
        }
        Some(crate::FetchPriorityHint::Low) => priority.min(crate::ResourceLoadPriority::Low),
        _ => priority,
    }
}

fn apply_image_priority_boost(
    priority: crate::ResourceLoadPriority,
    request: &Request,
    hint: Option<crate::FetchPriorityHint>,
) -> crate::ResourceLoadPriority {
    if request.priority_hints.in_document_image_priority_boost
        && request.resource_type == crate::RequestResourceType::Image
        && hint.is_none_or(|hint| hint == crate::FetchPriorityHint::Auto)
    {
        priority.max(crate::ResourceLoadPriority::Medium)
    } else {
        priority
    }
}

fn apply_subframe_priority_adjustment(
    priority: crate::ResourceLoadPriority,
    subframe: bool,
) -> crate::ResourceLoadPriority {
    if !subframe {
        priority
    } else if priority >= crate::ResourceLoadPriority::High {
        crate::ResourceLoadPriority::Low
    } else {
        crate::ResourceLoadPriority::VeryLow
    }
}

fn script_scheduler_priority(
    priority: crate::ScriptFetchSchedulerPriority,
) -> crate::ResourceLoadPriority {
    match priority {
        crate::ScriptFetchSchedulerPriority::Low => crate::ResourceLoadPriority::Low,
        crate::ScriptFetchSchedulerPriority::Auto => crate::ResourceLoadPriority::High,
        crate::ScriptFetchSchedulerPriority::High
        | crate::ScriptFetchSchedulerPriority::VeryHigh => crate::ResourceLoadPriority::VeryHigh,
    }
}

fn redirect_info(
    from: &Url,
    to: Url,
    status: u16,
    headers: Vec<(String, String)>,
    request_cookie_report: Option<StoredCookieQueryReport>,
    cookie_set_reports: Vec<moli_cookie_jar::StoredCookieSetReport>,
    from_cache: bool,
    negotiated_http_version: Option<NegotiatedHttpVersion>,
    request_extra_info: Option<NetworkRequestExtraInfo>,
) -> RedirectInfo {
    let has_extra = request_extra_info.is_some() && !from_cache;
    RedirectInfo {
        from_url: from.clone(),
        to_url: to,
        status,
        headers: headers.clone(),
        network_extra_info_available: has_extra,
        request_extra_info: None,
        response_extra_info: request_extra_info.map(|info| {
            network_response_extra_info(info, status, headers, cookie_set_reports.clone())
        }),
        redirect_has_extra_info: has_extra,
        request_cookie_report,
        cookie_set_reports,
        from_cache,
        negotiated_http_version,
    }
}

fn attach_next_request_extra_info(
    redirects: &mut [RedirectInfo],
    cookie_report: Option<StoredCookieQueryReport>,
    extra: Option<&NetworkRequestExtraInfo>,
) {
    if let Some(last) = redirects.last_mut() {
        last.request_cookie_report = cookie_report;
        last.request_extra_info = extra.cloned();
    }
}

fn network_response_extra_info(
    request_extra_info: NetworkRequestExtraInfo,
    status: u16,
    headers: Vec<(String, String)>,
    cookie_set_reports: Vec<moli_cookie_jar::StoredCookieSetReport>,
) -> NetworkResponseExtraInfo {
    NetworkResponseExtraInfo {
        request_extra_info,
        status,
        headers,
        cookie_set_reports,
    }
}

fn critical_client_hint_restart_redirect_info(
    url: Url,
    response_extra_info: NetworkResponseExtraInfo,
) -> RedirectInfo {
    RedirectInfo {
        from_url: url.clone(),
        headers: vec![("location".to_owned(), url.to_string())],
        to_url: url,
        status: 307,
        network_extra_info_available: false,
        request_extra_info: None,
        response_extra_info: Some(response_extra_info),
        redirect_has_extra_info: false,
        request_cookie_report: None,
        cookie_set_reports: Vec::new(),
        from_cache: false,
        negotiated_http_version: None,
    }
}

fn network_fetch_failure_for_request(
    request: &Request,
    current_url: &Url,
    redirects: &[RedirectInfo],
    error: anyhow::Error,
) -> anyhow::Error {
    if error.is::<NetworkFetchFailureContext>() {
        return error;
    }
    let Some(recorder) = request.network_observation_recorder() else {
        return error;
    };
    NetworkFetchFailureContext::attach_with_request_context(
        error,
        recorder.snapshot(),
        NetworkFetchFailureRequestContext::new(
            current_url.clone(),
            request.method.clone(),
            request.body.clone(),
            request.request_headers.clone(),
            redirects.to_vec(),
        ),
    )
}
