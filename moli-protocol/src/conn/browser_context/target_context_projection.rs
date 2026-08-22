use super::target_projection_error::TargetProjectionError;
use super::{BrowserContext, CdpConnection};

enum PhysicalBrowserContextSlot {
    Selected,
    Inactive(usize),
}

pub(super) struct StagedPhysicalBrowserContext {
    slot: PhysicalBrowserContextSlot,
    pub(super) browser_context: BrowserContext,
}

/// Stages the physical BrowserContext payload that contains a Target
/// transaction. Core never receives this payload; it is removed only so a
/// rejected same-turn owner call can restore the exact Protocol slot.
impl CdpConnection {
    pub(super) fn take_physical_browser_context_for_target_projection(
        &mut self,
        browser_context_id: &str,
    ) -> Result<StagedPhysicalBrowserContext, TargetProjectionError> {
        if self
            .browser_context
            .as_ref()
            .is_some_and(|context| context.id == browser_context_id)
        {
            let Some(browser_context) = self.browser_context.take() else {
                return Err(TargetProjectionError::PhysicalBrowserContextMissing(
                    browser_context_id.to_owned(),
                ));
            };
            return Ok(StagedPhysicalBrowserContext {
                slot: PhysicalBrowserContextSlot::Selected,
                browser_context,
            });
        }
        let Some(index) = self
            .inactive_browser_contexts
            .iter()
            .position(|context| context.id == browser_context_id)
        else {
            return Err(TargetProjectionError::PhysicalBrowserContextMissing(
                browser_context_id.to_owned(),
            ));
        };
        Ok(StagedPhysicalBrowserContext {
            slot: PhysicalBrowserContextSlot::Inactive(index),
            browser_context: self.inactive_browser_contexts.remove(index),
        })
    }

    pub(super) fn restore_physical_browser_context_after_target_projection(
        &mut self,
        mut staged: StagedPhysicalBrowserContext,
    ) {
        staged
            .browser_context
            .adopt_browser_network_artifact_store(self.browser_host_state.network_artifacts());
        match staged.slot {
            PhysicalBrowserContextSlot::Selected => {
                let displaced = self.browser_context.replace(staged.browser_context);
                debug_assert!(
                    displaced.is_none(),
                    "same-turn Target projection must leave the selected BrowserContext slot vacant"
                );
                if let Some(displaced) = displaced {
                    self.inactive_browser_contexts.push(displaced);
                }
            }
            PhysicalBrowserContextSlot::Inactive(index) => {
                let insert_index = index.min(self.inactive_browser_contexts.len());
                debug_assert_eq!(
                    insert_index, index,
                    "same-turn Target projection must preserve the inactive BrowserContext vector"
                );
                self.inactive_browser_contexts
                    .insert(insert_index, staged.browser_context);
            }
        }
    }
}
