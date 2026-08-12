use crate::page_task_queue::{
    PageV8ForegroundTaskEffect, PageV8ForegroundTaskTurnAction, PageV8ForegroundTaskTurnOutcome,
    RendererPageV8ForegroundTask,
};

use super::PageVm;

impl PageVm {
    pub(in crate::runtime) fn apply_selected_page_v8_foreground_task_turn(
        &mut self,
        task: RendererPageV8ForegroundTask,
    ) -> anyhow::Result<PageV8ForegroundTaskTurnOutcome> {
        let owner = task.owner();
        let effect = if self.vm_mut().run_v8_foreground_task_body(task.into_task()) {
            PageV8ForegroundTaskEffect::Ran
        } else {
            PageV8ForegroundTaskEffect::IgnoredInactiveIsolateRegistration
        };
        let action = PageV8ForegroundTaskTurnAction { owner, effect };
        Ok(PageV8ForegroundTaskTurnOutcome::new(action))
    }
}
