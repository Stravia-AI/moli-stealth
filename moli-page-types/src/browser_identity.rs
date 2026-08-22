use std::{
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_BROWSER_ACTION_ID: AtomicU64 = AtomicU64::new(1);

/// Protocol-neutral identity of one browser action.
///
/// Renderer intents allocate this identity before publication. Browser- and
/// frontend-originated actions use the same identity space, so diagnostics can
/// follow an action across renderer output, Browser Owner admission, request
/// replacement, and frontend projection without using a CDP command/session
/// id as authority.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BrowserActionId(NonZeroU64);

impl BrowserActionId {
    pub fn allocate() -> Self {
        let raw = NEXT_BROWSER_ACTION_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("browser action id exhausted"));
        Self(
            NonZeroU64::new(raw)
                .unwrap_or_else(|| panic!("browser action id allocator returned zero")),
        )
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn action_ids_are_nonzero_distinct_and_keep_the_option_niche() {
        let first = BrowserActionId::allocate();
        let second = BrowserActionId::allocate();

        assert_ne!(first, second);
        assert_ne!(first.get(), 0);
        assert_eq!(
            size_of::<Option<BrowserActionId>>(),
            size_of::<BrowserActionId>()
        );
    }
}
