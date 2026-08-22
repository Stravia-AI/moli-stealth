use moli_browser_profile::BrowserIdentityProfile;
use moli_fetch::FetchConfig;
use serde_json::Value;

/// Browser-owned bounds for the single lightweight top-level window surface.
///
/// CDP and WebDriver may project this value differently, but changing or
/// disconnecting a frontend must not reset the underlying Browser policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserWindowBounds {
    pub left: Option<i32>,
    pub top: Option<i32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub window_state: String,
}

impl Default for BrowserWindowBounds {
    fn default() -> Self {
        Self {
            left: None,
            top: None,
            width: None,
            height: None,
            window_state: "normal".to_owned(),
        }
    }
}

/// Browser-global network availability projected into renderer surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmulatedNetworkConditions {
    offline: bool,
}

impl EmulatedNetworkConditions {
    pub fn offline() -> Self {
        Self { offline: true }
    }

    pub fn navigator_online(self) -> bool {
        !self.offline
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmulatedGeolocationOverride {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy: f64,
    pub altitude: Option<f64>,
    pub altitude_accuracy: Option<f64>,
    pub heading: Option<f64>,
    pub speed: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EmulatedGeolocationOverrideState {
    Position(EmulatedGeolocationOverride),
    PositionUnavailable,
}

impl EmulatedGeolocationOverrideState {
    pub fn position(&self) -> Option<&EmulatedGeolocationOverride> {
        match self {
            Self::Position(position) => Some(position),
            Self::PositionUnavailable => None,
        }
    }
}

/// Normalized permission policy installed by a browser-level frontend command.
///
/// The permission descriptor remains an extensible JSON object because the
/// renderer permission surface accepts Chromium's evolving descriptor shape;
/// session ids, command ids and event subscriptions are intentionally absent.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserPermissionOverride {
    pub permission: Value,
    pub setting: String,
    pub origin: Option<String>,
    pub embedded_origin: Option<String>,
    pub browser_context_id: Option<String>,
}

/// Protocol-neutral mutation accepted by the Browser Host policy owner.
///
/// The migration adapter applies this synchronously today. Keeping the
/// mutation move-owned and closed over known variants lets a later Host queue
/// carry the same value without exposing an arbitrary re-entrant closure over
/// the authoritative allocation.
#[derive(Debug, PartialEq)]
pub enum BrowserHostPolicyUpdate {
    ReplaceWindowBounds(BrowserWindowBounds),
    ReplacePermissionOverrides(Vec<BrowserPermissionOverride>),
    SetBaseBrowserIdentity(BrowserIdentityProfile),
    SetGlobalExtraHeaders(Vec<(String, String)>),
    SetGlobalBrowserIdentityOverride(Option<BrowserIdentityProfile>),
    SetGlobalNetworkConditions(Option<EmulatedNetworkConditions>),
    SetGlobalGeolocationOverride(Option<EmulatedGeolocationOverrideState>),
    SetGlobalCacheDisabled(bool),
    SetBaseHttpProxy(Option<String>),
    SetBaseHttpNoProxy(Option<String>),
    SetBaseTlsVerifyHost(bool),
}

/// Move-owned read view used by navigation and renderer configuration paths.
///
/// Keeping this separate from [`BrowserHostPolicyState`] avoids cloning
/// permission descriptors or window state on every resource-runtime rebuild.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserHostNetworkPolicySnapshot {
    base_browser_identity: BrowserIdentityProfile,
    global_extra_headers: Vec<(String, String)>,
    global_browser_identity_override: Option<BrowserIdentityProfile>,
    global_network_conditions: Option<EmulatedNetworkConditions>,
    global_geolocation_override: Option<EmulatedGeolocationOverrideState>,
    global_cache_disabled: bool,
    base_http_proxy: Option<String>,
    base_http_no_proxy: Option<String>,
    base_tls_verify_host: bool,
}

impl BrowserHostNetworkPolicySnapshot {
    pub fn base_browser_identity(&self) -> &BrowserIdentityProfile {
        &self.base_browser_identity
    }

    pub fn global_extra_headers(&self) -> &[(String, String)] {
        &self.global_extra_headers
    }

    pub fn global_browser_identity_override(&self) -> Option<&BrowserIdentityProfile> {
        self.global_browser_identity_override.as_ref()
    }

    pub fn global_network_conditions(&self) -> Option<EmulatedNetworkConditions> {
        self.global_network_conditions
    }

    pub fn global_geolocation_override(&self) -> Option<&EmulatedGeolocationOverrideState> {
        self.global_geolocation_override.as_ref()
    }

    pub fn global_cache_disabled(&self) -> bool {
        self.global_cache_disabled
    }

    pub fn base_http_proxy(&self) -> Option<&str> {
        self.base_http_proxy.as_deref()
    }

    pub fn base_http_no_proxy(&self) -> Option<&str> {
        self.base_http_no_proxy.as_deref()
    }

    pub fn base_tls_verify_host(&self) -> bool {
        self.base_tls_verify_host
    }
}

/// One application-owned source of truth for browser-global behavior policy.
///
/// Context- and Target-specific overrides remain in their exact runtime
/// owners. Frontends may update this state synchronously through
/// [`super::BrowserHostState`], then apply the resulting policy to existing
/// renderer Pages without retaining a mutable Host borrow across a wait.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserHostPolicyState {
    window_bounds: BrowserWindowBounds,
    permission_overrides: Vec<BrowserPermissionOverride>,
    base_browser_identity: BrowserIdentityProfile,
    global_extra_headers: Vec<(String, String)>,
    global_browser_identity_override: Option<BrowserIdentityProfile>,
    global_network_conditions: Option<EmulatedNetworkConditions>,
    global_geolocation_override: Option<EmulatedGeolocationOverrideState>,
    global_cache_disabled: bool,
    base_http_proxy: Option<String>,
    base_http_no_proxy: Option<String>,
    base_tls_verify_host: bool,
}

