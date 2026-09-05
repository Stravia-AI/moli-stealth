use std::{fmt, sync::Arc};

use moli_cookie_jar::StoredCookieQueryReport;
use parking_lot::Mutex;
use url::Url;

use crate::RedirectInfo;

const MAX_OBSERVED_EXCHANGES: usize = 32;
const MAX_OBSERVED_HEADER_BLOCK_BYTES: usize = 256 * 1024;

/// Request headers observed after the transport has finalized an HTTP exchange.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkRequestObservation {
    headers: Vec<(String, String)>,
    cookie_report: Option<StoredCookieQueryReport>,
    truncated: bool,
}

impl NetworkRequestObservation {
    pub fn new(headers: Vec<(String, String)>) -> Self {
        Self {
            headers,
            cookie_report: None,
            truncated: false,
        }
    }

    fn from_headers(
        headers: &[(String, String)],
        cookie_report: Option<StoredCookieQueryReport>,
    ) -> Self {
        let (headers, truncated) = bounded_headers(headers);
        Self {
            headers,
            cookie_report,
            truncated,
        }
    }

    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    pub fn into_headers(self) -> Vec<(String, String)> {
        self.headers
    }

    pub fn cookie_report(&self) -> Option<&StoredCookieQueryReport> {
        self.cookie_report.as_ref()
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Raw response metadata observed before cache revalidation merges are applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkResponseObservation {
    status: u16,
    headers: Vec<(String, String)>,
    truncated: bool,
}

impl NetworkResponseObservation {
    pub fn new(status: u16, headers: Vec<(String, String)>) -> Self {
        Self {
            status,
            headers,
            truncated: false,
        }
    }

    fn from_headers(status: u16, headers: &[(String, String)]) -> Self {
        let (headers, truncated) = bounded_headers(headers);
        Self {
            status,
            headers,
            truncated,
        }
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Transport observations for one request/response exchange.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkExchangeObservation {
    request: NetworkRequestObservation,
    response: Option<NetworkResponseObservation>,
}

impl NetworkExchangeObservation {
    pub fn new(
        request: NetworkRequestObservation,
        response: Option<NetworkResponseObservation>,
    ) -> Self {
        Self { request, response }
    }

    fn request_only(request: NetworkRequestObservation) -> Self {
        Self {
            request,
            response: None,
        }
    }

    pub fn request(&self) -> &NetworkRequestObservation {
        &self.request
    }

    pub fn response(&self) -> Option<&NetworkResponseObservation> {
        self.response.as_ref()
    }
}

/// Ordered transport observations for a fetch, including redirect and auth hops.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NetworkObservationJournal {
    exchanges: Vec<NetworkExchangeObservation>,
    truncated: bool,
    terminal_response_is_failed_proxy_connect: bool,
}

impl NetworkObservationJournal {
    pub fn from_exchanges(exchanges: Vec<NetworkExchangeObservation>) -> Self {
        let mut journal = Self::default();
        journal.append(Self {
            exchanges,
            truncated: false,
            terminal_response_is_failed_proxy_connect: false,
        });
        journal
    }

    pub fn from_request_observation(observation: NetworkRequestObservation) -> Self {
        Self {
            exchanges: vec![NetworkExchangeObservation::request_only(observation)],
            truncated: false,
            terminal_response_is_failed_proxy_connect: false,
        }
    }

    pub fn exchanges(&self) -> &[NetworkExchangeObservation] {
        &self.exchanges
    }

    pub fn final_request_observation(&self) -> Option<&NetworkRequestObservation> {
        self.exchanges
            .last()
            .map(NetworkExchangeObservation::request)
    }

    pub fn final_response_observation(&self) -> Option<&NetworkResponseObservation> {
        self.exchanges
            .last()
            .and_then(NetworkExchangeObservation::response)
    }

    pub fn is_empty(&self) -> bool {
        self.exchanges.is_empty()
    }

    pub fn truncated(&self) -> bool {
        self.truncated
            || self.exchanges.iter().any(|exchange| {
                exchange.request.truncated
                    || exchange
                        .response
                        .as_ref()
                        .is_some_and(|response| response.truncated)
            })
    }

    pub fn terminal_response_is_failed_proxy_connect(&self) -> bool {
        self.terminal_response_is_failed_proxy_connect
    }

    pub fn append(&mut self, mut other: Self) {
        let remaining = MAX_OBSERVED_EXCHANGES.saturating_sub(self.exchanges.len());
        if other.exchanges.len() > remaining {
            other.exchanges.truncate(remaining);
            self.truncated = true;
        }
        self.truncated |= other.truncated;
        self.terminal_response_is_failed_proxy_connect =
            other.terminal_response_is_failed_proxy_connect;
        self.exchanges.extend(other.exchanges);
    }
}

#[derive(Clone, Default)]
pub(crate) struct NetworkObservationRecorder {
    state: Arc<Mutex<NetworkObservationRecorderState>>,
}

impl fmt::Debug for NetworkObservationRecorder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("NetworkObservationRecorder")
            .field("exchange_count", &state.journal.exchanges.len())
            .field("truncated", &state.journal.truncated())
            .finish()
    }
}

impl NetworkObservationRecorder {
    pub(crate) fn transport_observer(
        &self,
        cookie_report: Option<StoredCookieQueryReport>,
    ) -> Arc<dyn moli_stealth_net::TransportObserver> {
        Arc::new(NetworkTransportObserver {
            recorder: self.clone(),
            cookie_report,
        })
    }

