use moli_core::browser_host::BrowserTargetRegistryError;

use crate::devtools_runtime::{DevToolsError, DevToolsErrorKind};

/// A Browser Core Target transaction or its same-turn physical projection was
/// rejected before the two registries could diverge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TargetProjectionError {
    Core(BrowserTargetRegistryError),
    PhysicalTargetCountMismatch {
        authoritative: usize,
        projected: usize,
    },
    PhysicalContextTargetCountMismatch {
        browser_context_id: String,
        authoritative: usize,
        projected: usize,
    },
    PhysicalActiveTargetMismatch {
        browser_context_id: String,
        authoritative: Option<String>,
        projected: Option<String>,
    },
    DuplicatePhysicalTarget(String),
    PhysicalTargetContextMismatch {
        target_id: String,
        authoritative: Option<String>,
        projected: String,
    },
    PhysicalTargetHandleMismatch {
        browser_context_id: String,
        target_id: String,
    },
    PhysicalPageResidenceMismatch {
        browser_context_id: String,
        target_id: String,
    },
    PhysicalBrowserContextHandleMismatch(String),
    ForeignBrowserTopLevelTargetSnapshot,
    StaleBrowserContextTargetSnapshot(String),
    StaleTopLevelTargetSnapshot {
        browser_context_id: String,
        target_id: String,
    },
    PhysicalBrowserContextMissing(String),
    PhysicalTargetMissing {
        browser_context_id: String,
        target_id: String,
    },
}

impl std::fmt::Display for TargetProjectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Core(error) => error.fmt(formatter),
            Self::PhysicalTargetCountMismatch {
                authoritative,
                projected,
            } => write!(
                formatter,
                "Browser Core has {authoritative} Targets but the physical projection has {projected}"
            ),
            Self::PhysicalContextTargetCountMismatch {
                browser_context_id,
                authoritative,
                projected,
            } => write!(
                formatter,
                "BrowserContext {browser_context_id:?} has {authoritative} Core Targets but {projected} physical Targets"
            ),
            Self::PhysicalActiveTargetMismatch {
                browser_context_id,
                authoritative,
                projected,
            } => write!(
                formatter,
                "BrowserContext {browser_context_id:?} has authoritative active Target {authoritative:?} but physical active Target {projected:?}"
            ),
            Self::DuplicatePhysicalTarget(target_id) => write!(
                formatter,
                "physical Target projection contains duplicate identity {target_id:?}"
            ),
            Self::PhysicalTargetContextMismatch {
                target_id,
                authoritative,
                projected,
            } => write!(
                formatter,
                "physical Target {target_id:?} belongs to BrowserContext {projected:?}, but Browser Core reports {authoritative:?}"
            ),
            Self::PhysicalTargetHandleMismatch {
                browser_context_id,
                target_id,
            } => write!(
                formatter,
                "physical Target {target_id:?} in BrowserContext {browser_context_id:?} does not carry the exact live Browser Core handle"
            ),
            Self::PhysicalPageResidenceMismatch {
                browser_context_id,
                target_id,
            } => write!(
                formatter,
                "physical Target {target_id:?} in BrowserContext {browser_context_id:?} does not carry the exact Browser Core Page residence"
            ),
            Self::PhysicalBrowserContextHandleMismatch(browser_context_id) => write!(
                formatter,
                "physical BrowserContext {browser_context_id:?} does not carry the exact Browser Core handle from the current-state snapshot"
            ),
            Self::ForeignBrowserTopLevelTargetSnapshot => write!(
                formatter,
                "top-level Target current-state snapshot belongs to another Browser instance"
            ),
            Self::StaleBrowserContextTargetSnapshot(browser_context_id) => write!(
                formatter,
                "BrowserContext current-state snapshot for {browser_context_id:?} is no longer live"
            ),
            Self::StaleTopLevelTargetSnapshot {
                browser_context_id,
                target_id,
            } => write!(
                formatter,
                "top-level Target current-state snapshot for {target_id:?} in BrowserContext {browser_context_id:?} is no longer live"
            ),
            Self::PhysicalBrowserContextMissing(browser_context_id) => write!(
                formatter,
                "BrowserContext {browser_context_id:?} has no exact physical Target projection"
            ),
            Self::PhysicalTargetMissing {
                browser_context_id,
                target_id,
            } => write!(
                formatter,
                "Target {target_id:?} has no physical payload in BrowserContext {browser_context_id:?}"
            ),
        }
    }
}

impl std::error::Error for TargetProjectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Core(error) => Some(error),
            _ => None,
        }
    }
}

impl From<BrowserTargetRegistryError> for TargetProjectionError {
    fn from(error: BrowserTargetRegistryError) -> Self {
        Self::Core(error)
    }
}

impl From<TargetProjectionError> for DevToolsError {
    fn from(error: TargetProjectionError) -> Self {
        Self::new(
            DevToolsErrorKind::Internal,
            format!("TargetProjectionRejected: {error}"),
        )
    }
}
