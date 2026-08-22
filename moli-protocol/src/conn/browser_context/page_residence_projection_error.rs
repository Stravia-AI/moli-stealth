use super::TargetProjectionError;

/// A physical Page participant was rejected before Browser Core changed the
/// authoritative Page residence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PageResidenceProjectionError {
    TargetTopology(TargetProjectionError),
    PhysicalResidenceMismatch {
        browser_context_id: String,
        target_id: String,
    },
    InitialDocumentPageAlreadyPresent {
        browser_context_id: String,
        target_id: String,
    },
    RendererPageOwnerMissing {
        browser_context_id: String,
        target_id: String,
    },
}

impl std::fmt::Display for PageResidenceProjectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TargetTopology(error) => error.fmt(formatter),
            Self::PhysicalResidenceMismatch {
                browser_context_id,
                target_id,
            } => write!(
                formatter,
                "physical Page residence for Target {target_id:?} in BrowserContext {browser_context_id:?} does not match the prepared Browser Core permit"
            ),
            Self::InitialDocumentPageAlreadyPresent {
                browser_context_id,
                target_id,
            } => write!(
                formatter,
                "initial Document materialization for Target {target_id:?} in BrowserContext {browser_context_id:?} requires an empty physical Page slot"
            ),
            Self::RendererPageOwnerMissing {
                browser_context_id,
                target_id,
            } => write!(
                formatter,
                "Page candidate for Target {target_id:?} in BrowserContext {browser_context_id:?} has no renderer lifetime owner"
            ),
        }
    }
}

impl std::error::Error for PageResidenceProjectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TargetTopology(error) => Some(error),
            Self::PhysicalResidenceMismatch { .. }
            | Self::InitialDocumentPageAlreadyPresent { .. }
            | Self::RendererPageOwnerMissing { .. } => None,
        }
    }
}

impl From<TargetProjectionError> for PageResidenceProjectionError {
    fn from(error: TargetProjectionError) -> Self {
        Self::TargetTopology(error)
    }
}
