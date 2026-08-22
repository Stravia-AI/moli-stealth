use std::{cell::RefCell, collections::HashMap, rc::Rc};

use moli_bounded_buffer::{BoundedByteBuffer, ByteLimits, InsertOutcome};

use super::BrowserNetworkBody;

const RESPONSE_BODY_BUFFER_MAX_TOTAL_BYTES: usize = 20_000_000;
const RESPONSE_BODY_BUFFER_MAX_ENTRY_BYTES: usize = 2_000_000;
const REQUEST_BODY_BUFFER_MAX_TOTAL_BYTES: usize = 200_000_000;
const REQUEST_BODY_BUFFER_MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;

/// Browser-owned state of one captured network response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserNetworkResponseBody {
    Pending,
    Ready(BrowserNetworkBody),
    Failed(String),
    Evicted,
}

impl BrowserNetworkResponseBody {
    pub fn body_bytes_limited(&self, limit: usize) -> anyhow::Result<Vec<u8>> {
        match self {
            Self::Ready(body) => body.materialize_bytes_limited(limit),
            Self::Pending | Self::Failed(_) => {
                anyhow::bail!("No data found for resource with given identifier")
            }
            Self::Evicted => {
                anyhow::bail!("Request content was evicted from inspector cache")
            }
        }
    }

    pub fn ready_body(&self) -> Option<&BrowserNetworkBody> {
        match self {
            Self::Ready(body) => Some(body),
            Self::Pending | Self::Failed(_) | Self::Evicted => None,
        }
    }
}

/// Shared Browser Host residence for request identities and captured bodies.
///
/// The store contains no session ids, collector ids, event cursors or IO read
/// offsets. Frontends may keep those projections independently without owning
/// or resetting the underlying browser artifacts.
#[derive(Clone, Default)]
pub struct BrowserNetworkArtifactStore {
    inner: Rc<RefCell<BrowserNetworkArtifactStoreInner>>,
}

impl PartialEq for BrowserNetworkArtifactStore {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for BrowserNetworkArtifactStore {}

impl std::fmt::Debug for BrowserNetworkArtifactStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.borrow();
        formatter
            .debug_struct("BrowserNetworkArtifactStore")
            .field("next_request_sequence", &inner.next_request_sequence)
            .field("request_body_count", &inner.request_bodies.len())
            .field(
                "response_body_count",
                &(inner.response_bodies.len() + inner.response_terminal.len()),
            )
            .finish()
    }
}

#[derive(Debug)]
struct BrowserNetworkArtifactStoreInner {
    next_request_sequence: u64,
    request_bodies: BoundedByteBuffer<String, BrowserNetworkBody>,
    response_terminal: HashMap<String, BrowserNetworkResponseBody>,
    response_bodies: BoundedByteBuffer<String, BrowserNetworkBody>,
}

impl Default for BrowserNetworkArtifactStoreInner {
    fn default() -> Self {
        Self::with_limits(
            ByteLimits::new(
                REQUEST_BODY_BUFFER_MAX_TOTAL_BYTES,
                REQUEST_BODY_BUFFER_MAX_ENTRY_BYTES,
            ),
            ByteLimits::new(
                RESPONSE_BODY_BUFFER_MAX_TOTAL_BYTES,
                RESPONSE_BODY_BUFFER_MAX_ENTRY_BYTES,
            ),
        )
    }
}

impl BrowserNetworkArtifactStoreInner {
    fn with_limits(request_limits: ByteLimits, response_limits: ByteLimits) -> Self {
        Self {
            next_request_sequence: 0,
            request_bodies: BoundedByteBuffer::new(request_limits),
            response_terminal: HashMap::new(),
            response_bodies: BoundedByteBuffer::new(response_limits),
        }
    }

    fn response_body(&self, request_id: &str) -> Option<BrowserNetworkResponseBody> {
        self.response_bodies
            .get(request_id)
            .cloned()
            .map(BrowserNetworkResponseBody::Ready)
            .or_else(|| self.response_terminal.get(request_id).cloned())
    }

    fn record_response_body(&mut self, request_id: String, body: BrowserNetworkBody) {
        let byte_len = body.len();
        self.response_terminal.remove(&request_id);
        match self.response_bodies.insert(request_id, body, byte_len) {
            InsertOutcome::Stored { evicted } => {
                for (request_id, _) in evicted {
                    self.response_terminal
                        .insert(request_id, BrowserNetworkResponseBody::Evicted);
                }
            }
            InsertOutcome::Rejected { key, .. } => {
                self.response_terminal
                    .insert(key, BrowserNetworkResponseBody::Evicted);
            }
        }
    }

