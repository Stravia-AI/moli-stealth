use super::navigation_window::child_browsing_context_handle_for_runtime_owner;
use super::*;

/// Whether the current Document owns a committed session-history item that
/// can be used as the source of a reload.
///
/// An initial-empty child Document is installed synchronously before its first
/// real navigation and deliberately has no committed history item. Reloading
/// it must therefore leave any deferred iframe attribute navigation alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NavigationReloadAdmission {
    NoCommittedHistoryItem,
    Admitted,
}

pub(super) fn navigation_reload_admission<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    owner: v8::Local<'s, v8::Object>,
) -> NavigationReloadAdmission {
    let Some(handle) = child_browsing_context_handle_for_runtime_owner(scope, owner) else {
        return NavigationReloadAdmission::Admitted;
    };
    let Some(host_ptr) = context_host_ptr_from_global_bridge(scope) else {
        return NavigationReloadAdmission::Admitted;
    };
    if unsafe { &*host_ptr }.child_current_document_is_initial_empty(handle) {
        NavigationReloadAdmission::NoCommittedHistoryItem
    } else {
        NavigationReloadAdmission::Admitted
    }
}
