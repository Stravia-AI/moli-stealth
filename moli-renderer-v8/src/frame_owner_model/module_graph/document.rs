use crate::{
    frame_owner_model::{
        FrameDocumentDynamicImportTerminalWork, FrameDocumentModuleDependencyTerminalWork,
        FrameDocumentModuleScriptTerminalBatchTask, FrameDocumentModuleScriptTerminalTask,
        FrameDocumentModuleScriptTerminalWork, FrameDocumentModuleTerminalBatch,
        FrameDocumentModuleTerminalWarning, FrameDocumentModuleTerminalWarningRecord,
        FrameDocumentModulepreloadTerminalWork, FrameDocumentOwner,
        FrameDocumentParserRootTerminalWork, FrameDocumentTaskOwner, FrameRealmId,
    },
    module_runtime::{
        NativeDocumentModulator, NativeModuleMapSingleModuleClient, NativeModuleOwnerEvent,
    },
};

use super::ChildDocumentModulatorStore;
use super::dynamic_import_fetch::FrameDocumentDynamicImportTerminalPreparedAction;
use super::tree_jobs::FrameDocumentModuleScriptTerminalFollowup;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentModuleTerminalQueueFollowup {
    module_script_terminal_queued: bool,
    modulepreload_event_action_queued: bool,
    dynamic_import_owner_action_queued: bool,
    dynamic_import_job_resumed: bool,
    dynamic_import_wait_retained: bool,
    terminal_warning_recorded: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentModuleScriptTerminalOutcome {
    consumed_terminal_batch: bool,
    module_script_terminal_followup: FrameDocumentModuleScriptTerminalFollowup,
}

pub(crate) trait FrameDocumentModuleScriptTerminalHooks {
    fn handle_parser_root_terminal(
        &mut self,
        work: Box<FrameDocumentParserRootTerminalWork>,
    ) -> FrameDocumentModuleScriptTerminalFollowup;

    fn handle_single_module_terminal(
        &mut self,
        work: FrameDocumentModuleScriptTerminalWork,
    ) -> FrameDocumentModuleScriptTerminalFollowup;

    fn handle_dependency_terminal(
        &mut self,
        work: Box<FrameDocumentModuleDependencyTerminalWork>,
    ) -> FrameDocumentModuleScriptTerminalFollowup;
}

pub(crate) struct FrameDocumentModuleScriptTerminalRunner<Hooks> {
    hooks: Hooks,
}

impl<Hooks> FrameDocumentModuleScriptTerminalRunner<Hooks> {
    pub(crate) fn new(hooks: Hooks) -> Self {
        Self { hooks }
    }

    #[cfg(test)]
    pub(crate) fn into_hooks(self) -> Hooks {
        self.hooks
    }
}

impl<Hooks> FrameDocumentModuleScriptTerminalRunner<Hooks>
where
    Hooks: FrameDocumentModuleScriptTerminalHooks,
{
    pub(crate) fn run_terminal_batch_task(
        &mut self,
        task: FrameDocumentModuleScriptTerminalBatchTask,
    ) -> FrameDocumentModuleScriptTerminalOutcome {
        let mut outcome = FrameDocumentModuleScriptTerminalOutcome::consumed_terminal_batch();
        self.run_module_script_terminals(task.into_payload(), &mut outcome);
        outcome
    }

    fn run_module_script_terminals(
        &mut self,
        module_script_terminal_tasks: Vec<FrameDocumentModuleScriptTerminalTask>,
        outcome: &mut FrameDocumentModuleScriptTerminalOutcome,
    ) {
        for work in module_script_terminal_tasks {
            let followup = match work {
                FrameDocumentModuleScriptTerminalTask::ParserRoot(work) => {
                    self.hooks.handle_parser_root_terminal(work)
                }
                FrameDocumentModuleScriptTerminalTask::SingleModule(work) => {
                    self.hooks.handle_single_module_terminal(work)
                }
                FrameDocumentModuleScriptTerminalTask::Dependency(work) => {
                    self.hooks.handle_dependency_terminal(work)
                }
            };
            outcome.merge_module_script_terminal_followup(followup);
        }
    }
}

impl FrameDocumentModuleTerminalQueueFollowup {
    pub(crate) fn none() -> Self {
        Self::default()
    }

    pub(crate) fn module_script_terminal_queued() -> Self {
        Self {
            module_script_terminal_queued: true,
            ..Self::default()
        }
    }

    pub(crate) fn modulepreload_event_action_queued() -> Self {
        Self {
            modulepreload_event_action_queued: true,
            ..Self::default()
        }
    }

    pub(crate) fn dynamic_import_owner_action_queued() -> Self {
        Self {
            dynamic_import_owner_action_queued: true,
            ..Self::default()
        }
    }

    pub(crate) fn dynamic_import_wait_retained() -> Self {
        Self {
            dynamic_import_wait_retained: true,
            ..Self::default()
        }
    }

    pub(crate) fn dynamic_import_job_resumed() -> Self {
        Self {
            dynamic_import_job_resumed: true,
            ..Self::default()
        }
    }

    pub(crate) fn terminal_warning_recorded() -> Self {
        Self {
            terminal_warning_recorded: true,
            ..Self::default()
        }
    }

    pub(crate) fn terminal_warning_from_recorded(recorded: bool) -> Self {
        if recorded {
            Self::terminal_warning_recorded()
        } else {
            Self::none()
        }
    }

    pub(crate) fn module_script_terminal_from_queued(queued: bool) -> Self {
        if queued {
            Self::module_script_terminal_queued()
        } else {
            Self::none()
        }
    }

    pub(crate) fn modulepreload_event_action_from_queued(queued: bool) -> Self {
        if queued {
            Self::modulepreload_event_action_queued()
        } else {
            Self::none()
        }
    }

    pub(crate) fn dynamic_import_owner_action_from_queued(queued: bool) -> Self {
        if queued {
            Self::dynamic_import_owner_action_queued()
        } else {
            Self::none()
        }
    }

    #[cfg(test)]
    pub(crate) fn module_script_terminal_was_queued(self) -> bool {
        self.module_script_terminal_queued
    }

    #[cfg(test)]
    pub(crate) fn modulepreload_event_action_was_queued(self) -> bool {
        self.modulepreload_event_action_queued
    }

    #[cfg(test)]
    pub(crate) fn dynamic_import_owner_action_was_queued(self) -> bool {
        self.dynamic_import_owner_action_queued
    }

    #[cfg(test)]
    pub(crate) fn dynamic_import_wait_was_retained(self) -> bool {
        self.dynamic_import_wait_retained
    }

    #[cfg(test)]
    pub(crate) fn dynamic_import_job_was_resumed(self) -> bool {
        self.dynamic_import_job_resumed
    }

    #[cfg(test)]
    pub(crate) fn terminal_warning_was_recorded(self) -> bool {
        self.terminal_warning_recorded
    }

    pub(crate) fn made_progress(self) -> bool {
        self.module_script_terminal_queued
            || self.modulepreload_event_action_queued
            || self.dynamic_import_owner_action_queued
            || self.dynamic_import_job_resumed
            || self.dynamic_import_wait_retained
            || self.terminal_warning_recorded
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.module_script_terminal_queued |= other.module_script_terminal_queued;
        self.modulepreload_event_action_queued |= other.modulepreload_event_action_queued;
        self.dynamic_import_owner_action_queued |= other.dynamic_import_owner_action_queued;
        self.dynamic_import_job_resumed |= other.dynamic_import_job_resumed;
        self.dynamic_import_wait_retained |= other.dynamic_import_wait_retained;
        self.terminal_warning_recorded |= other.terminal_warning_recorded;
    }
}

impl FrameDocumentModuleScriptTerminalOutcome {
    pub(crate) fn consumed_terminal_batch() -> Self {
        Self {
            consumed_terminal_batch: true,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn made_progress(&self) -> bool {
        self.consumed_terminal_batch || self.module_script_terminal_followup.made_progress()
    }

    pub(crate) fn merge_module_script_terminal_followup(
        &mut self,
        followup: FrameDocumentModuleScriptTerminalFollowup,
    ) {
        self.module_script_terminal_followup.merge(followup);
    }

    #[cfg(test)]
    pub(crate) fn module_script_terminal_followup(
        &self,
    ) -> FrameDocumentModuleScriptTerminalFollowup {
        self.module_script_terminal_followup
    }
}

pub(super) struct ChildDocumentModulatorEntry {
    pub(super) realm_id: FrameRealmId,
    pub(super) document_modulator: NativeDocumentModulator,
}

impl ChildDocumentModulatorEntry {
    fn new(realm_id: FrameRealmId) -> Self {
        Self {
            realm_id,
            document_modulator: NativeDocumentModulator::default(),
        }
    }

    fn module_terminal_batch(
        &self,
        owner: FrameDocumentTaskOwner,
        notification: crate::module_runtime::ModuleMapTerminalNotification,
    ) -> FrameDocumentModuleTerminalBatch {
        let (key, clients, successful) = notification.into_parts();
        let (single_module_clients, parser_root_clients, modulepreload_link_clients) =
            clients.into_parts();
        let parser_root_client_count = parser_root_clients.len();
        let parser_root_terminal_works = self
            .document_modulator
            .parser_root_terminal_works(
                owner,
                self.realm_id,
                key.clone(),
                parser_root_clients,
                successful,
            )
            .unwrap_or_default();
        let modulepreload_terminal_works = modulepreload_link_clients
            .into_iter()
            .map(|link_client| {
                debug_assert_eq!(
                    link_client.key(),
                    &key,
                    "modulepreload terminal client must keep its accepted module key"
                );
                FrameDocumentModulepreloadTerminalWork::from_terminal_parts(
                    self.realm_id,
                    key.clone(),
                    link_client
                        .frame_document_client()
                        .expect("child modulator terminal must retain its frame owner projection"),
                    successful,
                )
            })
            .collect::<Vec<_>>();
        let mut module_script_terminal_tasks = parser_root_terminal_works
            .into_iter()
            .map(FrameDocumentModuleScriptTerminalTask::parser_root)
            .collect::<Vec<_>>();
        let mut dynamic_import_owner_actions = Vec::new();
        let mut warnings = Vec::new();
        for client in single_module_clients {
            match client {
                NativeModuleMapSingleModuleClient::DynamicImport(client) => {
                    dynamic_import_owner_actions.push(
                        FrameDocumentDynamicImportTerminalPreparedAction::from_terminal_work(
                            FrameDocumentDynamicImportTerminalWork::from_terminal_parts(
                                owner,
                                self.realm_id,
                                key.clone(),
                                client,
                            ),
                        ),
                    );
                }
                NativeModuleMapSingleModuleClient::ModuleScript(client) => {
                    module_script_terminal_tasks.push(
                        FrameDocumentModuleScriptTerminalTask::single_module(
                            FrameDocumentModuleScriptTerminalWork::from_terminal_parts(
                                owner,
                                self.realm_id,
                                key.clone(),
                                client,
                            ),
                        ),
                    );
                }
            }
        }
        if parser_root_client_count > 0
            && !module_script_terminal_tasks
                .iter()
                .any(|task| matches!(task, FrameDocumentModuleScriptTerminalTask::ParserRoot(_)))
        {
            warnings.push(
                FrameDocumentModuleTerminalWarning::ParserRootTerminalWithoutOwnerWork {
                    key: key.clone(),
                    successful,
                    parser_root_client_count,
                },
            );
        }
        let mut batch = FrameDocumentModuleTerminalBatch::default();
        for work in modulepreload_terminal_works {
            batch.push_modulepreload_terminal_work(work);
        }
        batch.push_module_script_terminals(owner, self.realm_id, module_script_terminal_tasks);
        for warning in warnings {
            batch.push_warning(FrameDocumentModuleTerminalWarningRecord::new(
                owner,
                self.realm_id,
                warning,
            ));
        }
        for action in dynamic_import_owner_actions {
            batch.push_dynamic_import_owner_action(action);
        }
        batch
    }

    pub(super) fn take_ready_document_modulator_terminal_batches(
        &mut self,
        owner: FrameDocumentTaskOwner,
    ) -> FrameDocumentModuleTerminalBatch {
        let mut batch = FrameDocumentModuleTerminalBatch::default();
        let mut events = Vec::new();
        self.document_modulator
            .drain_ready_owner_events(|event| events.push(event));
        for event in events {
            match event {
                NativeModuleOwnerEvent::ModuleMapTerminalNotification(notification) => {
                    if notification.is_empty() {
                        continue;
                    }
                    let (
                        tasks,
                        modulepreload_terminal_works,
                        dynamic_import_owner_actions,
                        warnings,
                    ) = self.module_terminal_batch(owner, notification).into_parts();
                    for task in tasks {
                        batch.push_terminal_batch(task);
                    }
                    for work in modulepreload_terminal_works {
                        batch.push_modulepreload_terminal_work(work);
                    }
                    for action in dynamic_import_owner_actions {
                        batch.push_dynamic_import_owner_action(action);
                    }
                    for warning in warnings {
                        batch.push_warning(warning);
                    }
                }
                NativeModuleOwnerEvent::ModulepreloadLinkError(link_handle) => {
                    tracing::warn!(
                        ?owner,
                        realm_id = ?self.realm_id,
                        ?link_handle,
                        "ignored main-adapter modulepreload link error in child document modulator"
                    );
                }
            }
        }
        batch
    }
}

impl ChildDocumentModulatorStore {
    pub(crate) fn clear(&mut self) {
        self.documents.clear();
    }

    pub(crate) fn remove_execution_context(
        &mut self,
        local_window_id: crate::frame_owner_model::LocalWindowId,
    ) -> usize {
        usize::from(self.documents.remove(&local_window_id).is_some())
    }

    #[cfg(test)]
    pub(crate) fn contains_execution_context(&self, owner: FrameDocumentOwner) -> bool {
        self.documents.contains_key(&owner.local_window_id)
    }

    pub(super) fn document_modulator_entry_mut(
        &mut self,
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
    ) -> &mut ChildDocumentModulatorEntry {
        if self
            .documents
            .get(&owner.local_window_id)
            .is_some_and(|entry| entry.realm_id != realm_id)
        {
            self.documents.insert(
                owner.local_window_id,
                ChildDocumentModulatorEntry::new(realm_id),
            );
        }
        self.documents
            .entry(owner.local_window_id)
            .or_insert_with(|| ChildDocumentModulatorEntry::new(realm_id))
    }

    pub(super) fn current_document_modulator_entry(
        &self,
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
    ) -> Option<&ChildDocumentModulatorEntry> {
        self.documents
            .get(&owner.local_window_id)
            .filter(|entry| entry.realm_id == realm_id)
    }

    pub(super) fn current_document_modulator_entry_mut(
        &mut self,
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
    ) -> Option<&mut ChildDocumentModulatorEntry> {
        self.documents
            .get_mut(&owner.local_window_id)
            .filter(|entry| entry.realm_id == realm_id)
    }

    #[cfg(test)]
    pub(crate) fn take_or_create_document_modulator(
        &mut self,
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
    ) -> NativeDocumentModulator {
        let document_modulator_entry = self.document_modulator_entry_mut(owner, realm_id);
        std::mem::take(&mut document_modulator_entry.document_modulator)
    }

    pub(crate) fn ensure_document_modulator(
        &mut self,
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
    ) {
        let _ = self.document_modulator_entry_mut(owner, realm_id);
    }

    pub(crate) fn take_current_document_modulator(
        &mut self,
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
    ) -> Option<NativeDocumentModulator> {
        let document_modulator_entry =
            self.current_document_modulator_entry_mut(owner, realm_id)?;
        Some(std::mem::take(
            &mut document_modulator_entry.document_modulator,
        ))
    }

    pub(crate) fn restore_document_modulator(
        &mut self,
        owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
        document_modulator: NativeDocumentModulator,
    ) -> FrameDocumentModuleTerminalBatch {
        let Some(document_modulator_entry) =
            self.current_document_modulator_entry_mut(owner.document_owner(), realm_id)
        else {
            return FrameDocumentModuleTerminalBatch::default();
        };
        document_modulator_entry.document_modulator = document_modulator;
        document_modulator_entry.take_ready_document_modulator_terminal_batches(owner)
    }

    pub(crate) fn restore_document_modulator_without_owner_events(
        &mut self,
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        document_modulator: NativeDocumentModulator,
    ) {
        if let Some(document_modulator_entry) =
            self.current_document_modulator_entry_mut(owner, realm_id)
        {
            document_modulator_entry.document_modulator = document_modulator;
        }
    }

    pub(crate) fn take_ready_document_modulator_terminal_batches(
        &mut self,
        owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
    ) -> FrameDocumentModuleTerminalBatch {
        let Some(document_modulator_entry) =
            self.current_document_modulator_entry_mut(owner.document_owner(), realm_id)
        else {
            return FrameDocumentModuleTerminalBatch::default();
        };
        document_modulator_entry.take_ready_document_modulator_terminal_batches(owner)
    }
}
