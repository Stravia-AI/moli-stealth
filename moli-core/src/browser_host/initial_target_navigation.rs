use super::{BrowserCommandId, PageResidenceIdentity};

/// Exact initial Target URL navigation admitted to Browser Owner.
///
/// A Target is first materialized with its initial empty Document and may have
/// a different creation URL waiting to replace it. DevTools commands can
/// trigger that transition, but neither their session nor their response
/// route owns it. The immutable destination and exact Page residence are all
/// the Browser Host mailbox needs.
#[derive(Debug)]
pub struct BrowserInitialTargetNavigationInput {
    page_owner: PageResidenceIdentity,
    url: String,
}

impl BrowserInitialTargetNavigationInput {
    pub(super) fn new(page_owner: PageResidenceIdentity, url: String) -> Self {
        Self { page_owner, url }
    }

    pub fn page_owner(&self) -> &PageResidenceIdentity {
        &self.page_owner
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn into_parts(self) -> (PageResidenceIdentity, String) {
        (self.page_owner, self.url)
    }
}

/// Frontend request to ensure one exact Target has left its initial empty
/// Document before the dependent command continues.
///
/// `BrowserCommandId` is opaque Browser/Protocol correlation. CDP request ids,
/// sessions, response payloads and the dependent command state remain in the
/// frontend sidecar; Browser Owner receives only the browser action and exact
/// Page authority it must serialize.
#[derive(Debug)]
pub struct BrowserInitialTargetNavigationCommandInput {
    command_id: BrowserCommandId,
    page_owner: PageResidenceIdentity,
    url: String,
}

impl BrowserInitialTargetNavigationCommandInput {
    pub(super) fn new(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
        url: String,
    ) -> Self {
        Self {
            command_id,
            page_owner,
            url,
        }
    }

    pub fn command_id(&self) -> BrowserCommandId {
        self.command_id
    }

    pub fn page_owner(&self) -> &PageResidenceIdentity {
        &self.page_owner
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn into_parts(self) -> (BrowserCommandId, PageResidenceIdentity, String) {
        (self.command_id, self.page_owner, self.url)
    }
}