    fn record_request_sent(
        &self,
        headers: &[(String, String)],
        cookie_report: Option<StoredCookieQueryReport>,
    ) {
        let mut state = self.state.lock();
        if state.journal.exchanges.len() == MAX_OBSERVED_EXCHANGES {
            state.journal.truncated = true;
            return;
        }
        state
            .journal
            .exchanges
            .push(NetworkExchangeObservation::request_only(
                NetworkRequestObservation::from_headers(headers, cookie_report),
            ));
    }

    fn record_response_received(&self, status: u16, headers: &[(String, String)]) {
        let mut state = self.state.lock();
        if state.journal.truncated {
            return;
        }
        if let Some(exchange) = state
            .journal
            .exchanges
            .iter_mut()
            .rev()
            .find(|exchange| exchange.response.is_none())
        {
            exchange.response = Some(NetworkResponseObservation::from_headers(status, headers));
        }
    }

    pub(crate) fn record_failed_proxy_connect_terminal(&self) {
        self.state
            .lock()
            .journal
            .terminal_response_is_failed_proxy_connect = true;
    }

    pub(crate) fn snapshot(&self) -> NetworkObservationJournal {
        self.state.lock().journal.clone()
    }
}

#[derive(Default)]
struct NetworkObservationRecorderState {
    journal: NetworkObservationJournal,
}

#[derive(Debug)]
struct NetworkTransportObserver {
    recorder: NetworkObservationRecorder,
    cookie_report: Option<StoredCookieQueryReport>,
}

impl moli_stealth_net::TransportObserver for NetworkTransportObserver {
    fn request_sent(&self, headers: &[(String, String)]) {
        self.recorder
            .record_request_sent(headers, self.cookie_report.clone());
    }

