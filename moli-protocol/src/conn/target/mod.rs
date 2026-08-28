mod control;
mod graph;
mod host;
mod observer;
mod projection;
mod registry;
mod session;
mod transaction;

pub(crate) use control::TargetControlPlane;
pub(crate) use observer::{TargetHandlerStore, target_destroyed_automation_events};
pub(crate) use registry::{TargetClosurePlan, TargetHostDelta, TargetRegistry};
pub(crate) use session::{
    CommittedAttachSession, DetachedTargetSession, PreparedAttachSession, TargetSessionRegistry,
};
pub(crate) use transaction::{
    PreparedTargetAttach, PreparedTargetHostClosure, PreparedTargetHostDelta,
    TargetAttachRollbackPlan, TargetAttachSessionCommit, TargetAutoAttachedSessionDetachPlan,
    TargetBindingCleanupAction, TargetBindingCleanupPlan, TargetClosureCleanupPlan,
    TargetEventPlan, TargetSessionDetachCleanupPlan,
};
