use std::{
    fmt,
    hash::{Hash, Hasher},
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
};

use super::BrowserContextId;

static NEXT_BROWSER_CONTEXT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

const CONTEXT_STAGED: u8 = 0;
const CONTEXT_LIVE: u8 = 1;
const CONTEXT_RETIRED: u8 = 2;
const CONTEXT_ACTIVATION_RESERVED: u8 = 3;
const CONTEXT_RETIREMENT_RESERVED: u8 = 4;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct BrowserContextInstanceId(NonZeroU64);

impl BrowserContextInstanceId {
    fn allocate() -> Self {
        let raw = NEXT_BROWSER_CONTEXT_INSTANCE_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("browser context instance id exhausted"));
        Self(
            NonZeroU64::new(raw)
                .unwrap_or_else(|| panic!("browser context instance allocator returned zero")),
        )
    }
}

struct BrowserContextHandleState {
    instance_id: BrowserContextInstanceId,
    browser_context_id: BrowserContextId,
    lifecycle: AtomicU8,
}

/// Stable capability for one BrowserContext instance.
///
/// Public BrowserContext ids may be supplied by a frontend and reused after
/// disposal. The physical context projection carries this capability so a
/// queued owner action cannot be redirected to a later context with the same
/// public id.
#[derive(Clone)]
pub struct BrowserContextHandle {
    state: Arc<BrowserContextHandleState>,
}

impl BrowserContextHandle {
    /// Allocates a staged BrowserContext capability.
    ///
    /// Browser Core must accept the matching context registration before the
    /// handle becomes live.
    pub fn staged(browser_context_id: impl Into<String>) -> Self {
        Self {
            state: Arc::new(BrowserContextHandleState {
                instance_id: BrowserContextInstanceId::allocate(),
                browser_context_id: BrowserContextId::new(browser_context_id),
                lifecycle: AtomicU8::new(CONTEXT_STAGED),
            }),
        }
    }

    pub fn browser_context_id(&self) -> &str {
        self.state.browser_context_id.as_str()
    }

    pub fn is_live(&self) -> bool {
        matches!(
            self.state.lifecycle.load(Ordering::Acquire),
            CONTEXT_LIVE | CONTEXT_RETIREMENT_RESERVED
        )
    }

    pub fn is_retired(&self) -> bool {
        self.state.lifecycle.load(Ordering::Acquire) == CONTEXT_RETIRED
    }

    #[cfg(test)]
    pub(super) fn is_staged(&self) -> bool {
        matches!(
            self.state.lifecycle.load(Ordering::Acquire),
            CONTEXT_STAGED | CONTEXT_ACTIVATION_RESERVED
        )
    }

    pub(super) fn reserve_activation(&self) -> bool {
        self.state
            .lifecycle
            .compare_exchange(
                CONTEXT_STAGED,
                CONTEXT_ACTIVATION_RESERVED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn commit_activation_reservation(&self) {
        let committed = self
            .state
            .lifecycle
            .compare_exchange(
                CONTEXT_ACTIVATION_RESERVED,
                CONTEXT_LIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        debug_assert!(
            committed,
            "only the registry transaction that reserved BrowserContext activation may publish it"
        );
    }

    pub(super) fn rollback_activation_reservation(&self) {
        let rolled_back = self
            .state
            .lifecycle
            .compare_exchange(
                CONTEXT_ACTIVATION_RESERVED,
                CONTEXT_STAGED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        debug_assert!(
            rolled_back,
            "only an uncommitted BrowserContext activation reservation may be rolled back"
        );
    }

    pub(super) fn reserve_retirement(&self) -> bool {
        self.state
            .lifecycle
            .compare_exchange(
                CONTEXT_LIVE,
                CONTEXT_RETIREMENT_RESERVED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn commit_retirement_reservation(&self) {
        let committed = self
            .state
            .lifecycle
            .compare_exchange(
                CONTEXT_RETIREMENT_RESERVED,
                CONTEXT_RETIRED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        debug_assert!(
            committed,
            "only the registry transaction that reserved BrowserContext retirement may publish it"
        );
    }

    pub(super) fn rollback_retirement_reservation(&self) {
        let rolled_back = self
            .state
            .lifecycle
            .compare_exchange(
                CONTEXT_RETIREMENT_RESERVED,
                CONTEXT_LIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        debug_assert!(
            rolled_back,
            "only an uncommitted BrowserContext retirement reservation may be rolled back"
        );
    }

    pub(super) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl fmt::Debug for BrowserContextHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserContextHandle")
            .field("instance_id", &self.state.instance_id)
            .field("browser_context_id", &self.browser_context_id())
            .field("live", &self.is_live())
            .field("retired", &self.is_retired())
            .finish()
    }
}

impl PartialEq for BrowserContextHandle {
    fn eq(&self, other: &Self) -> bool {
        self.state.instance_id == other.state.instance_id
    }
}

impl Eq for BrowserContextHandle {}

impl Hash for BrowserContextHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.state.instance_id.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_public_id_does_not_alias_context_instance() {
        let first = BrowserContextHandle::staged("context-1");
        let second = BrowserContextHandle::staged("context-1");

        assert_ne!(first, second);
        assert!(first.same_instance(&first.clone()));
        assert!(!first.same_instance(&second));
    }

    #[test]
    fn core_lifecycle_is_exact_once_and_reservations_remain_observably_live() {
        let handle = BrowserContextHandle::staged("context-1");

        assert!(handle.is_staged());
        assert!(handle.reserve_activation());
        assert!(handle.is_staged());
        handle.commit_activation_reservation();
        assert!(handle.is_live());
        assert!(!handle.reserve_activation());
        assert!(handle.reserve_retirement());
        assert!(handle.is_live());
        handle.commit_retirement_reservation();
        assert!(handle.is_retired());
        assert!(!handle.reserve_retirement());
    }

    #[test]
    fn uncommitted_lifecycle_reservations_restore_the_observable_state() {
        let handle = BrowserContextHandle::staged("context-1");

        assert!(handle.reserve_activation());
        handle.rollback_activation_reservation();
        assert!(handle.is_staged());

        assert!(handle.reserve_activation());
        handle.commit_activation_reservation();
        assert!(handle.reserve_retirement());
        handle.rollback_retirement_reservation();
        assert!(handle.is_live());
    }
}
