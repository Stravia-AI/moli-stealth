//! Stylesheet processing and connected-load runtime owned by `DocumentRuntime`.
//!
//! This module is a child of `document_runtime` so it can use the runtime's
//! private owner façade without widening DOM, V8, or StyleEngine internals.

use super::*;
use crate::modulepreload::{
    ModulepreloadAsState, modulepreload_as_state, modulepreload_fetch_candidate,
    modulepreload_media_matches,
};

mod attributes;
mod blocking;
mod client_index;
mod completion;
mod connected;
mod link_state;
mod load;
mod owner_state;
mod source_install;
#[cfg(test)]
mod test_driver;

pub(crate) use attributes::attribute_reprocesses_connected_stylesheet;
pub(crate) use blocking::OwnerlessStylesheetAdmissionError;
pub(super) use client_index::StylesheetLinkClientIndex;
pub(crate) use completion::{ConnectedLoadCompletion, LiveStylesheetImportLoadCompletion};
pub(crate) use connected::{
    ConnectedStyleLoadPrimeResult, PreparedConnectedStyleLoad,
    fetch_complete_stylesheet_import_graph,
};
pub(super) use link_state::LinkStyleState;
pub(crate) use load::StylesheetLinkClientTerminal;
pub(super) use load::{
    ConnectedLinkReadinessFetchOptions, ConnectedLoadOperation, ConnectedLoadParameters,
    ConnectedStyleImportSource, QueuedConnectedStyleLoad, ReadyConnectedStyleLoadOperation,
    StylesheetLinkClient,
};
pub(crate) use load::{ConnectedStyleEventElementKind, ReadyConnectedStyleLoad};
pub(crate) use load::{ConnectedStyleLoadEventAdmission, ConnectedStyleLoadEventPlan};
pub(crate) use load::{NativeModulepreloadLinkFetchOutcome, PendingNativeModulepreloadLinkEvent};
pub(super) use owner_state::{StylesheetOwnerCspDisposition, StylesheetOwnerRuntimeStates};
pub(crate) use source_install::{InstallLinkedStylesheet, PreparedLinkedStylesheetResource};

pub(super) fn is_declarative_css_module_style_element(
    element: &crate::dom::native::Element,
) -> bool {
    element.is_html_element("style")
        && element
            .attribute("type")
            .is_some_and(|value| value.eq_ignore_ascii_case("module"))
}

pub(super) fn is_inline_style_element(element: &crate::dom::native::Element) -> bool {
    element.is_inline_style_element()
}
