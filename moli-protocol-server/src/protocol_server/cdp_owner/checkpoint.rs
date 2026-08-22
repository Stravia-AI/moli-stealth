use std::sync::Arc;

use moli_core::runtime::storage_partition::StoragePartitionState;
use tokio::sync::mpsc;

/// Flushes the application-owned profile after Browser Host checkpoints.
///
/// Cookie mutations already live in `StoragePartitionState`; a checkpoint is
/// therefore only a persistence request. It never carries a frontend snapshot
/// and cannot merge stale connection state back into the Browser profile.
pub(super) fn spawn_checkpoint_worker(
    storage_partition: Arc<StoragePartitionState>,
    mut checkpoint_rx: mpsc::UnboundedReceiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while checkpoint_rx.recv().await.is_some() {
            // Coalesce a burst of detach/Target lifecycle checkpoints before
            // entering the blocking filesystem boundary.
            while checkpoint_rx.try_recv().is_ok() {}
            let partition = storage_partition.clone();
            match tokio::task::spawn_blocking(move || partition.flush()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(?error, "failed to flush CDP owner profile checkpoint");
                }
                Err(error) => {
                    tracing::warn!(?error, "CDP owner profile checkpoint worker panicked");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use moli_browser_profile::BrowserProfilePaths;
    use moli_cookie_jar::{StoredCookie, StoredCookieSameSite, StoredCookieSourceScheme};

    use super::*;

    struct TempProfile {
        path: PathBuf,
    }

    impl TempProfile {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            Self {
                path: std::env::temp_dir().join(format!(
                    "moli-cdp-profile-flush-{}-{nonce}",
                    std::process::id()
                )),
            }
        }
    }

    impl Drop for TempProfile {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn stored_cookie(name: &str, value: &str) -> StoredCookie {
        StoredCookie {
            name: name.to_owned(),
            value: value.to_owned(),
            domain: "example.com".to_owned(),
            host_only: false,
            path: "/".to_owned(),
            secure: false,
            http_only: false,
            expires: None,
            same_site: StoredCookieSameSite::Unspecified,
            priority: None,
            partition_key: None,
            source_scheme: StoredCookieSourceScheme::NonSecure,
            source_port: -1,
            creation_index: 0,
            last_access_index: 0,
        }
    }

    #[tokio::test]
    async fn checkpoint_flushes_live_browser_owned_cookie_store() {
        let profile = TempProfile::new();
        let partition =
            Arc::new(StoragePartitionState::open(Some(&profile.path)).expect("open test profile"));
        partition
            .import_cookies([stored_cookie("sid", "live")])
            .expect("import live cookie");
        let (checkpoint_tx, checkpoint_rx) = mpsc::unbounded_channel();
        let worker = spawn_checkpoint_worker(partition, checkpoint_rx);

        checkpoint_tx.send(()).expect("send profile flush");
        drop(checkpoint_tx);
        worker.await.expect("checkpoint worker");

        let paths = BrowserProfilePaths::new(&profile.path);
        let persisted = crate::cookie_cache::load_cookie_cache(&paths.cookies_path)
            .expect("read persisted cookie cache");
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].name, "sid");
        assert_eq!(persisted[0].value, "live");
    }
}
