use crate::runtime::RendererServiceWorkerRunIdentity;

use super::ids::ServiceWorkerVersionId;

/// Exact owner of work emitted by one concrete ServiceWorker execution run.
///
/// A version id survives worker restarts, while the renderer run identity does
/// not. Keeping the two values in one carrier prevents asynchronous events and
/// completions from accidentally combining a version with another run.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct ServiceWorkerRunOwner {
    version_id: ServiceWorkerVersionId,
    run: RendererServiceWorkerRunIdentity,
}

impl ServiceWorkerRunOwner {
    pub(crate) fn new(
        version_id: ServiceWorkerVersionId,
        run: RendererServiceWorkerRunIdentity,
    ) -> Self {
        Self { version_id, run }
    }

    pub(crate) fn fresh(version_id: ServiceWorkerVersionId) -> Self {
        Self::new(version_id, RendererServiceWorkerRunIdentity::fresh())
    }

    pub(crate) fn version_id(&self) -> ServiceWorkerVersionId {
        self.version_id
    }

    pub(crate) fn run_identity(&self) -> &RendererServiceWorkerRunIdentity {
        &self.run
    }

    pub(crate) fn cloned_run_identity(&self) -> RendererServiceWorkerRunIdentity {
        self.run.clone()
    }

    pub(crate) fn into_parts(self) -> (ServiceWorkerVersionId, RendererServiceWorkerRunIdentity) {
        (self.version_id, self.run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_binds_version_and_exact_run_as_one_value() {
        let version_id = ServiceWorkerVersionId::from_u64_for_test(7);
        let owner = ServiceWorkerRunOwner::fresh(version_id);

        assert_eq!(owner.version_id(), version_id);
        assert_eq!(owner.run_identity(), &owner.cloned_run_identity());
    }

    #[test]
    fn restarting_a_version_creates_a_distinct_owner() {
        let version_id = ServiceWorkerVersionId::from_u64_for_test(7);

        assert_ne!(
            ServiceWorkerRunOwner::fresh(version_id),
            ServiceWorkerRunOwner::fresh(version_id)
        );
    }
}
