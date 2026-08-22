use super::*;

/// Target-side staging for an initial BrowserContext topology. The registry
/// owns the provisional map entries while engine/context registration decides;
/// the handles remain observably staged until commit.
#[derive(Debug)]
pub(in crate::browser_host::navigation_owner) struct BrowserTargetContextRegistrationTransaction {
    browser_context_id: BrowserContextId,
    target_ids: Vec<BrowserTargetId>,
    handles: Vec<BrowserTargetHandle>,
    session_storage_accesses: Vec<(BrowserTargetId, BrowserTargetSessionStorageAccess)>,
}

/// Target-side staging for BrowserContext removal. Exact topology and reverse
/// owners remain available for same-turn rollback while every handle stays
/// observably live under a retirement reservation.
#[derive(Debug)]
pub(in crate::browser_host::navigation_owner) struct BrowserTargetContextRemovalTransaction {
    browser_context_id: BrowserContextId,
    topology: BrowserContextTargets,
    records: Vec<(BrowserTargetId, BrowserTargetRecord)>,
}

enum BrowserTargetRegistrationTopologyChange {
    Background {
        index: usize,
    },
    Active {
        previous_active: Option<BrowserTargetId>,
        previous_background_index: Option<usize>,
    },
}

/// Reversible new-Target topology and reverse-owner staging around an engine
/// handoff. The new capability is not observably live until commit.
pub(in crate::browser_host::navigation_owner) struct BrowserTargetRegistrationTransaction {
    browser_context_id: BrowserContextId,
    target_id: BrowserTargetId,
    handle: BrowserTargetHandle,
    session_storage_access: BrowserTargetSessionStorageAccess,
    topology_change: BrowserTargetRegistrationTopologyChange,
}

impl BrowserTargetRegistrationTransaction {
    pub(in crate::browser_host::navigation_owner) fn handle(&self) -> &BrowserTargetHandle {
        &self.handle
    }

    pub(in crate::browser_host::navigation_owner) fn previous_active_target_id(
        &self,
    ) -> Option<&BrowserTargetId> {
        match &self.topology_change {
            BrowserTargetRegistrationTopologyChange::Background { .. } => None,
            BrowserTargetRegistrationTopologyChange::Active {
                previous_active, ..
            } => previous_active.as_ref(),
        }
    }

    pub(in crate::browser_host::navigation_owner) fn session_storage_access(
        &self,
    ) -> &BrowserTargetSessionStorageAccess {
        &self.session_storage_access
    }
}

/// Reversible exact active-placeholder replacement. Both lifecycle changes
/// are reserved before the engine handoff and published only after it succeeds.
pub(in crate::browser_host::navigation_owner) struct BrowserTargetReplacementTransaction {
    browser_context_id: BrowserContextId,
    expected_target_id: BrowserTargetId,
    previous_record: BrowserTargetRecord,
    replacement_target_id: BrowserTargetId,
    replacement_handle: BrowserTargetHandle,
    replacement_session_storage_access: BrowserTargetSessionStorageAccess,
}

impl BrowserTargetReplacementTransaction {
    pub(in crate::browser_host::navigation_owner) fn replacement_handle(
        &self,
    ) -> &BrowserTargetHandle {
        &self.replacement_handle
    }

    pub(in crate::browser_host::navigation_owner) fn replacement_session_storage_access(
        &self,
    ) -> &BrowserTargetSessionStorageAccess {
        &self.replacement_session_storage_access
    }
}

/// Background Target removed from its exact vector slot while the engine
/// registry decides the matching activation handoff.
pub(in crate::browser_host::navigation_owner) struct BrowserTargetActivationTransaction {
    browser_context_id: BrowserContextId,
    target_id: BrowserTargetId,
    background_index: usize,
    previous_active: Option<BrowserTargetId>,
    previous_background_index: Option<usize>,
}

/// Reversible removal owned entirely by one synchronous Browser Owner turn.
///
/// Target termination removes topology before advancing the Page generation.
/// Keeping the live handle in this private token lets a stale Page commit put
/// the exact Target back without retiring and later reviving its capability.
pub(in crate::browser_host::navigation_owner) struct BrowserTargetRemovalTransaction {
    target_id: BrowserTargetId,
    browser_context_id: BrowserContextId,
    record: BrowserTargetRecord,
    residence: BrowserTargetResidence,
    background_index: Option<usize>,
}

