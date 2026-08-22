use super::BrowserTargetId;

/// Protocol-neutral terminal result for one frontend-originated navigation
/// command.
///
/// `Completed` means the command's response policy has reached its response
/// boundary. It does not imply that the resulting Document has reached DCL or
/// load; those remain independently observed browser lifecycle facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserNavigateCommandOutcome {
    Completed(BrowserNavigateCommandResult),
    Rejected(BrowserNavigateCommandError),
}

impl BrowserNavigateCommandOutcome {
    pub fn with_history_traversal(self, result: BrowserHistoryTraversalResult) -> Self {
        match self {
            Self::Completed(completed) => Self::Completed(completed.with_history_traversal(result)),
            Self::Rejected(error) => Self::Rejected(error),
        }
    }
}

/// Browser-visible navigation metadata available when a command response is
/// ready.
///
/// `is_download` is optional because a normal Document navigation does not
/// need to expose a download classification. A failed or download navigation
/// may additionally carry `error_text` while still completing the frontend
/// command successfully.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserNavigateCommandResult {
    requested_url: String,
    target_id: Option<BrowserTargetId>,
    loader_id: Option<String>,
    error_text: Option<String>,
    is_download: Option<bool>,
    history_traversal: Option<BrowserHistoryTraversalResult>,
}

impl BrowserNavigateCommandResult {
    pub fn new(
        requested_url: impl Into<String>,
        target_id: Option<BrowserTargetId>,
        loader_id: Option<String>,
        error_text: Option<String>,
        is_download: Option<bool>,
    ) -> Self {
        Self {
            requested_url: requested_url.into(),
            target_id,
            loader_id,
            error_text,
            is_download,
            history_traversal: None,
        }
    }

    pub fn with_history_traversal(mut self, result: BrowserHistoryTraversalResult) -> Self {
        self.history_traversal = Some(result);
        self
    }

    pub fn requested_url(&self) -> &str {
        &self.requested_url
    }

    pub fn target_id(&self) -> Option<&BrowserTargetId> {
        self.target_id.as_ref()
    }

    pub fn loader_id(&self) -> Option<&str> {
        self.loader_id.as_deref()
    }

    pub fn error_text(&self) -> Option<&str> {
        self.error_text.as_deref()
    }

    pub fn is_download(&self) -> Option<bool> {
        self.is_download
    }

    pub fn history_traversal(&self) -> Option<BrowserHistoryTraversalResult> {
        self.history_traversal
    }
}

/// Browser-owned classification of a completed joint session-history command.
///
/// This is terminal command metadata, not a frontend inference from URL or a
/// pre-selection history snapshot. A same-Document attempt that falls back to
/// loading its entry URL is reported as [`CrossDocument`](Self::CrossDocument).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserHistoryTraversalResult {
    Noop,
    SameDocument,
    CrossDocument,
}

/// Protocol-neutral reason that a navigation command could not reach its
/// normal response boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserNavigateCommandError {
    kind: BrowserNavigateCommandErrorKind,
    message: String,
}

impl BrowserNavigateCommandError {
    pub fn new(kind: BrowserNavigateCommandErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> BrowserNavigateCommandErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Stable browser-command rejection classes. Protocol frontends map these to
/// their own wire error codes and shapes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserNavigateCommandErrorKind {
    InvalidInput,
    RequesterUnavailable,
    TargetUnavailable,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigate_outcome_contains_no_frontend_correlation() {
        let outcome = BrowserNavigateCommandOutcome::Completed(BrowserNavigateCommandResult::new(
            "https://example.test/",
            Some(BrowserTargetId::new("target-1")),
            Some("loader-1".to_owned()),
            None,
            None,
        ));

        let BrowserNavigateCommandOutcome::Completed(result) = outcome else {
            panic!("expected completed navigation outcome");
        };
        assert_eq!(result.requested_url(), "https://example.test/");
        assert_eq!(
            result.target_id().map(BrowserTargetId::as_str),
            Some("target-1")
        );
        assert_eq!(result.loader_id(), Some("loader-1"));
        assert_eq!(result.error_text(), None);
        assert_eq!(result.is_download(), None);
        assert_eq!(result.history_traversal(), None);
    }

    #[test]
    fn navigate_outcome_can_carry_browser_owned_history_classification() {
        let result = BrowserNavigateCommandResult::new(
            "https://example.test/#before",
            None,
            None,
            None,
            None,
        )
        .with_history_traversal(BrowserHistoryTraversalResult::SameDocument);

        assert_eq!(
            result.history_traversal(),
            Some(BrowserHistoryTraversalResult::SameDocument)
        );
    }
}