    fn response_received(&self, status: u16, headers: &[(String, String)]) {
        self.recorder.record_response_received(status, headers);
    }
}

fn bounded_headers(headers: &[(String, String)]) -> (Vec<(String, String)>, bool) {
    let mut observed = Vec::with_capacity(headers.len());
    let mut observed_bytes = 0usize;
    for (name, value) in headers {
        let header_bytes = name.len().saturating_add(value.len()).saturating_add(4);
        if observed_bytes.saturating_add(header_bytes) > MAX_OBSERVED_HEADER_BLOCK_BYTES {
            return (observed, true);
        }
        observed_bytes += header_bytes;
        observed.push((name.clone(), value.clone()));
    }
    (observed, false)
}

/// Machine-readable context attached to a network fetch error.
///
/// The underlying cause remains owned by [`anyhow::Error`]. This context only
/// carries the transport and request state needed by browser-facing protocol
/// consumers when a transfer fails before response metadata is available.
pub struct NetworkFetchFailureContext {
    observation_journal: NetworkObservationJournal,
    network_error_text: &'static str,
    reason: String,
    request_context: Option<NetworkFetchFailureRequestContext>,
}

/// Request and redirect state owned by the fetch runtime when a transfer fails.
///
/// This remains separate from the raw transport observation journal: the
/// journal records what the transport put on the wire, while this context records the
/// browser-facing request chain that selected those exchanges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkFetchFailureRequestContext {
    current_url: Url,
    request_method: String,
    request_body: Option<Vec<u8>>,
    request_headers: Vec<(String, String)>,
    redirect_chain: Vec<RedirectInfo>,
}

impl NetworkFetchFailureRequestContext {
    pub(crate) fn new(
        current_url: Url,
        request_method: String,
        request_body: Option<Vec<u8>>,
        request_headers: Vec<(String, String)>,
        redirect_chain: Vec<RedirectInfo>,
    ) -> Self {
        Self {
            current_url,
            request_method,
            request_body,
            request_headers,
            redirect_chain,
        }
    }

    pub fn current_url(&self) -> &Url {
        &self.current_url
    }

    pub fn request_method(&self) -> &str {
        &self.request_method
    }

    pub fn request_body(&self) -> Option<&[u8]> {
        self.request_body.as_deref()
    }

    pub fn request_headers(&self) -> &[(String, String)] {
        &self.request_headers
    }

    pub fn redirect_chain(&self) -> &[RedirectInfo] {
        &self.redirect_chain
    }
}

impl NetworkFetchFailureContext {
    pub(crate) fn attach(
        source: anyhow::Error,
        observation_journal: NetworkObservationJournal,
    ) -> anyhow::Error {
        let network_error_text = crate::error::browser_network_error_text(&source);
        let reason = source.to_string();
        source.context(Self {
            observation_journal,
            network_error_text,
            reason,
            request_context: None,
        })
    }

    pub(crate) fn attach_with_request_context(
        source: anyhow::Error,
        observation_journal: NetworkObservationJournal,
        request_context: NetworkFetchFailureRequestContext,
    ) -> anyhow::Error {
        let network_error_text = crate::error::browser_network_error_text(&source);
        let reason = source.to_string();
        source.context(Self {
            observation_journal,
            network_error_text,
            reason,
            request_context: Some(request_context),
        })
    }

    pub fn observation_journal(&self) -> &NetworkObservationJournal {
        &self.observation_journal
    }

    pub fn network_error_text(&self) -> &'static str {
        self.network_error_text
    }

    /// Returns the single human-readable transport or policy reason captured
    /// before this machine-readable context was attached.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn request_context(&self) -> Option<&NetworkFetchFailureRequestContext> {
        self.request_context.as_ref()
    }
}

impl fmt::Display for NetworkFetchFailureContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.request_context {
            Some(request_context) => {
                write!(
                    formatter,
                    "failed to fetch `{}`",
                    request_context.current_url
                )
            }
            None => formatter.write_str("network fetch failed"),
        }
    }
}

impl fmt::Debug for NetworkFetchFailureContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("NetworkFetchFailureContext");
        debug
            .field(
                "observed_exchange_count",
                &self.observation_journal.exchanges().len(),
            )
            .field("network_error_text", &self.network_error_text);
        if let Some(request_context) = &self.request_context {
            debug
                .field("current_url", &request_context.current_url)
                .field("request_method", &request_context.request_method)
                .field("has_request_body", &request_context.request_body.is_some())
                .field(
                    "request_header_count",
                    &request_context.request_headers.len(),
                )
                .field("redirect_count", &request_context.redirect_chain.len());
        }
        debug.finish()
    }
}