impl BrowserHostPolicyState {
    pub fn from_fetch_config(fetch_config: &FetchConfig) -> Self {
        Self {
            window_bounds: BrowserWindowBounds::default(),
            permission_overrides: Vec::new(),
            base_browser_identity: fetch_config.browser_identity().clone(),
            global_extra_headers: Vec::new(),
            global_browser_identity_override: None,
            global_network_conditions: None,
            global_geolocation_override: None,
            global_cache_disabled: false,
            base_http_proxy: fetch_config.http_proxy().map(str::to_owned),
            base_http_no_proxy: fetch_config.http_no_proxy().map(str::to_owned),
            base_tls_verify_host: fetch_config.tls_verify_host(),
        }
    }

    pub fn window_bounds(&self) -> &BrowserWindowBounds {
        &self.window_bounds
    }

    pub fn network_snapshot(&self) -> BrowserHostNetworkPolicySnapshot {
        BrowserHostNetworkPolicySnapshot {
            base_browser_identity: self.base_browser_identity.clone(),
            global_extra_headers: self.global_extra_headers.clone(),
            global_browser_identity_override: self.global_browser_identity_override.clone(),
            global_network_conditions: self.global_network_conditions,
            global_geolocation_override: self.global_geolocation_override.clone(),
            global_cache_disabled: self.global_cache_disabled,
            base_http_proxy: self.base_http_proxy.clone(),
            base_http_no_proxy: self.base_http_no_proxy.clone(),
            base_tls_verify_host: self.base_tls_verify_host,
        }
    }

    pub fn permission_overrides(&self) -> &[BrowserPermissionOverride] {
        &self.permission_overrides
    }

    pub fn base_browser_identity(&self) -> &BrowserIdentityProfile {
        &self.base_browser_identity
    }

    pub fn global_extra_headers(&self) -> &[(String, String)] {
        &self.global_extra_headers
    }

    pub fn global_browser_identity_override(&self) -> Option<&BrowserIdentityProfile> {
        self.global_browser_identity_override.as_ref()
    }

    pub fn global_network_conditions(&self) -> Option<EmulatedNetworkConditions> {
        self.global_network_conditions
    }

    pub fn global_geolocation_override(&self) -> Option<&EmulatedGeolocationOverrideState> {
        self.global_geolocation_override.as_ref()
    }

    pub fn global_cache_disabled(&self) -> bool {
        self.global_cache_disabled
    }

    pub fn base_http_proxy(&self) -> Option<&str> {
        self.base_http_proxy.as_deref()
    }

    pub fn base_http_no_proxy(&self) -> Option<&str> {
        self.base_http_no_proxy.as_deref()
    }

    pub fn base_tls_verify_host(&self) -> bool {
        self.base_tls_verify_host
    }

    pub(crate) fn apply_update(&mut self, update: BrowserHostPolicyUpdate) {
        match update {
            BrowserHostPolicyUpdate::ReplaceWindowBounds(bounds) => {
                self.window_bounds = bounds;
            }
            BrowserHostPolicyUpdate::ReplacePermissionOverrides(overrides) => {
                self.permission_overrides = overrides;
            }
            BrowserHostPolicyUpdate::SetBaseBrowserIdentity(identity) => {
                self.base_browser_identity = identity;
            }
            BrowserHostPolicyUpdate::SetGlobalExtraHeaders(headers) => {
                self.global_extra_headers = headers;
            }
            BrowserHostPolicyUpdate::SetGlobalBrowserIdentityOverride(identity) => {
                self.global_browser_identity_override = identity;
            }
            BrowserHostPolicyUpdate::SetGlobalNetworkConditions(conditions) => {
                self.global_network_conditions = conditions;
            }
            BrowserHostPolicyUpdate::SetGlobalGeolocationOverride(override_state) => {
                self.global_geolocation_override = override_state;
            }
            BrowserHostPolicyUpdate::SetGlobalCacheDisabled(disabled) => {
                self.global_cache_disabled = disabled;
            }
            BrowserHostPolicyUpdate::SetBaseHttpProxy(proxy) => {
                self.base_http_proxy = proxy;
            }
            BrowserHostPolicyUpdate::SetBaseHttpNoProxy(no_proxy) => {
                self.base_http_no_proxy = no_proxy;
            }
            BrowserHostPolicyUpdate::SetBaseTlsVerifyHost(enabled) => {
                self.base_tls_verify_host = enabled;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_defaults_follow_the_browser_fetch_configuration() {
        let mut fetch_config = FetchConfig::default();
        fetch_config.set_user_agent("HostPolicy/1.0");
        fetch_config.set_http_proxy(Some("http://proxy.test:8080".to_owned()));
        fetch_config.set_http_no_proxy(Some("localhost".to_owned()));
        fetch_config.set_tls_verify_host(false);

        let policy = BrowserHostPolicyState::from_fetch_config(&fetch_config);

        assert_eq!(
            policy.base_browser_identity().user_agent(),
            "HostPolicy/1.0"
        );
        assert_eq!(policy.base_http_proxy(), Some("http://proxy.test:8080"));
        assert_eq!(policy.base_http_no_proxy(), Some("localhost"));
        assert!(!policy.base_tls_verify_host());
        assert!(policy.permission_overrides().is_empty());
        assert_eq!(policy.window_bounds(), &BrowserWindowBounds::default());
    }
}
