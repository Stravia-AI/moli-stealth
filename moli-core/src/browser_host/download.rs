use std::{collections::HashMap, path::PathBuf, sync::Arc};

use moli_fetch::FetchCancelHandle;
use parking_lot::Mutex;

/// Protocol-neutral browser policy for one download scope.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BrowserDownloadBehavior {
    #[default]
    Default,
    Deny,
    Allow,
    AllowAndName,
}

impl BrowserDownloadBehavior {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "deny" => Some(Self::Deny),
            "allow" => Some(Self::Allow),
            "allowAndName" => Some(Self::AllowAndName),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Deny => "deny",
            Self::Allow => "allow",
            Self::AllowAndName => "allowAndName",
        }
    }

    pub fn allows_download(self) -> bool {
        matches!(self, Self::Allow | Self::AllowAndName)
    }

    pub fn names_artifact_by_guid(self) -> bool {
        self == Self::AllowAndName
    }

    pub fn is_canceled_without_download(self) -> bool {
        matches!(self, Self::Default | Self::Deny)
    }
}

/// Browser-owned effective download policy.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BrowserDownloadPolicy {
    behavior: BrowserDownloadBehavior,
    download_path: Option<String>,
}

impl BrowserDownloadPolicy {
    pub fn new(behavior: BrowserDownloadBehavior, download_path: Option<String>) -> Self {
        Self {
            behavior,
            download_path,
        }
    }

    pub fn behavior(&self) -> BrowserDownloadBehavior {
        self.behavior
    }

    pub fn download_path(&self) -> Option<&str> {
        self.download_path.as_deref()
    }
}

/// One closed mutation of browser-owned download policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserDownloadPolicyUpdate {
    SetGlobal {
        behavior: BrowserDownloadBehavior,
        download_path: Option<String>,
    },
    SetBrowserContext {
        browser_context_id: String,
        behavior: BrowserDownloadBehavior,
        download_path: Option<String>,
    },
    ResetGlobal,
    RemoveBrowserContext {
        browser_context_id: String,
    },
}

/// Application-owned policy shared by every frontend of one Browser Host.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BrowserDownloadPolicyState {
    global: BrowserDownloadPolicy,
    browser_context_overrides: HashMap<String, BrowserDownloadPolicy>,
}

impl BrowserDownloadPolicyState {
    pub fn effective_for_browser_context(
        &self,
        browser_context_id: Option<&str>,
    ) -> BrowserDownloadPolicy {
        browser_context_id
            .and_then(|browser_context_id| self.browser_context_overrides.get(browser_context_id))
            .cloned()
            .unwrap_or_else(|| self.global.clone())
    }

    pub fn global(&self) -> &BrowserDownloadPolicy {
        &self.global
    }

    pub fn browser_context_override(
        &self,
        browser_context_id: &str,
    ) -> Option<&BrowserDownloadPolicy> {
        self.browser_context_overrides.get(browser_context_id)
    }

