use crate::browser_host::{BrowserContextId, BrowserTargetId};

/// Protocol-neutral key for the strong renderer/navigation owner parked behind
/// one browser Target.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrowserPageOwnerKey {
    browser_context_id: BrowserContextId,
    target_id: BrowserTargetId,
}

impl BrowserPageOwnerKey {
    pub fn new(browser_context_id: impl Into<String>, target_id: impl Into<String>) -> Self {
        Self {
            browser_context_id: BrowserContextId::new(browser_context_id),
            target_id: BrowserTargetId::new(target_id),
        }
    }

    pub fn browser_context_id(&self) -> &str {
        self.browser_context_id.as_str()
    }

    pub fn target_id(&self) -> &str {
        self.target_id.as_str()
    }
}

/// Browser-owned fetch policy applied to one active Page runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserPageFetchConfiguration {
    pub browser_identity: moli_browser_profile::BrowserIdentityProfile,
    pub http_proxy: Option<String>,
    pub http_no_proxy: Option<String>,
    pub tls_verify_host: bool,
    pub bypass_service_worker: bool,
}