impl BrowserTargetRegistry {
    pub(in crate::browser_host::navigation_owner) fn begin_context_registration(
        &mut self,
        projection: BrowserTargetTopologyProjection,
        mut target_session_storage_stores: HashMap<BrowserTargetId, SharedWebStorageStore>,
    ) -> Result<BrowserTargetContextRegistrationTransaction, BrowserContextRegistryError> {
        let browser_context_id = projection.browser_context_id.clone();
        self.validate_context_registration(&browser_context_id, &projection)?;

        let handles = projection
            .slots()
            .map(|slot| slot.target_handle().clone())
            .collect::<Vec<_>>();
        for (index, handle) in handles.iter().enumerate() {
            if handle.reserve_activation() {
                continue;
            }
            for reserved in handles[..index].iter().rev() {
                reserved.rollback_activation_reservation();
            }
            return Err(BrowserContextRegistryError::TargetHandleNotStaged(
                BrowserTargetId::new(handle.target_id()),
            ));
        }

        let topology = BrowserContextTargets {
            active: projection
                .active_target
                .as_ref()
                .map(|slot| BrowserTargetId::new(slot.target_id())),
            background: projection
                .background_targets
                .iter()
                .map(|slot| BrowserTargetId::new(slot.target_id()))
                .collect(),
        };
        let target_ids = topology
            .active
            .iter()
            .chain(topology.background.iter())
            .cloned()
            .collect::<Vec<_>>();
        match self.contexts.entry(browser_context_id.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(topology);
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                for handle in handles.iter().rev() {
                    handle.rollback_activation_reservation();
                }
                return Err(BrowserContextRegistryError::DuplicateTargetTopologyContext(
                    browser_context_id,
                ));
            }
        }

        let mut session_storage_accesses = Vec::with_capacity(target_ids.len());
        for ((target_id, handle), inserted_index) in target_ids.iter().zip(handles.iter()).zip(0..)
        {
            let session_storage_store = target_session_storage_stores
                .remove(target_id)
                .unwrap_or_else(crate::network::new_shared_web_storage_store);
            let session_storage_access = BrowserTargetSessionStorageAccess::new(
                handle.clone(),
                session_storage_store.clone(),
            );
            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.owners.entry(target_id.clone())
            {
                entry.insert(BrowserTargetRecord {
                    browser_context_id: browser_context_id.clone(),
                    handle: handle.clone(),
                    session_storage_store,
                });
                session_storage_accesses.push((target_id.clone(), session_storage_access));
                continue;
            }

            self.contexts.remove(&browser_context_id);
            for inserted_target_id in &target_ids[..inserted_index] {
                self.owners.remove(inserted_target_id);
            }
            for reserved in handles.iter().rev() {
                reserved.rollback_activation_reservation();
            }
            return Err(BrowserContextRegistryError::DuplicateBrowserTarget(
                target_id.clone(),
            ));
        }

