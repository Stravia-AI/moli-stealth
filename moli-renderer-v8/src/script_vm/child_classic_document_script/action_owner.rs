use crate::document_script_scheduler::ParserClassicDocumentScriptExecutionResult;
use crate::frame_owner_model::{
    FrameClassicDocumentScriptExecutionAction, FrameDocumentClassicExecutionFollowup,
    FrameDocumentClassicScriptCompletionAction, FrameDocumentClassicScriptScheduling,
};

use super::super::{ScriptVm, child_document_script_owner_hooks::ChildDocumentScriptOwnerHooks};

pub(in crate::script_vm) struct ChildClassicExecutionActionOwner<'vm> {
    vm: &'vm mut ScriptVm,
}

impl<'vm> ChildClassicExecutionActionOwner<'vm> {
    pub(in crate::script_vm) fn new(vm: &'vm mut ScriptVm) -> Self {
        Self { vm }
    }

    pub(in crate::script_vm) fn execute_action(
        &mut self,
        action: FrameClassicDocumentScriptExecutionAction,
    ) -> ParserClassicDocumentScriptExecutionResult<
        FrameDocumentClassicScriptCompletionAction,
        FrameDocumentClassicExecutionFollowup,
    > {
        let mut followup = FrameDocumentClassicExecutionFollowup::default();
        let (job, finish) = action.into_parts();
        let parser_blocking = matches!(
            finish.scheduling,
            FrameDocumentClassicScriptScheduling::ParserBlocking
        );
        let parser_ready = !parser_blocking
            || ChildDocumentScriptOwnerHooks::new(self.vm).resume_parser_for_classic_execution(
                finish.child_handle,
                finish.task_owner,
                finish.script_handle,
            );
        let _parser_script_nesting = (parser_blocking && parser_ready)
            .then(|| {
                ChildDocumentScriptOwnerHooks::new(self.vm)
                    .enter_parser_script_nesting(finish.child_handle, finish.task_owner)
            })
            .flatten();
        followup.note_script_job_attempted();
        let execution = parser_ready.then(|| {
            ChildDocumentScriptOwnerHooks::new(self.vm)
                .execute_frame_script_job_selected_task_body(job)
        });
        if let Some(Err(error)) = execution {
            tracing::warn!(
                error = %error,
                child_handle = ?finish.child_handle,
                script_handle = ?finish.script_handle,
                url = %finish.script_url,
                base_url = %finish.script_base_url,
                "child classic script execution failed"
            );
            followup.note_script_job_failed();
        } else if !parser_ready {
            tracing::debug!(
                child_handle = ?finish.child_handle,
                script_handle = ?finish.script_handle,
                "dropping child parser-blocking classic execution with a stale parser resume permit"
            );
            followup.note_script_job_failed();
        }
        let completion = ChildDocumentScriptOwnerHooks::new(self.vm)
            .finish_child_classic_script_execution(finish);
        if completion.is_some() {
            followup.note_completion_produced();
        }
        ParserClassicDocumentScriptExecutionResult::new(completion, followup)
    }
}