/// The response and independent per-exchange observations produced by one fetch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkFetchResult<R> {
    response: R,
    observation_journal: NetworkObservationJournal,
}

impl<R> NetworkFetchResult<R> {
    pub fn new(response: R, request_observation: Option<NetworkRequestObservation>) -> Self {
        let observation_journal = request_observation
            .map(NetworkObservationJournal::from_request_observation)
            .unwrap_or_default();
        Self::with_observation_journal(response, observation_journal)
    }

    pub fn with_observation_journal(
        response: R,
        observation_journal: NetworkObservationJournal,
    ) -> Self {
        Self {
            response,
            observation_journal,
        }
    }

    pub fn without_request_observation(response: R) -> Self {
        Self::with_observation_journal(response, NetworkObservationJournal::default())
    }

    pub fn response(&self) -> &R {
        &self.response
    }

    pub fn response_mut(&mut self) -> &mut R {
        &mut self.response
    }

    pub fn request_observation(&self) -> Option<&NetworkRequestObservation> {
        self.observation_journal.final_request_observation()
    }

    pub fn observation_journal(&self) -> &NetworkObservationJournal {
        &self.observation_journal
    }

    pub fn into_response(self) -> R {
        self.response
    }

    pub fn into_parts(self) -> (R, Option<NetworkRequestObservation>) {
        let final_request_observation = self
            .observation_journal
            .final_request_observation()
            .cloned();
        (self.response, final_request_observation)
    }

    pub fn into_parts_with_observation_journal(self) -> (R, NetworkObservationJournal) {
        (self.response, self.observation_journal)
    }

