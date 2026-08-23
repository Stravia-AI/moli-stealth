use std::{
    fmt,
    hash::{Hash, Hasher},
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use super::{BrowserContextId, BrowserTargetId};

static NEXT_PAGE_RESIDENCE_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct PageResidenceInstanceId(NonZeroU64);

impl PageResidenceInstanceId {
    fn allocate() -> Self {
        let raw = NEXT_PAGE_RESIDENCE_INSTANCE_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("browser Page residence instance id exhausted"));
        Self(
            NonZeroU64::new(raw)
                .unwrap_or_else(|| panic!("browser Page residence allocator returned zero")),
        )
    }
}

#[derive(Debug)]
pub(super) struct BrowserPageResidenceState {
    instance_id: PageResidenceInstanceId,
    generation: AtomicU64,
}

/// Stable capability for one physical browser Page slot.
///
/// Moving a target between active and parked storage moves this handle with
/// the slot. Installing or retiring the renderer Page advances its generation,
/// so work captured from the old Page cannot address its replacement.
#[derive(Clone)]
pub struct BrowserPageResidenceHandle {
    state: Arc<BrowserPageResidenceState>,
}

impl Default for BrowserPageResidenceHandle {
    fn default() -> Self {
        Self {
            state: Arc::new(BrowserPageResidenceState {
                instance_id: PageResidenceInstanceId::allocate(),
                generation: AtomicU64::new(0),
            }),
        }
    }
}

impl fmt::Debug for BrowserPageResidenceHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserPageResidenceHandle")
            .field("instance_id", &self.state.instance_id)
            .field("generation", &self.generation())
            .finish()
    }
}

impl BrowserPageResidenceHandle {
    pub fn generation(&self) -> u64 {
        self.state.generation.load(Ordering::Acquire)
    }

    pub(super) fn try_advance_generation_if_current(
        &self,
        expected: &PageResidenceIdentity,
    ) -> Result<u64, BrowserPageResidenceAdvanceError> {
        self.state.try_advance_generation_if_current(expected)
    }

    /// Fixture-only seam for downstream tests that synthesize Page-slot reuse
    /// without constructing a complete Browser Owner transaction.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn advance_generation_for_test_fixture(&self) -> u64 {
        self.state.advance_generation()
    }

    /// Fixture-only seam for tests that need an exact synthetic generation.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn set_generation_for_test_fixture(&self, generation: u64) {
        self.state.generation.store(generation, Ordering::Release);
    }

    pub fn identity(
        &self,
        browser_context_id: String,
        target_id: Option<String>,
    ) -> PageResidenceIdentity {
        PageResidenceIdentity {
            browser_context_id: BrowserContextId::new(browser_context_id),
            target_id: target_id.map(BrowserTargetId::new),
            instance_id: Some(self.state.instance_id),
            loaded_page_generation: self.generation(),
        }
    }

    /// Returns whether `identity` names this exact physical Page slot at its
    /// current generation.
    pub fn is_current(&self, identity: &PageResidenceIdentity) -> bool {
        self.state.is_current(identity)
    }

    pub(super) fn owns_identity_instance(&self, identity: &PageResidenceIdentity) -> bool {
        identity.instance_id == Some(self.state.instance_id)
    }

    pub(super) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl PartialEq for BrowserPageResidenceHandle {
    fn eq(&self, other: &Self) -> bool {
        self.state.instance_id == other.state.instance_id
    }
}

impl Eq for BrowserPageResidenceHandle {}

impl Hash for BrowserPageResidenceHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.state.instance_id.hash(state);
    }
}

impl BrowserPageResidenceState {
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn advance_generation(&self) -> u64 {
        let previous = self
            .generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("browser Page residence generation exhausted"));
        previous + 1
    }

    fn try_advance_generation_if_current(
        &self,
        expected: &PageResidenceIdentity,
    ) -> Result<u64, BrowserPageResidenceAdvanceError> {
        if expected.instance_id != Some(self.instance_id) {
            return Err(BrowserPageResidenceAdvanceError::InstanceMismatch);
        }
        let expected_generation = expected.loaded_page_generation;
        let Some(successor_generation) = expected_generation.checked_add(1) else {
            return Err(BrowserPageResidenceAdvanceError::GenerationExhausted);
        };
        self.generation
            .compare_exchange(
                expected_generation,
                successor_generation,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| successor_generation)
            .map_err(
                |current_generation| BrowserPageResidenceAdvanceError::StaleGeneration {
                    current_generation,
                },
            )
    }

    pub(super) fn is_current(&self, identity: &PageResidenceIdentity) -> bool {
        identity.instance_id == Some(self.instance_id)
            && identity.loaded_page_generation == self.generation.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BrowserPageResidenceAdvanceError {
    InstanceMismatch,
    StaleGeneration { current_generation: u64 },
    GenerationExhausted,
}

/// Identifies one installed renderer Page residence within a browser Target.
///
/// This identity deliberately excludes frontend/session identity. Attaching,
/// detaching, or replacing a DevTools session cannot change whether a
/// browser-owned action still addresses the same Page.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct PageResidenceIdentity {
    browser_context_id: BrowserContextId,
    target_id: Option<BrowserTargetId>,
    instance_id: Option<PageResidenceInstanceId>,
    loaded_page_generation: u64,
}

