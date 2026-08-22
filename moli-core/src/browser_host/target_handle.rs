use std::{
    fmt,
    hash::{Hash, Hasher},
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
};

use super::BrowserTargetId;

static NEXT_TARGET_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

const TARGET_STAGED: u8 = 0;
const TARGET_LIVE: u8 = 1;
const TARGET_RETIRED: u8 = 2;
const TARGET_ACTIVATION_RESERVED: u8 = 3;
const TARGET_RETIREMENT_RESERVED: u8 = 4;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct BrowserTargetInstanceId(NonZeroU64);

impl BrowserTargetInstanceId {
    fn allocate() -> Self {
        let raw = NEXT_TARGET_INSTANCE_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("browser Target instance id exhausted"));
        Self(
            NonZeroU64::new(raw)
                .unwrap_or_else(|| panic!("browser Target instance allocator returned zero")),
        )
    }
}

struct BrowserTargetHandleState {
    instance_id: BrowserTargetInstanceId,
    target_id: BrowserTargetId,
    lifecycle: AtomicU8,
}

/// Stable capability for one browser Target instance.
///
/// A physical active/background slot moves this handle with its payload. Only
/// Browser Core may activate a staged handle or retire a live handle, so a
/// stale physical slot cannot become authoritative merely by reusing the same
/// public Target id.
#[derive(Clone)]
pub struct BrowserTargetHandle {
    state: Arc<BrowserTargetHandleState>,
}

impl BrowserTargetHandle {
    /// Allocates a staged Target capability.
    ///
    /// Constructing the handle does not make a Target live. Browser Core must
    /// accept it through a context/Target registration transaction first.
    pub fn staged(target_id: impl Into<String>) -> Self {
        Self {
            state: Arc::new(BrowserTargetHandleState {
                instance_id: BrowserTargetInstanceId::allocate(),
                target_id: BrowserTargetId::new(target_id),
                lifecycle: AtomicU8::new(TARGET_STAGED),
            }),
        }
    }

    pub fn target_id(&self) -> &str {
        self.state.target_id.as_str()
    }

    pub fn is_live(&self) -> bool {
        matches!(
            self.state.lifecycle.load(Ordering::Acquire),
            TARGET_LIVE | TARGET_RETIREMENT_RESERVED
        )
    }

    pub fn is_retired(&self) -> bool {
        self.state.lifecycle.load(Ordering::Acquire) == TARGET_RETIRED
    }

    pub(super) fn is_staged(&self) -> bool {
        matches!(
            self.state.lifecycle.load(Ordering::Acquire),
            TARGET_STAGED | TARGET_ACTIVATION_RESERVED
        )
    }

    /// Reserves the staged-to-live transition for one synchronous Browser
    /// Owner transaction. Public lifecycle observers continue to see a staged
    /// handle until the registry publishes the commit.
    pub(super) fn reserve_activation(&self) -> bool {
        self.state
            .lifecycle
            .compare_exchange(
                TARGET_STAGED,
                TARGET_ACTIVATION_RESERVED,
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
                TARGET_ACTIVATION_RESERVED,
                TARGET_LIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        debug_assert!(
            committed,
            "only the registry transaction that reserved Target activation may publish it"
        );
    }

    pub(super) fn rollback_activation_reservation(&self) {
        let rolled_back = self
            .state
            .lifecycle
            .compare_exchange(
                TARGET_ACTIVATION_RESERVED,
                TARGET_STAGED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        debug_assert!(
            rolled_back,
            "only an uncommitted Target activation reservation may be rolled back"
        );
    }

    /// Reserves the live-to-retired transition while keeping the handle
    /// observably live until the matching registry topology commit.
    pub(super) fn reserve_retirement(&self) -> bool {
        self.state
            .lifecycle
            .compare_exchange(
                TARGET_LIVE,
                TARGET_RETIREMENT_RESERVED,
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
                TARGET_RETIREMENT_RESERVED,
                TARGET_RETIRED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        debug_assert!(
            committed,
            "only the registry transaction that reserved Target retirement may publish it"
        );
    }

    pub(super) fn rollback_retirement_reservation(&self) {
        let rolled_back = self
            .state
            .lifecycle
            .compare_exchange(
                TARGET_RETIREMENT_RESERVED,
                TARGET_LIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        debug_assert!(
            rolled_back,
            "only an uncommitted Target retirement reservation may be rolled back"
        );
    }

    pub(super) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl From<String> for BrowserTargetHandle {
    fn from(target_id: String) -> Self {
        Self::staged(target_id)
    }
}

impl From<&str> for BrowserTargetHandle {
    fn from(target_id: &str) -> Self {
        Self::staged(target_id)
    }
}

impl fmt::Debug for BrowserTargetHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserTargetHandle")
            .field("instance_id", &self.state.instance_id)
            .field("target_id", &self.target_id())
            .field("live", &self.is_live())
            .field("retired", &self.is_retired())
            .finish()
    }
}

impl PartialEq for BrowserTargetHandle {
    fn eq(&self, other: &Self) -> bool {
        self.state.instance_id == other.state.instance_id
    }
}

impl Eq for BrowserTargetHandle {}

impl Hash for BrowserTargetHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.state.instance_id.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_public_id_does_not_alias_target_instance() {
        let first = BrowserTargetHandle::staged("TID-1");
        let second = BrowserTargetHandle::staged("TID-1");

        assert_ne!(first, second);
        assert!(first.same_instance(&first.clone()));
        assert!(!first.same_instance(&second));
    }

    #[test]
    fn core_lifecycle_is_exact_once() {
        let handle = BrowserTargetHandle::staged("TID-1");

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
        let handle = BrowserTargetHandle::staged("TID-1");

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
