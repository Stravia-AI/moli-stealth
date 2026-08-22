use super::BrowserDocumentNavigation;

/// Immutable Browser-owned reason and values for one top-level Target
/// metadata transition.
///
/// CDP attachment state is deliberately absent. A committed Document freezes
/// both URL and title. Merely accepting a navigation, including named-target
/// reuse, is not a metadata transition: Chromium reports the new values only
/// after the successor Document commits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserTargetMetadataTransition {
    navigation: BrowserDocumentNavigation,
    url: String,
    title: String,
}

impl BrowserTargetMetadataTransition {
    pub(super) fn navigation_committed(
        navigation: BrowserDocumentNavigation,
        url: String,
        title: String,
    ) -> Self {
        Self {
            navigation,
            url,
            title,
        }
    }

    pub fn navigation(&self) -> &BrowserDocumentNavigation {
        &self.navigation
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn title(&self) -> &str {
        &self.title
    }
}
