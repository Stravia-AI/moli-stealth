use crate::conn::{BrowserContext, CdpConnection, CdpSessionRoute, TargetPageSessionState};
use crate::domains::audits::SessionOwnerAuditsEnableResult;
use crate::domains::log::{SessionOwnerLogControlResult, SessionOwnerLogEnableResult};
use serde_json::Value;

mod emulation_owner;
mod engine_factory;
mod fetch_owner;
mod lifecycle;
mod loaded_page_projection;
mod lookup;
mod network_owner;
mod page_owner;
mod page_residence_projection;
mod page_residence_projection_error;
mod registry_projection;
mod runtime_owner;
mod session_owner;
mod target_context_projection;
mod target_projection_error;
mod target_registry_projection;
mod target_session_owner;
mod target_snapshot_projection;
mod target_termination_projection;
mod target_topology_projection;

use engine_factory::BrowserEngineReplacementInputs;

pub(crate) use target_projection_error::TargetProjectionError;
pub(crate) use target_termination_projection::{
    ProjectedActiveTargetClose, ProjectedClosedPageTarget,
};

pub(crate) use emulation_owner::TargetEmulationSessionStateMut;
pub(crate) use page_owner::PageLifecycleEventsEnableResult;
pub(crate) use runtime_owner::{
    SessionOwnerInspectorEnableResult, SessionOwnerRuntimeFrontendEnableResult,
};
pub(crate) use session_owner::CdpSessionRoute;
pub(crate) use target_session_owner::{
    ClosedPageTarget, TargetLoadedNavigationCommitState, TargetNavigationLoadInputs,
};