    pub fn map_response<T>(self, map: impl FnOnce(R) -> T) -> NetworkFetchResult<T> {
        let (response, observation_journal) = self.into_parts_with_observation_journal();
        NetworkFetchResult::with_observation_journal(map(response), observation_journal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_fetch_failure_context_preserves_source_without_repeating_it() {
        let source = std::io::Error::other("transport failure sentinel");
        let error = NetworkFetchFailureContext::attach_with_request_context(
            anyhow::Error::new(source),
            NetworkObservationJournal::default(),
            NetworkFetchFailureRequestContext::new(
                Url::parse("http://localhost/").expect("test URL should parse"),
                "GET".to_owned(),
                None,
                Vec::new(),
                Vec::new(),
            ),
        )
        .context("outer failure sentinel");

        let report = format!("{error:?}");

        assert!(
            report.contains("failed to fetch `http://localhost/`"),
            "{report}"
        );
        assert_eq!(report.matches("transport failure sentinel").count(), 1);
        assert!(error.downcast_ref::<NetworkFetchFailureContext>().is_some());
        assert_eq!(
            error
                .downcast_ref::<NetworkFetchFailureContext>()
                .map(NetworkFetchFailureContext::reason),
            Some("transport failure sentinel")
        );
        assert!(error.downcast_ref::<std::io::Error>().is_some());
    }

    #[test]
    fn network_fetch_failure_debug_redacts_request_values() {
        let failure = NetworkFetchFailureContext {
            observation_journal: NetworkObservationJournal::default(),
            network_error_text: "net::ERR_FAILED",
            reason: "transport rejected secret values".to_owned(),
            request_context: Some(NetworkFetchFailureRequestContext::new(
                Url::parse("https://example.test/").expect("test URL should parse"),
                "POST".to_owned(),
                Some(b"secret request body".to_vec()),
                vec![("Authorization".to_owned(), "secret token".to_owned())],
                Vec::new(),
            )),
        };

        let report = format!("{failure:?}");

        assert!(!report.contains("secret request body"), "{report}");
        assert!(!report.contains("secret token"), "{report}");
        assert!(
            !report.contains("transport rejected secret values"),
            "{report}"
        );
        assert!(report.contains("has_request_body: true"), "{report}");
        assert!(report.contains("request_header_count: 1"), "{report}");
    }

    #[test]
    fn recorder_preserves_redirect_exchange_order_and_raw_response_status() {
        let recorder = NetworkObservationRecorder::default();
        let observer = recorder.transport_observer(None);
        observer.request_sent(&[
            ("Host".to_owned(), "example.test".to_owned()),
            ("Accept-Encoding".to_owned(), "gzip".to_owned()),
        ]);
        observer.response_received(302, &[("Location".to_owned(), "/final".to_owned())]);
        observer.request_sent(&[
            ("Host".to_owned(), "example.test".to_owned()),
            ("If-None-Match".to_owned(), "\"v1\"".to_owned()),
        ]);
        observer.response_received(304, &[("ETag".to_owned(), "\"v1\"".to_owned())]);

        let journal = recorder.snapshot();
        assert_eq!(journal.exchanges().len(), 2);
        assert_eq!(
            journal.exchanges()[0].request().headers(),
            [
                ("Host".to_owned(), "example.test".to_owned()),
                ("Accept-Encoding".to_owned(), "gzip".to_owned()),
            ]
        );
        assert_eq!(
            journal.exchanges()[0]
                .response()
                .expect("redirect response")
                .status(),
            302
        );
        assert_eq!(
            journal
                .final_request_observation()
                .expect("final request")
                .headers(),
            [
                ("Host".to_owned(), "example.test".to_owned()),
                ("If-None-Match".to_owned(), "\"v1\"".to_owned()),
            ]
        );
        assert_eq!(
            journal
                .final_response_observation()
                .expect("final response")
                .status(),
            304
        );
    }

    #[test]
    fn response_mapping_preserves_observation_journal() {
        let result = NetworkFetchResult::with_observation_journal(
            "streaming",
            NetworkObservationJournal::from_request_observation(NetworkRequestObservation::new(
                vec![("Accept".to_owned(), "text/html".to_owned())],
            )),
        );

        let result = result.map_response(|_| "materialized");

        assert_eq!(result.response(), &"materialized");
        assert_eq!(
            result
                .request_observation()
                .expect("request observation")
                .headers(),
            [("Accept".to_owned(), "text/html".to_owned())]
        );
    }

    #[test]
    fn recorder_bounds_exchange_count() {
        let recorder = NetworkObservationRecorder::default();
        let observer = recorder.transport_observer(None);
        for index in 0..=MAX_OBSERVED_EXCHANGES {
            observer.request_sent(&[("X-Request".to_owned(), index.to_string())]);
        }
        observer.response_received(200, &[("Content-Length".to_owned(), "0".to_owned())]);

        let journal = recorder.snapshot();
        assert_eq!(journal.exchanges().len(), MAX_OBSERVED_EXCHANGES);
        assert!(journal.truncated());
        assert!(journal.exchanges().last().unwrap().response().is_none());
    }

    #[test]
    fn recorder_bounds_each_header_block() {
        let recorder = NetworkObservationRecorder::default();
        let observer = recorder.transport_observer(None);
        observer.request_sent(&[(
            "X-Large".to_owned(),
            "a".repeat(MAX_OBSERVED_HEADER_BLOCK_BYTES + 1),
        )]);

        let journal = recorder.snapshot();
        let request = journal
            .final_request_observation()
            .expect("request observation");
        assert!(request.truncated());
        assert!(journal.truncated());
    }

    #[test]
    fn journal_append_takes_terminal_provenance_from_latest_result() {
        let recorder = NetworkObservationRecorder::default();
        recorder.record_failed_proxy_connect_terminal();
        let failed_connect = recorder.snapshot();

        let mut followed_by_target_response = failed_connect.clone();
        followed_by_target_response.append(NetworkObservationJournal::default());
        assert!(!followed_by_target_response.terminal_response_is_failed_proxy_connect());

        let mut followed_by_connect_failure = NetworkObservationJournal::default();
        followed_by_connect_failure.append(failed_connect);
        assert!(followed_by_connect_failure.terminal_response_is_failed_proxy_connect());
    }
}
