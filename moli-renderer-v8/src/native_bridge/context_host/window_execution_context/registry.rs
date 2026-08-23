use super::super::{
    LightweightPopupLocalWindowId, OwnerDispatchScope, RuntimeObservableContextToken,
};
pub(crate) use crate::browsing_context_model::RealmAccessPolicy as WindowExecutionContextAccessPolicy;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum WindowExecutionContextOwner {
    Frame(crate::frame_owner_model::LocalWindowId),
    LightweightPopup {
        popup_id: u64,
        local_window_id: LightweightPopupLocalWindowId,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::native_bridge::context_host) struct WindowExecutionContextRealmRegistration {
    pub(in crate::native_bridge::context_host) owner: WindowExecutionContextOwner,
    pub(in crate::native_bridge::context_host) access_policy: WindowExecutionContextAccessPolicy,
}

impl WindowExecutionContextRealmRegistration {
    pub(in crate::native_bridge::context_host) fn new(
        owner: WindowExecutionContextOwner,
        access_policy: WindowExecutionContextAccessPolicy,
    ) -> Self {
        Self {
            owner,
            access_policy,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::native_bridge::context_host) struct WindowExecutionContextScopedRealmRegistration {
    pub(in crate::native_bridge::context_host) dispatch_scope: OwnerDispatchScope,
    pub(in crate::native_bridge::context_host) registration:
        WindowExecutionContextRealmRegistration,
}

impl WindowExecutionContextScopedRealmRegistration {
    pub(in crate::native_bridge::context_host) fn new(
        dispatch_scope: OwnerDispatchScope,
        registration: WindowExecutionContextRealmRegistration,
    ) -> Self {
        Self {
            dispatch_scope,
            registration,
        }
    }
}

#[derive(Default)]
pub(in crate::native_bridge::context_host) struct WindowExecutionContextRealmRecords {
    // A V8 context token identifies one concrete realm. Lightweight popups
    // currently share their opener's concrete context, so their entries are
    // explicitly scoped aliases rather than competing concrete registrations.
    pub(in crate::native_bridge::context_host) concrete_by_token:
        HashMap<RuntimeObservableContextToken, WindowExecutionContextScopedRealmRegistration>,
    lightweight_popup_aliases: HashMap<
        (OwnerDispatchScope, RuntimeObservableContextToken),
        WindowExecutionContextRealmRegistration,
    >,
}

impl WindowExecutionContextRealmRecords {
    pub(in crate::native_bridge::context_host) fn registration(
        &self,
        dispatch_scope: OwnerDispatchScope,
        realm_token: RuntimeObservableContextToken,
    ) -> Option<WindowExecutionContextRealmRegistration> {
        match dispatch_scope {
            OwnerDispatchScope::LightweightPopup(_) => self
                .lightweight_popup_aliases
                .get(&(dispatch_scope, realm_token))
                .copied(),
            OwnerDispatchScope::Top | OwnerDispatchScope::Child(_) => self
                .concrete_by_token
                .get(&realm_token)
                .filter(|registered| registered.dispatch_scope == dispatch_scope)
                .map(|registered| registered.registration),
        }
    }

    pub(in crate::native_bridge::context_host) fn concrete_registration(
        &self,
        realm_token: RuntimeObservableContextToken,
    ) -> Option<WindowExecutionContextScopedRealmRegistration> {
        self.concrete_by_token.get(&realm_token).copied()
    }

    pub(in crate::native_bridge::context_host) fn register(
        &mut self,
        dispatch_scope: OwnerDispatchScope,
        realm_token: RuntimeObservableContextToken,
        registration: WindowExecutionContextRealmRegistration,
    ) -> Result<(), WindowExecutionContextScopedRealmRegistration> {
        let candidate =
            WindowExecutionContextScopedRealmRegistration::new(dispatch_scope, registration);
        match dispatch_scope {
            OwnerDispatchScope::LightweightPopup(_) => {
                match self
                    .lightweight_popup_aliases
                    .get(&(dispatch_scope, realm_token))
                {
                    Some(registered) if *registered != registration => {
                        return Err(WindowExecutionContextScopedRealmRegistration::new(
                            dispatch_scope,
                            *registered,
                        ));
                    }
                    Some(_) => return Ok(()),
                    None => {}
                }
                self.lightweight_popup_aliases
                    .insert((dispatch_scope, realm_token), registration);
            }
            OwnerDispatchScope::Top | OwnerDispatchScope::Child(_) => {
                match self.concrete_by_token.get(&realm_token) {
                    Some(registered) if *registered != candidate => return Err(*registered),
                    Some(_) => return Ok(()),
                    None => {}
                }
                self.concrete_by_token.insert(realm_token, candidate);
            }
        }
        Ok(())
    }

    pub(in crate::native_bridge::context_host) fn remove(
        &mut self,
        dispatch_scope: OwnerDispatchScope,
        realm_token: RuntimeObservableContextToken,
    ) {
        match dispatch_scope {
            OwnerDispatchScope::LightweightPopup(_) => {
                self.lightweight_popup_aliases
                    .remove(&(dispatch_scope, realm_token));
            }
            OwnerDispatchScope::Top | OwnerDispatchScope::Child(_) => {
                if self
                    .concrete_by_token
                    .get(&realm_token)
                    .is_some_and(|registered| registered.dispatch_scope == dispatch_scope)
                {
                    self.concrete_by_token.remove(&realm_token);
                }
            }
        }
    }

    pub(in crate::native_bridge::context_host) fn remove_token(
        &mut self,
        realm_token: RuntimeObservableContextToken,
    ) -> usize {
        let concrete_count = usize::from(self.concrete_by_token.remove(&realm_token).is_some());
        let alias_count_before = self.lightweight_popup_aliases.len();
        self.lightweight_popup_aliases
            .retain(|(_, token), _| *token != realm_token);
        concrete_count + alias_count_before.saturating_sub(self.lightweight_popup_aliases.len())
    }

    pub(in crate::native_bridge::context_host) fn retire_owner(
        &mut self,
        owner: WindowExecutionContextOwner,
    ) {
        self.concrete_by_token
            .retain(|_, registered| registered.registration.owner != owner);
        self.lightweight_popup_aliases
            .retain(|_, registered| registered.owner != owner);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct WindowExecutionContextIdentity {
    owner: WindowExecutionContextOwner,
    dispatch_scope: OwnerDispatchScope,
    realm_token: RuntimeObservableContextToken,
    access_policy: WindowExecutionContextAccessPolicy,
}

impl WindowExecutionContextIdentity {
    pub(crate) fn new(
        owner: WindowExecutionContextOwner,
        dispatch_scope: OwnerDispatchScope,
        realm_token: RuntimeObservableContextToken,
        access_policy: WindowExecutionContextAccessPolicy,
    ) -> Self {
        Self {
            owner,
            dispatch_scope,
            realm_token,
            access_policy,
        }
    }

    pub(crate) fn owner(self) -> WindowExecutionContextOwner {
        self.owner
    }

    pub(crate) fn dispatch_scope(self) -> OwnerDispatchScope {
        self.dispatch_scope
    }

    pub(crate) fn realm_token(self) -> RuntimeObservableContextToken {
        self.realm_token
    }

    pub(crate) fn grants_universal_access(self) -> bool {
        self.access_policy == WindowExecutionContextAccessPolicy::Universal
    }

    pub(in crate::native_bridge::context_host) fn access_policy(
        self,
    ) -> WindowExecutionContextAccessPolicy {
        self.access_policy
    }
}
