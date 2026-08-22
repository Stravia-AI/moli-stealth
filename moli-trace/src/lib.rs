//! Shared debug and investigation trace switches.
//!
//! These flags are intentionally process-global and cached after first read:
//! they are diagnostics controls, not dynamic runtime configuration.

mod browser_owner;
mod dom_binding;
mod flags;
mod promise_hook;

pub use browser_owner::{
    BrowserOwnerTraceDocument, BrowserOwnerTraceRecord, emit_browser_owner_trace_record,
};
pub use dom_binding::{
    DomBindingOperationStats, record_dom_binding_operation, take_dom_binding_operation_stats,
};
pub use flags::*;
pub use promise_hook::{
    PromiseHookStats, record_promise_hook_init, record_promise_hook_resolve,
    record_promise_reaction_after, record_promise_reaction_before, take_promise_hook_stats,
};

#[cfg(test)]
mod tests;
