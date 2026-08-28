use super::BrowserDocumentNavigation;

#[derive(Clone, Debug, Eq, PartialEq)]
enum BrowserTargetMetadataTransitionSource {
    NavigationCommitted(BrowserDocumentNavigation),
    DocumentTitleChanged,
}

/// Immutable Browser-owned reason and values for one top-level Target
/// metadata transition.
///
/// CDP attachment state is deliberately absent. A committed Document freezes
/// its URL and initial title, while later renderer-observed title updates carry
/// the current URL with a title-change source. Merely accepting a navigation,
/// including named-target reuse, is not a metadata transition: Chromium
/// reports the new values only after the successor Document commits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserTargetMetadataTransition {
    source: BrowserTargetMetadataTransitionSource,
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
            source: BrowserTargetMetadataTransitionSource::NavigationCommitted(navigation),
            url,
            title,
        }
    }

    pub(super) fn document_title_changed(url: String, title: String) -> Self {
        Self {
            source: BrowserTargetMetadataTransitionSource::DocumentTitleChanged,
            url,
            title,
        }
    }

    pub fn navigation(&self) -> Option<&BrowserDocumentNavigation> {
        match &self.source {
            BrowserTargetMetadataTransitionSource::NavigationCommitted(navigation) => {
                Some(navigation)
            }
            BrowserTargetMetadataTransitionSource::DocumentTitleChanged => None,
        }
    }

    pub fn is_document_title_change(&self) -> bool {
        matches!(
            self.source,
            BrowserTargetMetadataTransitionSource::DocumentTitleChanged
        )
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn title(&self) -> &str {
        &self.title
    }
}
