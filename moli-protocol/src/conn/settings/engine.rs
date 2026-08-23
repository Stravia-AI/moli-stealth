use super::*;
use crate::conn::{EmulatedGeolocationOverrideState, EmulatedNetworkConditions};
use moli_core::browser_host::BrowserHostPolicyUpdate;

impl CdpConnection {
    pub(crate) fn apply_active_engine_fetch_overrides(&mut self) {
        let policy = self.browser_host_network_policy_snapshot();
        let browser_identity = self
            .browser_context
            .as_ref()
            .and_then(|bc| bc.effective_active_browser_identity_override_owned())
            .or_else(|| policy.global_browser_identity_override().cloned())
            .unwrap_or_else(|| policy.base_browser_identity().clone());
        let http_proxy = self
            .browser_context
            .as_ref()
            .and_then(|bc| bc.http_proxy_override.clone())
            .or_else(|| policy.base_http_proxy().map(str::to_owned));
        let http_no_proxy = self
            .browser_context
            .as_ref()
            .and_then(|bc| bc.http_no_proxy_override.clone())
            .or_else(|| policy.base_http_no_proxy().map(str::to_owned));
        let tls_verify_host = self
            .browser_context
            .as_ref()
            .and_then(|bc| bc.tls_verify_host_override)
            .unwrap_or(policy.base_tls_verify_host());
        let bypass_service_worker = self
            .browser_context
            .as_ref()
            .is_some_and(|bc| bc.network_policy.bypass_service_worker());
        self.browser_host_state.configure_active_fetch(
            moli_core::browser_host::BrowserPageFetchConfiguration {
                browser_identity,
                http_proxy,
                http_no_proxy,
                tls_verify_host,
                bypass_service_worker,
            },
        );
    }

    pub async fn set_tls_verify_host_async(&mut self, enabled: bool) {
        if let Some(browser_context) = self.browser_context.as_mut() {
            browser_context.tls_verify_host_override = Some(enabled);
        } else {
            self.apply_browser_host_policy_update(BrowserHostPolicyUpdate::SetBaseTlsVerifyHost(
                enabled,
            ));
        }
        self.apply_active_engine_fetch_overrides();
        self.rebuild_resource_runtime_for_loaded_page_async().await;
    }

    pub fn tls_verify_host(&self) -> bool {
        let base_tls_verify_host = self
            .browser_host_network_policy_snapshot()
            .base_tls_verify_host();
        self.browser_context
            .as_ref()
            .and_then(|bc| bc.tls_verify_host_override)
            .unwrap_or(base_tls_verify_host)
    }

    pub fn user_agent(&self) -> String {
        let policy = self.browser_host_network_policy_snapshot();
        self.browser_context
            .as_ref()
            .and_then(|bc| bc.effective_active_browser_identity_override())
            .or_else(|| policy.global_browser_identity_override())
            .unwrap_or_else(|| policy.base_browser_identity())
            .user_agent()
            .to_owned()
    }

    pub async fn set_user_agent_override_async(&mut self, user_agent: impl Into<String>) {
        let user_agent = user_agent.into();
        let policy = self.browser_host_network_policy_snapshot();
        let browser_identity = moli_browser_profile::BrowserIdentityProfile::new(
            user_agent.clone(),
            policy.base_browser_identity().accept_language(),
        );
        if let Some(browser_context) = self.browser_context.as_mut() {
            browser_context
                .network_policy
                .set_browser_identity_override(browser_identity);
        } else {
            self.apply_browser_host_policy_update(BrowserHostPolicyUpdate::SetBaseBrowserIdentity(
                browser_identity,
            ));
        }
        self.apply_active_engine_fetch_overrides();
        self.rebuild_resource_runtime_for_loaded_page_async().await;
    }

    pub(crate) fn set_global_browser_identity_override_from_user_agent(
        &mut self,
        user_agent: Option<String>,
    ) {
        let base_identity = self
            .browser_host_network_policy_snapshot()
            .base_browser_identity()
            .clone();
        let identity = user_agent.as_ref().map(|user_agent| {
            moli_browser_profile::BrowserIdentityProfile::new(
                user_agent.clone(),
                base_identity.accept_language(),
            )
        });
        self.apply_browser_host_policy_update(
            BrowserHostPolicyUpdate::SetGlobalBrowserIdentityOverride(identity),
        );
        self.apply_active_engine_fetch_overrides();
    }

    pub(crate) fn set_global_network_conditions(
        &mut self,
        conditions: Option<EmulatedNetworkConditions>,
    ) {
        self.apply_browser_host_policy_update(BrowserHostPolicyUpdate::SetGlobalNetworkConditions(
            conditions,
        ));
        if let Some(browser_context) = self.browser_context.as_mut() {
            browser_context.global_network_conditions = conditions;
        }
        for browser_context in &mut self.inactive_browser_contexts {
            browser_context.global_network_conditions = conditions;
        }
    }

    pub(crate) fn set_global_geolocation_override(
        &mut self,
        override_state: Option<EmulatedGeolocationOverrideState>,
    ) {
        self.apply_browser_host_policy_update(
            BrowserHostPolicyUpdate::SetGlobalGeolocationOverride(override_state.clone()),
        );
        if let Some(browser_context) = self.browser_context.as_mut() {
            browser_context.global_geolocation_override = override_state.clone();
        }
        for browser_context in &mut self.inactive_browser_contexts {
            browser_context.global_geolocation_override = override_state.clone();
        }
    }

    #[cfg(test)]
    pub(crate) async fn set_http_proxy_override_async(&mut self, proxy: Option<String>) {
        if let Some(browser_context) = self.browser_context.as_mut() {
            browser_context.http_proxy_override = proxy;
        } else {
            self.apply_browser_host_policy_update(BrowserHostPolicyUpdate::SetBaseHttpProxy(proxy));
        }
        self.apply_active_engine_fetch_overrides();
        self.rebuild_resource_runtime_for_loaded_page_async().await;
    }

    pub fn http_proxy(&self) -> Option<String> {
        let base_http_proxy = self
            .browser_host_network_policy_snapshot()
            .base_http_proxy()
            .map(str::to_owned);
        self.browser_context
            .as_ref()
            .and_then(|bc| bc.http_proxy_override.clone())
            .or(base_http_proxy)
    }

    pub(crate) fn http_proxy_for_session_owner_owned(
        &self,
        session_id: Option<&str>,
    ) -> Option<String> {
        self.navigation_load_inputs_for_session_owner(session_id)
            .http_proxy_override
            .or_else(|| {
                self.browser_host_network_policy_snapshot()
                    .base_http_proxy()
                    .map(str::to_owned)
            })
    }

    pub fn http_no_proxy(&self) -> Option<String> {
        let base_http_no_proxy = self
            .browser_host_network_policy_snapshot()
            .base_http_no_proxy()
            .map(str::to_owned);
        self.browser_context
            .as_ref()
            .and_then(|bc| bc.http_no_proxy_override.clone())
            .or(base_http_no_proxy)
    }

    pub(crate) fn fetch_config(&self) -> moli_fetch::FetchConfig {
        self.browser_host_state
            .navigation_owner()
            .active_fetch_config()
            .clone()
    }

    pub(crate) fn base_browser_identity(&self) -> moli_browser_profile::BrowserIdentityProfile {
        self.browser_host_network_policy_snapshot()
            .base_browser_identity()
            .clone()
    }
}
