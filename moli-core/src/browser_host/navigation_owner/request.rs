use std::{
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::browser_host::BrowserTargetId;

static NEXT_BROWSER_NAVIGATION_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Browser-owned identity of one navigation request.
///
/// This identity is independent of CDP/BiDi command ids and frontend
/// sessions. Redirects, renderer attachment work, and late completions use it
/// to decide whether they still belong to the same browser request.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BrowserNavigationRequestId(NonZeroU64);

impl BrowserNavigationRequestId {
    fn allocate() -> Self {
        let raw = NEXT_BROWSER_NAVIGATION_REQUEST_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("browser navigation request id exhausted"));
        Self(
            NonZeroU64::new(raw)
                .unwrap_or_else(|| panic!("browser navigation request id allocator returned zero")),
        )
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// Exact browser transaction for one cross-document navigation.
///
/// The token deliberately contains no protocol session or command identity.
/// Frontends may correlate it with their own pending command state, but that
/// correlation cannot affect request equality or completion authorization.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct BrowserDocumentNavigation {
    target_id: BrowserTargetId,
    loader_id: String,
    request_id: BrowserNavigationRequestId,
}

/// Protocol-neutral terminal reason for one accepted cross-Document request
/// that did not commit a successor Document.
///
/// Frontend response shapes and command/session ids deliberately do not live
/// here. The diagnostic text is the browser/runtime failure that ended the
/// request and may be projected differently by CDP, BiDi, or Classic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserNavigationFailure {
    Network {
        error_text: String,
    },
    Commit {
        error_text: String,
    },
    Canceled {
        error_text: String,
    },
    Superseded {
        replacement: BrowserDocumentNavigation,
    },
    TargetCrashed,
    TargetClosed,
}

impl BrowserDocumentNavigation {
    pub fn new(target_id: impl Into<String>, loader_id: impl Into<String>) -> Self {
        Self {
            target_id: BrowserTargetId::new(target_id),
            loader_id: loader_id.into(),
            request_id: BrowserNavigationRequestId::allocate(),
        }
    }

    pub fn target_id(&self) -> &str {
        self.target_id.as_str()
    }

    pub fn loader_id(&self) -> &str {
        &self.loader_id
    }

    pub fn request_id(&self) -> BrowserNavigationRequestId {
        self.request_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn request_identity_is_nonzero_distinct_and_frontend_neutral() {
        let first = BrowserDocumentNavigation::new("target-1", "loader-1");
        let second = BrowserDocumentNavigation::new("target-1", "loader-1");

        assert_ne!(first, second);
        assert_ne!(first.request_id(), second.request_id());
        assert_ne!(first.request_id().get(), 0);
        assert_eq!(first.target_id(), "target-1");
        assert_eq!(first.loader_id(), "loader-1");
    }

    #[test]
    fn optional_request_id_preserves_the_nonzero_niche() {
        assert_eq!(
            size_of::<Option<BrowserNavigationRequestId>>(),
            size_of::<BrowserNavigationRequestId>()
        );
    }
}
