// Compatibility names for the protocol facade. The authoritative history
// state and all cursor/update operations are owned by Browser Core.
#[cfg(test)]
pub(crate) use moli_core::browser_host::BrowserNavigationHistory as TargetNavigationHistoryState;
pub use moli_core::browser_host::{
    BrowserNavigationHistoryEntry as PageNavigationHistoryEntry,
    BrowserNavigationHistoryUpdate as PendingNavigationHistoryUpdate,
};
