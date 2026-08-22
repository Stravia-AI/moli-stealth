use super::PageResidenceIdentity;

/// Why an already-created auxiliary Target needs a top-level navigation.
///
/// The distinction is browser state, not a frontend projection detail. A new
/// auxiliary Target starts with an initial empty Document, while a named
/// Target reuse navigates an already-loaded Page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserAuxiliaryNavigationKind {
    InitialDocument,
    NamedTargetReuse,
}

/// Exact auxiliary-Target navigation admitted to Browser Owner.
///
/// Target creation/name resolution has already completed when this input is
/// published. The destination and exact Page residence are immutable, so a
/// delayed owner turn cannot follow a replacement Page or a frontend route.
#[derive(Debug)]
pub struct BrowserAuxiliaryNavigationInput {
    page_owner: PageResidenceIdentity,
    url: String,
    kind: BrowserAuxiliaryNavigationKind,
}

impl BrowserAuxiliaryNavigationInput {
    pub(super) fn new(
        page_owner: PageResidenceIdentity,
        url: String,
        kind: BrowserAuxiliaryNavigationKind,
    ) -> Self {
        Self {
            page_owner,
            url,
            kind,
        }
    }

    pub fn page_owner(&self) -> &PageResidenceIdentity {
        &self.page_owner
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn kind(&self) -> BrowserAuxiliaryNavigationKind {
        self.kind
    }

    pub fn into_parts(
        self,
    ) -> (
        PageResidenceIdentity,
        String,
        BrowserAuxiliaryNavigationKind,
    ) {
        (self.page_owner, self.url, self.kind)
    }
}
