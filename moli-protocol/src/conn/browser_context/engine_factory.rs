use moli_core::runtime::{
    NavigationEngine, NavigationRuntimeConfig, RendererBrowserContextRuntimeOwnerAccess,
};

use super::CdpConnection;

/// BrowserContext-scoped inputs needed only when Browser Core cannot restore
/// a retained Target engine and asks the migration adapter for a replacement.
pub(super) struct BrowserEngineReplacementInputs {
    runtime_config: NavigationRuntimeConfig,
}

impl BrowserEngineReplacementInputs {
    pub(super) fn capture(connection: &CdpConnection) -> Self {
        Self {
            runtime_config: connection
                .browser_host_state
                .navigation_owner()
                .active_runtime_config(),
        }
    }

    pub(super) fn create_engine(
        self,
        renderer_runtime: RendererBrowserContextRuntimeOwnerAccess,
    ) -> NavigationEngine {
        NavigationEngine::new_with_runtime_config_and_browser_context_access(
            self.runtime_config,
            renderer_runtime,
        )
        .expect("projected BrowserContext renderer owner must remain live")
    }

    pub(super) fn create_unbound_engine(self) -> NavigationEngine {
        NavigationEngine::new_with_runtime_config(self.runtime_config)
    }
}
