use super::parser_blocking_owner::MainParserBlockingExternalLoadOwner;
use crate::DocumentBlockingStylesheetSignature;
use crate::frame_owner_model::FrameDocumentTaskOwner;
use crate::live_document_parser::ParserResumePermit;
use crate::parser_script::context::{
    ParserClassicScriptDocumentOwnerState, ParserClassicScriptExecutionGateState,
    ParserClassicScriptSourceLoadOutcomeState, ParserClassicScriptSourceLoadStartState,
    ParserClassicScriptSourceLoadState, ParserClassicScriptSourceLoadWaitState,
    ParserClassicScriptSourceResultState,
};
use crate::parser_script::item::ParserClassicScriptRunnerItem;
#[cfg(test)]
use crate::parser_script::payload::ParserClassicScriptMetadata;
use crate::parser_script::payload::ParserPreparedClassicScript;
use crate::parser_script::runner::ParserClassicScriptRunner;
#[cfg(test)]
use crate::planning::PreparedScript;
use crate::planning::SharedScriptSourceLoad;
use std::collections::HashSet;

pub(super) type PendingParsingBlockingClassicScript =
    ParserClassicScriptRunnerItem<PendingParsingBlockingClassicScriptContext>;
pub(super) type PendingParsingBlockingClassicScriptRunner =
    ParserClassicScriptRunner<PendingParsingBlockingClassicScriptContext>;

#[derive(Debug, Clone)]
pub(super) struct PendingParsingBlockingClassicScriptContext {
    owner: FrameDocumentTaskOwner,
    pub(super) blocking_signatures_before: HashSet<DocumentBlockingStylesheetSignature>,
    pub(super) source_load: Option<PendingParserBlockingSourceLoad>,
    resume_permit: Option<ParserResumePermit>,
}

#[derive(Debug, Clone)]
pub(super) enum PendingParserBlockingSourceLoad {
    ReusablePreload(SharedScriptSourceLoad),
    ParserDiscovered(SharedScriptSourceLoad),
}

impl PendingParserBlockingSourceLoad {
    pub(super) fn shared_load(&self) -> SharedScriptSourceLoad {
        match self {
            Self::ReusablePreload(load) | Self::ParserDiscovered(load) => load.clone(),
        }
    }
}

impl PendingParsingBlockingClassicScriptContext {
    fn new(
        owner: FrameDocumentTaskOwner,
        blocking_signatures_before: HashSet<DocumentBlockingStylesheetSignature>,
        source_load: Option<PendingParserBlockingSourceLoad>,
    ) -> Self {
        Self {
            owner,
            blocking_signatures_before,
            source_load,
            resume_permit: None,
        }
    }

    pub(super) fn install_resume_permit(&mut self, permit: ParserResumePermit) {
        assert!(
            self.resume_permit.replace(permit).is_none(),
            "one parser-blocking script context can own only one parser resume permit"
        );
    }

    pub(super) fn resume_permit(&self) -> Option<ParserResumePermit> {
        self.resume_permit
    }
}

impl ParserClassicScriptDocumentOwnerState for PendingParsingBlockingClassicScriptContext {
    fn parser_classic_document_task_owner(&self) -> FrameDocumentTaskOwner {
        self.owner
    }
}

impl ParserClassicScriptExecutionGateState for PendingParsingBlockingClassicScriptContext {
    type ExecutionGateState = HashSet<DocumentBlockingStylesheetSignature>;

    fn parser_classic_execution_gate_state(&self) -> Self::ExecutionGateState {
        self.blocking_signatures_before.clone()
    }
}

impl ParserClassicScriptSourceLoadState for PendingParsingBlockingClassicScriptContext {
    fn clear_parser_classic_source_load_state(&mut self) {
        self.source_load = None;
    }
}

impl ParserClassicScriptSourceResultState for PendingParsingBlockingClassicScriptContext {
    fn parser_classic_source_load_outcome_state(
        &self,
    ) -> ParserClassicScriptSourceLoadOutcomeState {
        let Some(source_load) = self.source_load.as_ref() else {
            return ParserClassicScriptSourceLoadOutcomeState::NoSourceLoad;
        };
        let Some(outcome) = source_load.shared_load().try_outcome() else {
            return ParserClassicScriptSourceLoadOutcomeState::Waiting;
        };
        ParserClassicScriptSourceLoadOutcomeState::Ready(outcome)
    }
}

impl ParserClassicScriptSourceLoadStartState for PendingParsingBlockingClassicScriptContext {
    type SourceLoadState = PendingParserBlockingSourceLoad;

    fn install_parser_classic_source_load_state(&mut self, state: Self::SourceLoadState) {
        self.source_load = Some(state);
    }
}

impl ParserClassicScriptSourceLoadWaitState for PendingParsingBlockingClassicScriptContext {
    type SourceLoadWait = SharedScriptSourceLoad;

    fn parser_classic_source_load_wait(&self) -> Option<Self::SourceLoadWait> {
        self.source_load
            .as_ref()
            .map(|source_load| source_load.shared_load())
    }
}

pub(super) fn main_parser_blocking_classic_script_item(
    owner: FrameDocumentTaskOwner,
    input: ParserPreparedClassicScript,
    blocking_signatures_before: HashSet<DocumentBlockingStylesheetSignature>,
    source_load: Option<PendingParserBlockingSourceLoad>,
) -> PendingParsingBlockingClassicScript {
    let has_source_load = source_load.is_some();
    let is_ready = input.ready_script().is_some();
    let context =
        PendingParsingBlockingClassicScriptContext::new(owner, blocking_signatures_before, None);
    let mut item = if has_source_load || !is_ready {
        ParserClassicScriptRunnerItem::external_pending(input, context)
    } else {
        ParserClassicScriptRunnerItem::inline_ready(input, context)
    };
    if let Some(source_load) = source_load {
        let mut owner = MainParserBlockingExternalLoadOwner::new(owner, source_load);
        let _ = item
            .begin_runner_external_load_with_load_id_and_owner(None, &mut owner)
            .expect("parser-blocking source load must move pending script into loading state");
        debug_assert!(
            owner.source_load_transferred(),
            "parser-blocking source load owner must transfer load into context"
        );
    }
    item
}

#[cfg(test)]
pub(super) fn parser_blocking_classic_script_for_test(
    script: &PendingParsingBlockingClassicScript,
) -> Option<&PreparedScript> {
    script.runner_script()
}

#[cfg(test)]
pub(super) fn parser_blocking_classic_metadata_for_test(
    script: &PendingParsingBlockingClassicScript,
) -> Option<ParserClassicScriptMetadata> {
    script.runner_metadata()
}

#[cfg(test)]
pub(super) fn parser_blocking_classic_source_load_for_test(
    script: &PendingParsingBlockingClassicScript,
) -> Option<&PendingParserBlockingSourceLoad> {
    script.context().source_load.as_ref()
}