    pub(crate) fn apply(&mut self, update: BrowserDownloadPolicyUpdate) {
        match update {
            BrowserDownloadPolicyUpdate::SetGlobal {
                behavior,
                download_path,
            } => {
                self.global = BrowserDownloadPolicy::new(behavior, download_path);
            }
            BrowserDownloadPolicyUpdate::SetBrowserContext {
                browser_context_id,
                behavior,
                download_path,
            } => {
                self.browser_context_overrides.insert(
                    browser_context_id,
                    BrowserDownloadPolicy::new(behavior, download_path),
                );
            }
            BrowserDownloadPolicyUpdate::ResetGlobal => {
                self.global = BrowserDownloadPolicy::default();
            }
            BrowserDownloadPolicyUpdate::RemoveBrowserContext { browser_context_id } => {
                self.browser_context_overrides.remove(&browser_context_id);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct BrowserDownloadRecord {
    state: BrowserDownloadLifecycle,
    artifact_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
enum BrowserDownloadLifecycle {
    Active(FetchCancelHandle),
    Completed,
    Canceled,
}

/// Cross-frontend registry for accepted downloads and their artifacts.
///
/// Download participants may finish on background Tokio tasks, so this
/// protocol-neutral handle is thread-safe even though the surrounding Browser
/// Host actor remains current-thread owned.
#[derive(Clone, Default)]
pub struct BrowserDownloadRegistry {
    inner: Arc<Mutex<HashMap<String, BrowserDownloadRecord>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserDownloadCancelOutcome {
    Handled,
    AlreadyTerminal,
    NotFound,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserDownloadArtifactOutcome {
    Ready(PathBuf),
    InProgress,
    NotAvailable,
    NotFound,
}

impl BrowserDownloadRegistry {
    pub fn register_active(&self, guid: String, cancel_handle: FetchCancelHandle) {
        self.with_mut(|downloads| {
            downloads.insert(
                guid,
                BrowserDownloadRecord {
                    state: BrowserDownloadLifecycle::Active(cancel_handle),
                    artifact_path: None,
                },
            );
        });
    }

    pub fn record_completed(&self, guid: &str, artifact_path: PathBuf) {
        self.with_mut(|downloads| match downloads.get_mut(guid) {
            Some(record) => {
                record.state = BrowserDownloadLifecycle::Completed;
                record.artifact_path = Some(artifact_path);
            }
            None => {
                downloads.insert(
                    guid.to_owned(),
                    BrowserDownloadRecord {
                        state: BrowserDownloadLifecycle::Completed,
                        artifact_path: Some(artifact_path),
                    },
                );
            }
        });
    }

    pub fn record_canceled(&self, guid: &str) {
        self.with_mut(|downloads| match downloads.get_mut(guid) {
            Some(record) => record.state = BrowserDownloadLifecycle::Canceled,
            None => {
                downloads.insert(
                    guid.to_owned(),
                    BrowserDownloadRecord {
                        state: BrowserDownloadLifecycle::Canceled,
                        artifact_path: None,
                    },
                );
            }
        });
    }

    pub fn cancel(&self, guid: &str) -> BrowserDownloadCancelOutcome {
        self.with_mut(|downloads| match downloads.get_mut(guid) {
            Some(BrowserDownloadRecord {
                state: BrowserDownloadLifecycle::Active(cancel_handle),
                ..
            }) => {
                cancel_handle.cancel();
                BrowserDownloadCancelOutcome::Handled
            }
            Some(_) => BrowserDownloadCancelOutcome::AlreadyTerminal,
            None => BrowserDownloadCancelOutcome::NotFound,
        })
    }

    pub fn artifact(&self, guid: &str) -> BrowserDownloadArtifactOutcome {
        self.with_mut(|downloads| match downloads.get(guid) {
            Some(BrowserDownloadRecord {
                state: BrowserDownloadLifecycle::Completed,
                artifact_path: Some(path),
            }) => BrowserDownloadArtifactOutcome::Ready(path.clone()),
            Some(BrowserDownloadRecord {
                state: BrowserDownloadLifecycle::Active(_),
                ..
            }) => BrowserDownloadArtifactOutcome::InProgress,
            Some(_) => BrowserDownloadArtifactOutcome::NotAvailable,
            None => BrowserDownloadArtifactOutcome::NotFound,
        })
    }

    fn with_mut<T>(
        &self,
        operation: impl FnOnce(&mut HashMap<String, BrowserDownloadRecord>) -> T,
    ) -> T {
        operation(&mut self.inner.lock())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_policy_overrides_global_behavior_and_path() {
        let mut state = BrowserDownloadPolicyState::default();
        state.apply(BrowserDownloadPolicyUpdate::SetGlobal {
            behavior: BrowserDownloadBehavior::Deny,
            download_path: None,
        });
        state.apply(BrowserDownloadPolicyUpdate::SetBrowserContext {
            browser_context_id: "context-a".to_owned(),
            behavior: BrowserDownloadBehavior::AllowAndName,
            download_path: Some("/second".to_owned()),
        });

        let policy = state.effective_for_browser_context(Some("context-a"));
        assert_eq!(policy.behavior(), BrowserDownloadBehavior::AllowAndName);
        assert_eq!(policy.download_path(), Some("/second"));
    }

    #[test]
    fn registry_handle_keeps_terminal_artifact_visible() {
        let registry = BrowserDownloadRegistry::default();
        let observer = registry.clone();
        let path = PathBuf::from("/tmp/download-artifact");

        registry.record_completed("download-1", path.clone());
        drop(registry);

        assert_eq!(
            observer.artifact("download-1"),
            BrowserDownloadArtifactOutcome::Ready(path)
        );
    }
}
