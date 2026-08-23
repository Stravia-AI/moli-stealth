use super::*;

#[derive(Debug)]
struct BrowserPageResidenceRegistration {
    owner: BrowserPageOwnerKey,
    handle: BrowserPageResidenceHandle,
}

/// Exact Page-side staging for an initial BrowserContext topology.
///
/// Entries are reserved in the registry but remain invisible to live Page
/// lookups while staged. The Browser Owner publishes them immediately before
/// publishing the matching Target lifecycle in the same synchronous turn.
#[derive(Debug)]
pub(in crate::browser_host::navigation_owner) struct BrowserPageContextRegistrationTransaction {
    registrations: Vec<BrowserPageResidenceRegistration>,
}

/// Exact Page-side staging for one new Target.
#[derive(Debug)]
pub(in crate::browser_host::navigation_owner) struct BrowserPageTargetRegistrationTransaction {
    registration: BrowserPageResidenceRegistration,
}

impl BrowserPageTargetRegistrationTransaction {
    #[cfg(test)]
    pub(super) fn handle(&self) -> &BrowserPageResidenceHandle {
        &self.registration.handle
    }
}

impl BrowserPageResidenceRegistry {
    pub(in crate::browser_host::navigation_owner) fn begin_context_registration(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        browser_context_id: &BrowserContextId,
        projection: &BrowserTargetTopologyProjection,
    ) -> Result<BrowserPageContextRegistrationTransaction, BrowserPageResidenceRegistryError> {
        self.validate_context_registration(runtimes, browser_context_id, projection)?;
        let registrations = projection
            .slots()
            .map(|slot| BrowserPageResidenceRegistration {
                owner: BrowserPageOwnerKey::new(browser_context_id.as_str(), slot.target_id()),
                handle: slot.page_residence_handle().clone(),
            })
            .collect::<Vec<_>>();

        for (index, registration) in registrations.iter().enumerate() {
            let runtime = runtimes
                .entries
                .entry(registration.owner.clone())
                .or_default();
            if runtime.page_residence.is_some() {
                let mut exact = true;
                for staged in registrations[..index].iter().rev() {
                    exact &= self.remove_staged_exact(runtimes, staged);
                }
                debug_assert!(
                    exact,
                    "same-turn Page context registration rejection must remove every staged entry"
                );
                return Err(BrowserPageResidenceRegistryError::DuplicateTarget(
                    registration.owner.clone(),
                ));
            }
            runtime.page_residence = Some(BrowserPageResidenceRecord::staged(
                registration.handle.clone(),
            ));
        }

        Ok(BrowserPageContextRegistrationTransaction { registrations })
    }

    pub(in crate::browser_host::navigation_owner) fn rollback_context_registration(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        transaction: BrowserPageContextRegistrationTransaction,
    ) -> bool {
        let mut exact = true;
        for registration in transaction.registrations.iter().rev() {
            exact &= self.remove_staged_exact(runtimes, registration);
        }
        debug_assert!(
            exact,
            "same-turn BrowserContext Page registration rollback must remove every exact staged entry"
        );
        exact
    }

    pub(in crate::browser_host::navigation_owner) fn commit_context_registration(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        transaction: BrowserPageContextRegistrationTransaction,
    ) {
        let exact = transaction
            .registrations
            .iter()
            .all(|registration| self.is_staged_exact(runtimes, registration));
        if exact {
            for registration in &transaction.registrations {
                let published = self
                    .record_mut(runtimes, &registration.owner)
                    .is_some_and(|record| record.publish_if_staged_exact(&registration.handle));
                debug_assert!(published, "validated Page registration must publish");
            }
        }
        debug_assert!(
            exact,
            "only the Browser Owner transaction that staged every Page residence may publish it"
        );
    }

    pub(in crate::browser_host::navigation_owner) fn begin_target_registration(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        owner: BrowserPageOwnerKey,
    ) -> Result<BrowserPageTargetRegistrationTransaction, BrowserPageResidenceRegistryError> {
        let handle = BrowserPageResidenceHandle::default();
        let runtime = runtimes.entries.entry(owner.clone()).or_default();
        if runtime.page_residence.is_some() {
            return Err(BrowserPageResidenceRegistryError::DuplicateTarget(owner));
        }
        runtime.page_residence = Some(BrowserPageResidenceRecord::staged(handle.clone()));
        Ok(BrowserPageTargetRegistrationTransaction {
            registration: BrowserPageResidenceRegistration { owner, handle },
        })
    }

    pub(in crate::browser_host::navigation_owner) fn rollback_target_registration(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        transaction: BrowserPageTargetRegistrationTransaction,
    ) -> bool {
        let exact = self.remove_staged_exact(runtimes, &transaction.registration);
        debug_assert!(
            exact,
            "same-turn Target Page registration rollback must remove its exact staged entry"
        );
        exact
    }

    pub(in crate::browser_host::navigation_owner) fn commit_target_registration(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        transaction: BrowserPageTargetRegistrationTransaction,
    ) -> BrowserPageResidenceHandle {
        let registration = transaction.registration;
        let published = self
            .record_mut(runtimes, &registration.owner)
            .is_some_and(|record| record.publish_if_staged_exact(&registration.handle));
        debug_assert!(
            published,
            "only the Browser Owner transaction that staged a Page residence may publish it"
        );
        registration.handle
    }

    fn is_staged_exact(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        registration: &BrowserPageResidenceRegistration,
    ) -> bool {
        self.record(runtimes, &registration.owner)
            .is_some_and(|record| record.is_staged_exact(&registration.handle))
    }

    fn remove_staged_exact(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        registration: &BrowserPageResidenceRegistration,
    ) -> bool {
        let exact = self
            .record(runtimes, &registration.owner)
            .is_some_and(|record| record.is_staged_exact(&registration.handle));
        if exact {
            let removed = runtimes
                .entries
                .get_mut(&registration.owner)
                .and_then(|runtime| runtime.page_residence.take());
            debug_assert!(removed.is_some());
            runtimes.prune_empty();
        }
        exact
    }
}