impl PageResidenceIdentity {
    /// Creates a synthetic identity for compatibility fixtures.
    ///
    /// Live browser actions must capture their identity from a
    /// `BrowserPageResidenceHandle`; a synthetic identity has no slot instance
    /// capability and cannot authorize an owner lookup.
    pub fn new(
        browser_context_id: String,
        target_id: Option<String>,
        loaded_page_generation: u64,
    ) -> Self {
        Self {
            browser_context_id: BrowserContextId::new(browser_context_id),
            target_id: target_id.map(BrowserTargetId::new),
            instance_id: None,
            loaded_page_generation,
        }
    }

    pub fn browser_context_id(&self) -> &str {
        self.browser_context_id.as_str()
    }

    pub(super) fn browser_context_identity(&self) -> &BrowserContextId {
        &self.browser_context_id
    }

    pub fn target_id(&self) -> Option<&str> {
        self.target_id.as_ref().map(BrowserTargetId::as_str)
    }

    pub(super) fn target_identity(&self) -> Option<&BrowserTargetId> {
        self.target_id.as_ref()
    }

    pub fn loaded_page_generation(&self) -> u64 {
        self.loaded_page_generation
    }

    /// Returns whether both identities name the same live Page-slot instance.
    ///
    /// The loaded Document generation may differ: Target creation precedes
    /// initial-Document materialization, but the stable Page-slot capability
    /// must remain the same. Synthetic compatibility identities deliberately
    /// cannot prove instance equality.
    pub fn same_residence_instance(&self, other: &Self) -> bool {
        self.instance_id.is_some()
            && self.browser_context_id == other.browser_context_id
            && self.target_id == other.target_id
            && self.instance_id == other.instance_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_residence_identity_has_no_frontend_component() {
        let handle = BrowserPageResidenceHandle::default();
        let identity = handle.identity("context-1".to_owned(), Some("target-1".to_owned()));

        assert_eq!(identity.browser_context_id(), "context-1");
        assert_eq!(identity.target_id(), Some("target-1"));
        assert_eq!(identity.loaded_page_generation(), 0);
        assert_ne!(
            identity,
            PageResidenceIdentity::new("context-1".to_owned(), Some("target-1".to_owned()), 0,),
            "a live capture must contain a slot instance capability"
        );
        assert_ne!(
            identity,
            handle.identity("context-2".to_owned(), Some("target-1".to_owned())),
            "browser context remains part of exact Page identity"
        );
        assert_ne!(
            identity,
            handle.identity("context-1".to_owned(), Some("target-2".to_owned())),
            "browser Target remains part of exact Page identity"
        );
    }

    #[test]
    fn distinct_slots_and_successor_generations_reject_old_identity() {
        let first = BrowserPageResidenceHandle::default();
        let second = BrowserPageResidenceHandle::default();
        let original = first.identity("context-1".to_owned(), Some("target-1".to_owned()));

        assert!(first.state.is_current(&original));
        assert!(!second.state.is_current(&original));
        assert_eq!(first.advance_generation_for_test_fixture(), 1);
        assert!(!first.state.is_current(&original));
    }

    #[test]
    fn residence_instance_identity_survives_generation_but_rejects_id_reuse() {
        let first = BrowserPageResidenceHandle::default();
        let original = first.identity("context-1".to_owned(), Some("target-1".to_owned()));

        first.advance_generation_for_test_fixture();
        let successor = first.identity("context-1".to_owned(), Some("target-1".to_owned()));
        assert!(original.same_residence_instance(&successor));

        let reused_public_id = BrowserPageResidenceHandle::default()
            .identity("context-1".to_owned(), Some("target-1".to_owned()));
        assert!(!original.same_residence_instance(&reused_public_id));
        assert!(
            !original.same_residence_instance(&PageResidenceIdentity::new(
                "context-1".to_owned(),
                Some("target-1".to_owned()),
                0,
            ))
        );
    }
}
