use crate::{document_runtime::DomHandle, dom::native::DomHost};

use moli_selector::stylo_shadow_root_host_participates_in_style_scope as shadow_root_host_participates_in_style_scope;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct StyleSourceId {
    pub(super) scope_id: StyleScopeId,
    pub(super) kind: StyleSourceKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum StyleInvalidationSourceTarget {
    Stylesheet(StyleSourceId),
    UserAgent {
        document: DomHandle,
    },
    FallbackRoot {
        scope_id: StyleScopeId,
        root: DomHandle,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum StyleScopeId {
    Document(DomHandle),
    ShadowRoot(DomHandle),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum StyleSourceKind {
    OwnerStyleSheet { owner: DomHandle },
    LinkedStyleSheet { owner: DomHandle },
    DocumentAdoptedStyleSheet { client_id: u64 },
    ShadowRootAdoptedStyleSheet { client_id: u64 },
}

impl StyleSourceId {
    pub(crate) fn owner_style_sheet(host: &DomHost, owner: DomHandle) -> Option<Self> {
        Some(Self {
            scope_id: StyleScopeId::for_stylesheet_owner(host, owner)?,
            kind: StyleSourceKind::OwnerStyleSheet { owner },
        })
    }

    pub(crate) fn linked_style_sheet(host: &DomHost, owner: DomHandle) -> Option<Self> {
        Some(Self {
            scope_id: StyleScopeId::for_stylesheet_owner(host, owner)?,
            kind: StyleSourceKind::LinkedStyleSheet { owner },
        })
    }

    pub(crate) fn document_adopted_style_sheet(document: DomHandle, client_id: u64) -> Self {
        Self {
            scope_id: StyleScopeId::Document(document),
            kind: StyleSourceKind::DocumentAdoptedStyleSheet { client_id },
        }
    }

    pub(crate) fn shadow_root_adopted_style_sheet(root: DomHandle, client_id: u64) -> Self {
        Self {
            scope_id: StyleScopeId::ShadowRoot(root),
            kind: StyleSourceKind::ShadowRootAdoptedStyleSheet { client_id },
        }
    }
}

impl StyleInvalidationSourceTarget {
    pub(super) fn stylesheet(source_id: StyleSourceId) -> Self {
        Self::Stylesheet(source_id)
    }

    pub(super) fn user_agent(document: DomHandle) -> Self {
        Self::UserAgent { document }
    }

    pub(super) fn fallback_root(host: &DomHost, root: DomHandle) -> Option<Self> {
        Some(Self::FallbackRoot {
            scope_id: StyleScopeId::for_style_root(host, root)?,
            root,
        })
    }

    pub(super) fn scope_id(&self) -> StyleScopeId {
        match self {
            Self::Stylesheet(source_id) => source_id.scope_id,
            Self::UserAgent { document } => StyleScopeId::Document(*document),
            Self::FallbackRoot { scope_id, .. } => *scope_id,
        }
    }

    pub(super) fn stylesheet_source_id(&self) -> Option<&StyleSourceId> {
        match self {
            Self::Stylesheet(source_id) => Some(source_id),
            Self::UserAgent { .. } | Self::FallbackRoot { .. } => None,
        }
    }

    pub(super) fn is_fallback_root(&self) -> bool {
        matches!(self, Self::FallbackRoot { .. })
    }

    #[cfg(test)]
    pub(super) fn is_user_agent(&self) -> bool {
        matches!(self, Self::UserAgent { .. })
    }

    pub(super) fn source_scope_fallback_roots(&self, host: &DomHost) -> Vec<DomHandle> {
        match self {
            Self::Stylesheet(source_id) => source_id.source_scope_fallback_roots(host),
            Self::UserAgent { document } => {
                StyleScopeId::Document(*document).source_scope_fallback_roots(host)
            }
            Self::FallbackRoot { scope_id, root } => {
                fallback_root_target_scope_roots(host, *scope_id, *root)
            }
        }
    }
}

impl StyleScopeId {
    fn for_stylesheet_owner(host: &DomHost, owner: DomHandle) -> Option<Self> {
        if let Some(root) = host.containing_shadow_root(owner)
            && shadow_root_host_participates_in_style_scope(host, root)
        {
            return Some(Self::ShadowRoot(root));
        }
        host.owner_document_handle(owner).map(Self::Document)
    }

    fn for_style_root(host: &DomHost, root: DomHandle) -> Option<Self> {
        if host.is_shadow_root(root) && shadow_root_host_participates_in_style_scope(host, root) {
            return Some(Self::ShadowRoot(root));
        }
        if let Some(shadow_root) = host.containing_shadow_root(root)
            && shadow_root_host_participates_in_style_scope(host, shadow_root)
        {
            return Some(Self::ShadowRoot(shadow_root));
        }
        host.owner_document_handle(root).map(Self::Document)
    }

    fn source_scope_fallback_roots(self, host: &DomHost) -> Vec<DomHandle> {
        match self {
            Self::Document(document) => {
                if host.node(document).is_some() {
                    vec![document]
                } else {
                    Vec::new()
                }
            }
            Self::ShadowRoot(root) => {
                if host.node(root).is_none()
                    || !shadow_root_host_participates_in_style_scope(host, root)
                {
                    return Vec::new();
                }
                let mut roots = vec![root];
                if let Some(shadow_host) = host.shadow_root_host(root) {
                    roots.push(shadow_host);
                }
                roots
            }
        }
    }
}

impl StyleSourceId {
    fn source_scope_fallback_roots(&self, host: &DomHost) -> Vec<DomHandle> {
        self.scope_id.source_scope_fallback_roots(host)
    }
}

fn fallback_root_target_scope_roots(
    host: &DomHost,
    scope_id: StyleScopeId,
    root: DomHandle,
) -> Vec<DomHandle> {
    if host.node(root).is_none() {
        return Vec::new();
    }
    let mut roots = vec![root];
    if let StyleScopeId::ShadowRoot(scope_root) = scope_id
        && root == scope_root
        && let Some(shadow_host) = host.shadow_root_host(scope_root)
    {
        roots.push(shadow_host);
    }
    roots
}