    fn record_response_terminal(
        &mut self,
        request_id: String,
        terminal: BrowserNetworkResponseBody,
    ) {
        self.response_bodies.remove(&request_id);
        self.response_terminal.insert(request_id, terminal);
    }
}

impl BrowserNetworkArtifactStore {
    pub fn allocate_request_sequence(&self) -> u64 {
        let mut inner = self.inner.borrow_mut();
        inner.next_request_sequence = inner.next_request_sequence.wrapping_add(1).max(1);
        inner.next_request_sequence
    }

    pub fn allocate_request_id(&self) -> String {
        format!("REQ-{}", self.allocate_request_sequence())
    }

    pub fn record_request_body(&self, request_id: String, body: BrowserNetworkBody) {
        let byte_len = body.len();
        let _ = self
            .inner
            .borrow_mut()
            .request_bodies
            .insert(request_id, body, byte_len);
    }

    pub fn record_pending_response_body(&self, request_id: String) {
        let mut inner = self.inner.borrow_mut();
        if inner.response_bodies.contains_key(request_id.as_str())
            || inner.response_terminal.contains_key(request_id.as_str())
        {
            return;
        }
        inner.record_response_terminal(request_id, BrowserNetworkResponseBody::Pending);
    }

    pub fn record_response_body(&self, request_id: String, body: BrowserNetworkBody) {
        self.inner
            .borrow_mut()
            .record_response_body(request_id, body);
    }

    pub fn record_failed_response_body(&self, request_id: String, error_text: String) {
        self.inner
            .borrow_mut()
            .record_response_terminal(request_id, BrowserNetworkResponseBody::Failed(error_text));
    }

    pub fn request_body(&self, request_id: &str) -> Option<BrowserNetworkBody> {
        self.inner.borrow().request_bodies.get(request_id).cloned()
    }

    pub fn response_body(&self, request_id: &str) -> Option<BrowserNetworkResponseBody> {
        self.inner.borrow().response_body(request_id)
    }

