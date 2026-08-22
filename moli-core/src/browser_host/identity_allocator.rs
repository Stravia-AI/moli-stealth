use std::{
    cell::Cell,
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use super::BrowserCommandId;

/// Process-shareable source for public Browser Target identities.
///
/// A server may expose more than one Browser Host while keeping all Target
/// ids in one DevTools discovery namespace. The application therefore owns
/// and clones this source into each participating Host; frontend connections
/// never own or reset it.
#[derive(Clone, Debug, Default)]
pub struct BrowserTargetIdAllocator {
    next_sequence: Arc<AtomicU64>,
}

impl BrowserTargetIdAllocator {
    fn allocate_sequence(&self) -> u64 {
        // Relaxed is intentional: this atomic carries uniqueness only.
        // Browser Host mailbox/commit boundaries publish the payloads that
        // later use the allocated identity.
        self.next_sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .expect("Browser Target id space exhausted")
            + 1
    }
}

/// Monotonic identity namespace owned by one Browser Host.
#[derive(Debug)]
pub(crate) struct BrowserHostIdentityState {
    next_browser_context_sequence: Cell<u64>,
    next_browser_command_sequence: Cell<u64>,
    target_ids: BrowserTargetIdAllocator,
}

impl BrowserHostIdentityState {
    pub(crate) fn new(target_ids: BrowserTargetIdAllocator) -> Self {
        Self {
            next_browser_context_sequence: Cell::new(0),
            next_browser_command_sequence: Cell::new(0),
            target_ids,
        }
    }

    pub(crate) fn allocate_browser_context_sequence(&self) -> u64 {
        next_nonzero_sequence(&self.next_browser_context_sequence)
    }

    pub(crate) fn allocate_target_sequence(&self) -> u64 {
        self.target_ids.allocate_sequence()
    }

    pub(crate) fn allocate_browser_command_id(&self) -> BrowserCommandId {
        let sequence = next_nonzero_sequence(&self.next_browser_command_sequence);
        let nonzero = NonZeroU64::new(sequence).unwrap_or(NonZeroU64::MIN);
        BrowserCommandId::new(nonzero)
    }
}

fn next_nonzero_sequence(sequence: &Cell<u64>) -> u64 {
    let next = sequence
        .get()
        .checked_add(1)
        .expect("Browser Host identity space exhausted");
    sequence.set(next);
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_target_allocator_keeps_one_shared_sequence() {
        let first = BrowserTargetIdAllocator::default();
        let second = first.clone();

        assert_eq!(first.allocate_sequence(), 1);
        assert_eq!(second.allocate_sequence(), 2);
        assert_eq!(first.allocate_sequence(), 3);
    }

    #[test]
    fn host_context_and_command_sequences_are_independent() {
        let identities = BrowserHostIdentityState::new(BrowserTargetIdAllocator::default());

        assert_eq!(identities.allocate_browser_context_sequence(), 1);
        assert_eq!(identities.allocate_browser_command_id().get(), 1);
        assert_eq!(identities.allocate_browser_context_sequence(), 2);
        assert_eq!(identities.allocate_browser_command_id().get(), 2);
    }

    #[test]
    #[should_panic(expected = "Browser Target id space exhausted")]
    fn target_sequence_never_wraps() {
        let identities = BrowserTargetIdAllocator {
            next_sequence: Arc::new(AtomicU64::new(u64::MAX)),
        };

        let _ = identities.allocate_sequence();
    }

    #[test]
    #[should_panic(expected = "Browser Host identity space exhausted")]
    fn host_sequence_never_wraps() {
        let identities = BrowserHostIdentityState {
            next_browser_context_sequence: Cell::new(u64::MAX),
            next_browser_command_sequence: Cell::new(0),
            target_ids: BrowserTargetIdAllocator::default(),
        };

        let _ = identities.allocate_browser_context_sequence();
    }
}
