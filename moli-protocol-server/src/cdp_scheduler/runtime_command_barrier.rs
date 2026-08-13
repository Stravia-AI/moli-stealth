use moli_protocol::{CommandResponseFlushPermit, RuntimeCommandOutputBarrierPermit};

/// The single-use authority that finishes one command's protocol-output
/// boundary.
///
/// The response permit and optional Runtime-output barrier are one value so a
/// command cannot publish its response while forgetting to release the
/// concrete Page output held on its behalf. Commands that do not execute
/// Page JavaScript still carry the response permit and simply have no Runtime
/// barrier.
#[must_use = "a command output release permit must be completed exactly once"]
pub(crate) struct CommandOutputReleasePermit {
    response_flush: CommandResponseFlushPermit,
    runtime_output_barrier: Option<RuntimeCommandOutputBarrierPermit>,
}

impl CommandOutputReleasePermit {
    pub(super) fn new(
        response_flush: CommandResponseFlushPermit,
        runtime_output_barrier: Option<RuntimeCommandOutputBarrierPermit>,
    ) -> Self {
        Self {
            response_flush,
            runtime_output_barrier,
        }
    }

    pub(super) fn finish_response(self) -> Option<RuntimeCommandOutputBarrierPermit> {
        self.response_flush.finish();
        self.runtime_output_barrier
    }
}