        Ok(BrowserTargetContextRegistrationTransaction {
            browser_context_id,
            target_ids,
            handles,
            session_storage_accesses,
        })
    }

    pub(in crate::browser_host::navigation_owner) fn rollback_context_registration(
        &mut self,
        transaction: BrowserTargetContextRegistrationTransaction,
    ) -> bool {
        let removed_context = self.contexts.remove(&transaction.browser_context_id);
        let mut exact = removed_context.is_some();
        for target_id in &transaction.target_ids {
            exact &= self.owners.remove(target_id).is_some();
        }
        for handle in transaction.handles.iter().rev() {
            handle.rollback_activation_reservation();
        }
        debug_assert!(
            exact,
            "same-turn BrowserContext Target registration rollback must recover every staged entry"
        );
        exact
    }

    pub(in crate::browser_host::navigation_owner) fn commit_context_registration(
        &mut self,
        transaction: BrowserTargetContextRegistrationTransaction,
    ) -> Vec<(BrowserTargetId, BrowserTargetSessionStorageAccess)> {
        for handle in &transaction.handles {
            handle.commit_activation_reservation();
        }
        transaction.session_storage_accesses
    }

    pub(in crate::browser_host::navigation_owner) fn begin_context_removal(
        &mut self,
        browser_context_id: &BrowserContextId,
    ) -> Result<BrowserTargetContextRemovalTransaction, BrowserContextRegistryError> {
        let Some(topology) = self.contexts.get(browser_context_id).cloned() else {
            return Err(BrowserContextRegistryError::UnknownBrowserContext(
                browser_context_id.clone(),
            ));
        };
        let target_ids = topology
            .active
            .iter()
            .chain(topology.background.iter())
            .cloned()
            .collect::<Vec<_>>();
        let mut handles = Vec::with_capacity(target_ids.len());
        for target_id in &target_ids {
            let Some(record) = self.owners.get(target_id) else {
                return Err(BrowserContextRegistryError::TargetTopologyOwnerMissing {
                    browser_context_id: browser_context_id.clone(),
                    target_id: target_id.clone(),
                });
            };
            if record.browser_context_id != *browser_context_id {
                return Err(
                    BrowserContextRegistryError::TargetTopologyOwnerContextMismatch {
                        browser_context_id: browser_context_id.clone(),
                        target_id: target_id.clone(),
                        actual_browser_context_id: record.browser_context_id.clone(),
                    },
                );
            }
            handles.push(record.handle.clone());
        }

        for (index, handle) in handles.iter().enumerate() {
            if handle.reserve_retirement() {
                continue;
            }
            for reserved in handles[..index].iter().rev() {
                reserved.rollback_retirement_reservation();
            }
            return Err(BrowserContextRegistryError::TargetHandleNotLive(
                BrowserTargetId::new(handle.target_id()),
            ));
        }

        let mut records = Vec::with_capacity(target_ids.len());
        for target_id in &target_ids {
            let Some(record) = self.owners.remove(target_id) else {
                for (removed_target_id, removed_record) in records.drain(..) {
                    self.owners.insert(removed_target_id, removed_record);
                }
                for handle in handles.iter().rev() {
                    handle.rollback_retirement_reservation();
                }
                return Err(BrowserContextRegistryError::TargetTopologyOwnerMissing {
                    browser_context_id: browser_context_id.clone(),
                    target_id: target_id.clone(),
                });
            };
            records.push((target_id.clone(), record));
        }
        let Some(removed_topology) = self.contexts.remove(browser_context_id) else {
            for (target_id, record) in records.drain(..) {
                self.owners.insert(target_id, record);
            }
            for handle in handles.iter().rev() {
                handle.rollback_retirement_reservation();
            }
            return Err(BrowserContextRegistryError::UnknownBrowserContext(
                browser_context_id.clone(),
            ));
        };

        Ok(BrowserTargetContextRemovalTransaction {
            browser_context_id: browser_context_id.clone(),
            topology: removed_topology,
            records,
        })
    }

    pub(in crate::browser_host::navigation_owner) fn rollback_context_removal(
        &mut self,
        transaction: BrowserTargetContextRemovalTransaction,
    ) -> bool {
        let mut exact = match self.contexts.entry(transaction.browser_context_id.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(transaction.topology);
                true
            }
            std::collections::hash_map::Entry::Occupied(_) => false,
        };
        for (target_id, record) in transaction.records {
            record.handle.rollback_retirement_reservation();
            match self.owners.entry(target_id) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(record);
                }
                std::collections::hash_map::Entry::Occupied(_) => exact = false,
            }
        }
        debug_assert!(
            exact,
            "same-turn BrowserContext Target removal rollback must restore exact topology and owners"
        );
        exact
    }

    pub(in crate::browser_host::navigation_owner) fn commit_context_removal(
        &mut self,
        transaction: BrowserTargetContextRemovalTransaction,
    ) -> Vec<BrowserTargetId> {
        transaction
            .records
            .into_iter()
            .map(|(target_id, record)| {
                record.handle.commit_retirement_reservation();
                target_id
            })
            .collect()
    }

    pub(in crate::browser_host::navigation_owner) fn begin_background_registration(
        &mut self,
        browser_context_id: &BrowserContextId,
        target_id: &BrowserTargetId,
        session_storage_store: SharedWebStorageStore,
    ) -> Result<BrowserTargetRegistrationTransaction, BrowserTargetRegistryError> {
        self.begin_target_registration(
            browser_context_id,
            target_id,
            BrowserTargetResidence::Background,
            session_storage_store,
        )
    }

    pub(in crate::browser_host::navigation_owner) fn begin_active_registration(
        &mut self,
        browser_context_id: &BrowserContextId,
        target_id: &BrowserTargetId,
        session_storage_store: SharedWebStorageStore,
    ) -> Result<BrowserTargetRegistrationTransaction, BrowserTargetRegistryError> {
        self.begin_target_registration(
            browser_context_id,
            target_id,
            BrowserTargetResidence::Active,
            session_storage_store,
        )
    }

    fn begin_target_registration(
        &mut self,
        browser_context_id: &BrowserContextId,
        target_id: &BrowserTargetId,
        residence: BrowserTargetResidence,
        session_storage_store: SharedWebStorageStore,
    ) -> Result<BrowserTargetRegistrationTransaction, BrowserTargetRegistryError> {
        self.validate_new_target(browser_context_id, target_id)?;
        let handle = BrowserTargetHandle::staged(target_id.as_str());
        if !handle.reserve_activation() {
            return Err(BrowserTargetRegistryError::TargetHandleNotStaged(
                target_id.clone(),
            ));
        }

        if let std::collections::hash_map::Entry::Occupied(_) = self.owners.entry(target_id.clone())
        {
            handle.rollback_activation_reservation();
            return Err(BrowserTargetRegistryError::DuplicateTarget(
                target_id.clone(),
            ));
        }
        let Some(topology) = self.contexts.get_mut(browser_context_id) else {
            handle.rollback_activation_reservation();
            return Err(BrowserTargetRegistryError::UnknownBrowserContext(
                browser_context_id.clone(),
            ));
        };

        let topology_change = match residence {
            BrowserTargetResidence::Background => {
                let index = topology.background.len();
                topology.background.push(target_id.clone());
                BrowserTargetRegistrationTopologyChange::Background { index }
            }
            BrowserTargetResidence::Active => {
                let previous_active = topology.active.replace(target_id.clone());
                let previous_background_index = previous_active.as_ref().map(|previous| {
                    let index = topology.background.len();
                    topology.background.push(previous.clone());
                    index
                });
                BrowserTargetRegistrationTopologyChange::Active {
                    previous_active,
                    previous_background_index,
                }
            }
        };
        let session_storage_access =
            BrowserTargetSessionStorageAccess::new(handle.clone(), session_storage_store.clone());
        self.owners.insert(
            target_id.clone(),
            BrowserTargetRecord {
                browser_context_id: browser_context_id.clone(),
                handle: handle.clone(),
                session_storage_store,
            },
        );
        Ok(BrowserTargetRegistrationTransaction {
            browser_context_id: browser_context_id.clone(),
            target_id: target_id.clone(),
            handle,
            session_storage_access,
            topology_change,
        })
    }

    pub(in crate::browser_host::navigation_owner) fn rollback_target_registration(
        &mut self,
        transaction: BrowserTargetRegistrationTransaction,
    ) -> bool {
        let removed_owner = self.owners.remove(&transaction.target_id);
        let Some(topology) = self.contexts.get_mut(&transaction.browser_context_id) else {
            transaction.handle.rollback_activation_reservation();
            debug_assert!(
                false,
                "same-turn Target registration rollback lost its context"
            );
            return false;
        };
        let topology_restored = match transaction.topology_change {
            BrowserTargetRegistrationTopologyChange::Background { index } => {
                if topology.background.get(index) == Some(&transaction.target_id) {
                    topology.background.remove(index);
                    true
                } else {
                    false
                }
            }
            BrowserTargetRegistrationTopologyChange::Active {
                previous_active,
                previous_background_index,
            } => {
                let active_matches = topology.active.as_ref() == Some(&transaction.target_id);
                let background_matches = match (&previous_active, previous_background_index) {
                    (Some(previous), Some(index)) => {
                        topology.background.get(index) == Some(previous)
                    }
                    (None, None) => true,
                    _ => false,
                };
                if active_matches && background_matches {
                    topology.active = previous_active;
                    if let Some(index) = previous_background_index {
                        topology.background.remove(index);
                    }
                    true
                } else {
                    false
                }
            }
        };
        transaction.handle.rollback_activation_reservation();
        let exact = removed_owner.is_some() && topology_restored;
        debug_assert!(
            exact,
            "same-turn Target registration rollback must restore exact topology and owner"
        );
        exact
    }

    pub(in crate::browser_host::navigation_owner) fn commit_target_registration(
        &mut self,
        transaction: BrowserTargetRegistrationTransaction,
    ) {
        transaction.handle.commit_activation_reservation();
    }

    pub(in crate::browser_host::navigation_owner) fn begin_active_replacement(
        &mut self,
        browser_context_id: &BrowserContextId,
        expected_target_id: &BrowserTargetId,
        replacement_target_id: &BrowserTargetId,
        replacement_session_storage_store: SharedWebStorageStore,
    ) -> Result<BrowserTargetReplacementTransaction, BrowserTargetRegistryError> {
        let expected_owner =
            BrowserPageOwnerKey::new(browser_context_id.as_str(), expected_target_id.as_str());
        if self.validate_target_owner(&expected_owner)? != BrowserTargetResidence::Active {
            return Err(BrowserTargetRegistryError::TargetIsNotActive(
                expected_owner,
            ));
        }
        self.validate_new_target(browser_context_id, replacement_target_id)?;

        let replacement_handle = BrowserTargetHandle::staged(replacement_target_id.as_str());
        if !replacement_handle.reserve_activation() {
            return Err(BrowserTargetRegistryError::TargetHandleNotStaged(
                replacement_target_id.clone(),
            ));
        }
        let Some(previous_record) = self.owners.get(expected_target_id).cloned() else {
            replacement_handle.rollback_activation_reservation();
            return Err(BrowserTargetRegistryError::UnknownTarget(
                expected_target_id.clone(),
            ));
        };
        if !previous_record.handle.reserve_retirement() {
            replacement_handle.rollback_activation_reservation();
            return Err(BrowserTargetRegistryError::TargetHandleNotLive(
                expected_target_id.clone(),
            ));
        }
        let Some(topology) = self.contexts.get_mut(browser_context_id) else {
            previous_record.handle.rollback_retirement_reservation();
            replacement_handle.rollback_activation_reservation();
            return Err(BrowserTargetRegistryError::UnknownBrowserContext(
                browser_context_id.clone(),
            ));
        };
        if topology.active.as_ref() != Some(expected_target_id) {
            previous_record.handle.rollback_retirement_reservation();
            replacement_handle.rollback_activation_reservation();
            return Err(BrowserTargetRegistryError::TargetIsNotActive(
                expected_owner,
            ));
        }

        let Some(removed) = self.owners.remove(expected_target_id) else {
            previous_record.handle.rollback_retirement_reservation();
            replacement_handle.rollback_activation_reservation();
            return Err(BrowserTargetRegistryError::UnknownTarget(
                expected_target_id.clone(),
            ));
        };
        match self.owners.entry(replacement_target_id.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(BrowserTargetRecord {
                    browser_context_id: browser_context_id.clone(),
                    handle: replacement_handle.clone(),
                    session_storage_store: replacement_session_storage_store.clone(),
                });
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                self.owners.insert(expected_target_id.clone(), removed);
                previous_record.handle.rollback_retirement_reservation();
                replacement_handle.rollback_activation_reservation();
                return Err(BrowserTargetRegistryError::DuplicateTarget(
                    replacement_target_id.clone(),
                ));
            }
        }
        topology.active = Some(replacement_target_id.clone());

        Ok(BrowserTargetReplacementTransaction {
            browser_context_id: browser_context_id.clone(),
            expected_target_id: expected_target_id.clone(),
            previous_record: removed,
            replacement_target_id: replacement_target_id.clone(),
            replacement_session_storage_access: BrowserTargetSessionStorageAccess::new(
                replacement_handle.clone(),
                replacement_session_storage_store,
            ),
            replacement_handle,
        })
    }

    pub(in crate::browser_host::navigation_owner) fn rollback_active_replacement(
        &mut self,
        transaction: BrowserTargetReplacementTransaction,
    ) -> bool {
        let topology_restored = self
            .contexts
            .get_mut(&transaction.browser_context_id)
            .is_some_and(|topology| {
                if topology.active.as_ref() != Some(&transaction.replacement_target_id) {
                    return false;
                }
                topology.active = Some(transaction.expected_target_id.clone());
                true
            });
        let removed_replacement = self.owners.remove(&transaction.replacement_target_id);
        let restored_previous = match self.owners.entry(transaction.expected_target_id.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(transaction.previous_record.clone());
                true
            }
            std::collections::hash_map::Entry::Occupied(_) => false,
        };
        transaction
            .previous_record
            .handle
            .rollback_retirement_reservation();
        transaction
            .replacement_handle
            .rollback_activation_reservation();
        let exact = topology_restored && removed_replacement.is_some() && restored_previous;
        debug_assert!(
            exact,
            "same-turn active Target replacement rollback must restore exact source and topology"
        );
        exact
    }

    pub(in crate::browser_host::navigation_owner) fn commit_active_replacement(
        &mut self,
        transaction: BrowserTargetReplacementTransaction,
    ) {
        transaction
            .previous_record
            .handle
            .commit_retirement_reservation();
        transaction
            .replacement_handle
            .commit_activation_reservation();
    }

    pub(in crate::browser_host::navigation_owner) fn begin_activation(
        &mut self,
        owner: &BrowserPageOwnerKey,
    ) -> Result<BrowserTargetActivationTransaction, BrowserTargetRegistryError> {
        let residence = self.validate_target_owner(owner)?;
        if residence != BrowserTargetResidence::Background {
            return Err(BrowserTargetRegistryError::TargetIsNotBackground(
                owner.clone(),
            ));
        }
        let browser_context_id = BrowserContextId::new(owner.browser_context_id());
        let target_id = BrowserTargetId::new(owner.target_id());
        let Some(topology) = self.contexts.get_mut(&browser_context_id) else {
            return Err(BrowserTargetRegistryError::UnknownBrowserContext(
                browser_context_id,
            ));
        };
        let Some(background_index) = topology
            .background
            .iter()
            .position(|candidate| candidate == &target_id)
        else {
            return Err(BrowserTargetRegistryError::TargetIsNotBackground(
                owner.clone(),
            ));
        };
        let activated = topology.background.remove(background_index);
        let previous_active = topology.active.replace(activated);
        let previous_background_index = previous_active.as_ref().map(|previous| {
            let index = topology.background.len();
            topology.background.push(previous.clone());
            index
        });
        Ok(BrowserTargetActivationTransaction {
            browser_context_id,
            target_id,
            background_index,
            previous_active,
            previous_background_index,
        })
    }

    pub(in crate::browser_host::navigation_owner) fn rollback_activation(
        &mut self,
        transaction: BrowserTargetActivationTransaction,
    ) -> bool {
        let Some(topology) = self.contexts.get_mut(&transaction.browser_context_id) else {
            debug_assert!(
                false,
                "same-turn Target activation rollback lost its context"
            );
            return false;
        };
        if topology.active.as_ref() != Some(&transaction.target_id) {
            debug_assert!(
                false,
                "same-turn Target activation rollback lost its active slot"
            );
            return false;
        }
        let previous_matches = match (
            transaction.previous_active.as_ref(),
            transaction.previous_background_index,
        ) {
            (Some(previous), Some(index)) => topology.background.get(index) == Some(previous),
            (None, None) => true,
            _ => false,
        };
        if !previous_matches || transaction.background_index > topology.background.len() {
            debug_assert!(
                false,
                "same-turn Target activation rollback lost an exact background slot"
            );
            return false;
        }
        topology.active = transaction.previous_active;
        if let Some(index) = transaction.previous_background_index {
            topology.background.remove(index);
        }
        topology
            .background
            .insert(transaction.background_index, transaction.target_id);
        true
    }

    pub(in crate::browser_host::navigation_owner) fn commit_activation(
        &mut self,
        transaction: BrowserTargetActivationTransaction,
    ) -> Option<BrowserTargetId> {
        transaction.previous_active
    }

    pub(in crate::browser_host::navigation_owner) fn remove_target(
        &mut self,
        owner: &BrowserPageOwnerKey,
    ) -> Result<BrowserTargetResidence, BrowserTargetRegistryError> {
        let transaction = self.begin_target_removal(owner)?;
        Ok(self.commit_target_removal(transaction))
    }

    pub(in crate::browser_host::navigation_owner) fn begin_target_removal(
        &mut self,
        owner: &BrowserPageOwnerKey,
    ) -> Result<BrowserTargetRemovalTransaction, BrowserTargetRegistryError> {
        let residence = self.validate_target_owner(owner)?;
        let browser_context_id = BrowserContextId::new(owner.browser_context_id());
        let target_id = BrowserTargetId::new(owner.target_id());
        let background_index = match residence {
            BrowserTargetResidence::Active => None,
            BrowserTargetResidence::Background => {
                let index = self
                    .contexts
                    .get(&browser_context_id)
                    .and_then(|topology| {
                        topology
                            .background
                            .iter()
                            .position(|candidate| candidate == &target_id)
                    })
                    .ok_or_else(|| {
                        BrowserTargetRegistryError::TargetIsNotBackground(owner.clone())
                    })?;
                Some(index)
            }
        };
        let Some(handle) = self
            .owners
            .get(&target_id)
            .map(|record| record.handle.clone())
        else {
            return Err(BrowserTargetRegistryError::UnknownTarget(target_id));
        };
        if !handle.reserve_retirement() {
            return Err(BrowserTargetRegistryError::TargetHandleNotLive(target_id));
        }
        let record = self.owners.remove(&target_id).ok_or_else(|| {
            handle.rollback_retirement_reservation();
            BrowserTargetRegistryError::UnknownTarget(target_id.clone())
        })?;
        let Some(topology) = self.contexts.get_mut(&browser_context_id) else {
            self.owners.insert(target_id.clone(), record);
            handle.rollback_retirement_reservation();
            return Err(BrowserTargetRegistryError::UnknownBrowserContext(
                browser_context_id,
            ));
        };
        match residence {
            BrowserTargetResidence::Active => {
                if topology.active.as_ref() != Some(&target_id) {
                    self.owners.insert(target_id, record);
                    handle.rollback_retirement_reservation();
                    return Err(BrowserTargetRegistryError::TargetIsNotActive(owner.clone()));
                }
                topology.active = None;
            }
            BrowserTargetResidence::Background => {
                let Some(index) = background_index
                    .filter(|index| topology.background.get(*index) == Some(&target_id))
                else {
                    self.owners.insert(target_id, record);
                    handle.rollback_retirement_reservation();
                    return Err(BrowserTargetRegistryError::TargetIsNotBackground(
                        owner.clone(),
                    ));
                };
                topology.background.remove(index);
            }
        }
        Ok(BrowserTargetRemovalTransaction {
            target_id,
            browser_context_id,
            record,
            residence,
            background_index,
        })
    }

    pub(in crate::browser_host::navigation_owner) fn rollback_target_removal(
        &mut self,
        transaction: BrowserTargetRemovalTransaction,
    ) -> bool {
        let BrowserTargetRemovalTransaction {
            target_id,
            browser_context_id,
            record,
            residence,
            background_index,
        } = transaction;
        let Some(topology) = self.contexts.get_mut(&browser_context_id) else {
            debug_assert!(
                false,
                "same-turn Target removal rollback lost its BrowserContext"
            );
            return false;
        };
        if self.owners.contains_key(&target_id) {
            debug_assert!(
                false,
                "same-turn Target removal rollback found a replacement owner"
            );
            return false;
        }
        let background_index = match residence {
            BrowserTargetResidence::Active if topology.active.is_none() => None,
            BrowserTargetResidence::Active => {
                debug_assert!(
                    false,
                    "same-turn active Target rollback found an occupied slot"
                );
                return false;
            }
            BrowserTargetResidence::Background => {
                let Some(index) =
                    background_index.filter(|index| *index <= topology.background.len())
                else {
                    debug_assert!(false, "same-turn background Target rollback lost its slot");
                    return false;
                };
                Some(index)
            }
        };
        record.handle.rollback_retirement_reservation();
        match (residence, background_index) {
            (BrowserTargetResidence::Active, _) => topology.active = Some(target_id.clone()),
            (BrowserTargetResidence::Background, Some(index)) => {
                topology.background.insert(index, target_id.clone());
            }
            (BrowserTargetResidence::Background, None) => {
                debug_assert!(
                    false,
                    "validated background rollback index must remain present"
                );
                return false;
            }
        }
        self.owners.insert(target_id, record).is_none()
    }

    pub(in crate::browser_host::navigation_owner) fn commit_target_removal(
        &mut self,
        transaction: BrowserTargetRemovalTransaction,
    ) -> BrowserTargetResidence {
        transaction.record.handle.commit_retirement_reservation();
        transaction.residence
    }
}
