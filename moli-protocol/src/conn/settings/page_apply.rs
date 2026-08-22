use super::*;

impl CdpConnection {
    pub fn effective_permission_overrides_for_browser_context_id(
        &self,
        browser_context_id: &str,
    ) -> Vec<moli_core::page::PermissionOverrideRegistration> {
        self.browser_host_policy_snapshot()
            .permission_overrides()
            .iter()
            .filter(|entry| {
                entry.browser_context_id.is_none()
                    || entry.browser_context_id.as_deref() == Some(browser_context_id)
            })
            .map(|entry| moli_core::page::PermissionOverrideRegistration {
                permission: entry.permission.clone(),
                setting: entry.setting.clone(),
                origin: entry.origin.clone(),
                embedded_origin: entry.embedded_origin.clone(),
            })
            .collect()
    }
}