    /// Copies selected candidate artifacts into this authoritative store.
    ///
    /// BrowserContext candidates are often built before Core registration.
    /// Adoption copies only ids visible in that candidate, then its standalone
    /// store is dropped. Already-shared stores are detected and left alone.
    pub fn adopt_entries_from<'a>(
        &self,
        candidate: &Self,
        request_ids: impl IntoIterator<Item = &'a str>,
    ) {
        if Rc::ptr_eq(&self.inner, &candidate.inner) {
            return;
        }
        let candidate_next_request_sequence = candidate.inner.borrow().next_request_sequence;
        {
            let mut inner = self.inner.borrow_mut();
            inner.next_request_sequence = inner
                .next_request_sequence
                .max(candidate_next_request_sequence);
        }
        let request_ids = request_ids.into_iter().collect::<Vec<_>>();
        for request_id in request_ids {
            if let Some(body) = candidate.request_body(request_id) {
                self.record_request_body(request_id.to_owned(), body);
            }
            if let Some(response) = candidate.response_body(request_id) {
                match response {
                    BrowserNetworkResponseBody::Pending => {
                        self.record_pending_response_body(request_id.to_owned());
                    }
                    BrowserNetworkResponseBody::Ready(body) => {
                        self.record_response_body(request_id.to_owned(), body);
                    }
                    BrowserNetworkResponseBody::Failed(error) => {
                        self.record_failed_response_body(request_id.to_owned(), error);
                    }
                    BrowserNetworkResponseBody::Evicted => {
                        self.inner.borrow_mut().record_response_terminal(
                            request_id.to_owned(),
                            BrowserNetworkResponseBody::Evicted,
                        );
                    }
                }
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn next_request_sequence_for_test(&self) -> u64 {
        self.inner.borrow().next_request_sequence
    }

    #[cfg(test)]
    fn with_limits(request_limits: ByteLimits, response_limits: ByteLimits) -> Self {
        Self {
            inner: Rc::new(RefCell::new(BrowserNetworkArtifactStoreInner::with_limits(
                request_limits,
                response_limits,
            ))),
        }
    }

    #[cfg(test)]
    fn response_body_bytes(&self) -> usize {
        self.inner.borrow().response_bodies.used_bytes()
    }

    #[cfg(test)]
    fn request_body_bytes(&self) -> usize {
        self.inner.borrow().request_bodies.used_bytes()
    }

    #[cfg(test)]
    fn response_limits(&self) -> ByteLimits {
        self.inner.borrow().response_bodies.limits()
    }

    #[cfg(test)]
    fn request_limits(&self) -> ByteLimits {
        self.inner.borrow().request_bodies.limits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_store_preserves_existing_inspector_budget() {
        let store = BrowserNetworkArtifactStore::default();

        assert_eq!(
            store.response_limits(),
            ByteLimits::new(20_000_000, 2_000_000)
        );
        assert_eq!(
            store.request_limits(),
            ByteLimits::new(200_000_000, 64 * 1024 * 1024)
        );
    }

    #[test]
    fn request_store_evicts_oldest_payload_within_host_budget() {
        let store = BrowserNetworkArtifactStore::with_limits(
            ByteLimits::new(5, 4),
            ByteLimits::new(64, 64),
        );
        store.record_request_body(
            "REQ-first".to_owned(),
            BrowserNetworkBody::from_string("aa".to_owned()),
        );
        store.record_request_body(
            "REQ-second".to_owned(),
            BrowserNetworkBody::from_string("bb".to_owned()),
        );
        store.record_request_body(
            "REQ-third".to_owned(),
            BrowserNetworkBody::from_string("ccc".to_owned()),
        );

        assert_eq!(store.request_body_bytes(), 5);
        assert!(store.request_body("REQ-first").is_none());
        assert_eq!(
            store
                .request_body("REQ-second")
                .expect("second request body should remain")
                .materialize_bytes()
                .expect("second request body should materialize"),
            b"bb"
        );
    }

    #[test]
    fn response_store_marks_oldest_payload_evicted() {
        let store = BrowserNetworkArtifactStore::with_limits(
            ByteLimits::new(64, 64),
            ByteLimits::new(5, 4),
        );
        store.record_response_body(
            "REQ-first".to_owned(),
            BrowserNetworkBody::from_string("aa".to_owned()),
        );
        store.record_response_body(
            "REQ-second".to_owned(),
            BrowserNetworkBody::from_string("bb".to_owned()),
        );
        store.record_response_body(
            "REQ-third".to_owned(),
            BrowserNetworkBody::from_string("ccc".to_owned()),
        );

        assert_eq!(store.response_body_bytes(), 5);
        assert_eq!(
            store
                .response_body("REQ-first")
                .expect("evicted metadata must remain")
                .body_bytes_limited(10)
                .expect_err("oldest body should be evicted")
                .to_string(),
            "Request content was evicted from inspector cache"
        );
        assert_eq!(
            store
                .response_body("REQ-second")
                .expect("second response should remain")
                .body_bytes_limited(10)
                .expect("second body should be readable"),
            b"bb"
        );
    }

    #[test]
    fn response_store_returns_byte_charge_when_ready_body_becomes_terminal() {
        let store = BrowserNetworkArtifactStore::with_limits(
            ByteLimits::new(64, 64),
            ByteLimits::new(8, 4),
        );
        store.record_response_body(
            "REQ-failed".to_owned(),
            BrowserNetworkBody::from_string("body".to_owned()),
        );
        assert_eq!(store.response_body_bytes(), 4);

        store.record_failed_response_body("REQ-failed".to_owned(), "network failed".to_owned());

        assert_eq!(store.response_body_bytes(), 0);
        assert!(matches!(
            store.response_body("REQ-failed"),
            Some(BrowserNetworkResponseBody::Failed(error)) if error == "network failed"
        ));
    }

    #[test]
    fn cloned_store_shares_ids_and_artifacts() {
        let first = BrowserNetworkArtifactStore::default();
        let second = first.clone();

        assert_eq!(first.allocate_request_id(), "REQ-1");
        assert_eq!(second.allocate_request_id(), "REQ-2");
        first.record_request_body(
            "REQ-1".to_owned(),
            BrowserNetworkBody::from_bytes(b"post".to_vec()),
        );
        assert_eq!(
            second
                .request_body("REQ-1")
                .expect("clone should observe the same artifact")
                .materialize_bytes()
                .expect("body should materialize"),
            b"post"
        );
    }

    #[test]
    fn candidate_entries_are_adopted_without_copying_frontend_state() {
        let candidate = BrowserNetworkArtifactStore::default();
        assert_eq!(candidate.allocate_request_id(), "REQ-1");
        candidate.record_response_body(
            "REQ-candidate".to_owned(),
            BrowserNetworkBody::from_string("body".to_owned()),
        );
        let host = BrowserNetworkArtifactStore::default();

        host.adopt_entries_from(&candidate, ["REQ-candidate"]);

        assert_eq!(
            host.response_body("REQ-candidate")
                .expect("host should adopt the candidate body")
                .body_bytes_limited(16)
                .expect("adopted body should remain readable"),
            b"body"
        );
        assert_eq!(
            host.allocate_request_id(),
            "REQ-2",
            "Host adoption must not reuse an id already allocated by the candidate"
        );
    }
}
