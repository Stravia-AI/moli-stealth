use std::time::Duration;

use axum::extract::ws::WebSocket;
use moli_core::{page::RendererDocumentLifecycleMilestone, runtime::NavigationRuntimeConfig};
use moli_protocol::{
    CdpInitialStoragePartition, DevToolsPageResidenceIdentity,
    devtools_runtime::{
        DevToolsCommand, DevToolsCommandContext, DevToolsCommandResult, DevToolsError,
        DevToolsErrorKind,
    },
};
use tokio::sync::{mpsc, oneshot};

use super::{
    protocol_local_executor::spawn_protocol_local_task, webdriver_bidi::SharedBidiSessionRegistry,
};

mod actor;

/// Cloneable frontend endpoint for one application-owned DevTools Host.
///
/// The endpoint owns no scheduler, Browser Host actor, renderer receiver, or
/// protocol progress loop. Classic and BiDi frontends may wait for their own
/// replies through this channel without gaining the ability to pump Browser
/// progress.
#[derive(Debug, Clone)]
pub(super) struct DevToolsHostServiceHandle {
    tx: mpsc::UnboundedSender<DevToolsHostServiceRequest>,
}

pub(super) struct AttachedBidiFrontendLifetime {
    detached_rx: oneshot::Receiver<()>,
}

pub(super) struct DevToolsHostCommandExecution {
    pub(super) result: Result<DevToolsCommandResult, DevToolsError>,
    pub(super) page_residence: Option<DevToolsPageResidenceIdentity>,
}

impl AttachedBidiFrontendLifetime {
    pub(super) async fn wait(self) {
        let _ = self.detached_rx.await;
    }
}

#[derive(Debug)]
pub(super) enum BidiFrontendSession {
    Standalone,
    Existing {
        session_id: String,
        file_prompt_handler: Option<String>,
    },
}

enum DevToolsHostServiceRequest {
    Execute {
        command: Box<DevToolsCommand>,
        timeout: Option<Duration>,
        pending_navigation_timeout: Option<Duration>,
        expected_page: Option<DevToolsPageResidenceIdentity>,
        response_tx: oneshot::Sender<DevToolsHostCommandExecution>,
    },
    WaitForDocumentLifecycle {
        context: DevToolsCommandContext,
        milestone: RendererDocumentLifecycleMilestone,
        timeout: Option<Duration>,
        response_tx: oneshot::Sender<Result<(), DevToolsError>>,
    },
    SetJavaScriptDialogHandlerEnabled {
        enabled: bool,
        response_tx: oneshot::Sender<Result<(), DevToolsError>>,
    },
    AttachBidi {
        socket: Box<WebSocket>,
        web_socket_url: String,
        session: BidiFrontendSession,
        session_registry: SharedBidiSessionRegistry,
        response_tx: oneshot::Sender<Option<AttachedBidiFrontendLifetime>>,
    },
    Shutdown {
        response_tx: oneshot::Sender<()>,
    },
}

impl DevToolsHostServiceHandle {
    pub(super) fn spawn(
        initial_storage_partition: CdpInitialStoragePartition,
        navigation_runtime_config: NavigationRuntimeConfig,
    ) -> (Self, oneshot::Receiver<()>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let finished = spawn_protocol_local_task("devtools-host", move || {
            actor::run_devtools_host_service(
                rx,
                initial_storage_partition,
                navigation_runtime_config,
            )
        });
        (Self { tx }, finished)
    }

    pub(super) async fn execute(
        &self,
        command: DevToolsCommand,
        timeout: Option<Duration>,
        pending_navigation_timeout: Option<Duration>,
    ) -> Result<DevToolsCommandResult, DevToolsError> {
        self.execute_with_page_residence(command, timeout, pending_navigation_timeout, None)
            .await
            .result
    }

    pub(super) async fn execute_with_page_residence(
        &self,
        command: DevToolsCommand,
        timeout: Option<Duration>,
        pending_navigation_timeout: Option<Duration>,
        expected_page: Option<DevToolsPageResidenceIdentity>,
    ) -> DevToolsHostCommandExecution {
        let (response_tx, response_rx) = oneshot::channel();
        if self
            .tx
            .send(DevToolsHostServiceRequest::Execute {
                command: Box::new(command),
                timeout,
                pending_navigation_timeout,
                expected_page,
                response_tx,
            })
            .is_err()
        {
            return DevToolsHostCommandExecution {
                result: Err(devtools_host_stopped_error()),
                page_residence: None,
            };
        }
        response_rx
            .await
            .unwrap_or_else(|_| DevToolsHostCommandExecution {
                result: Err(devtools_host_stopped_error()),
                page_residence: None,
            })
    }

    pub(super) async fn wait_for_document_lifecycle(
        &self,
        context: DevToolsCommandContext,
        milestone: RendererDocumentLifecycleMilestone,
        timeout: Option<Duration>,
    ) -> Result<(), DevToolsError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(DevToolsHostServiceRequest::WaitForDocumentLifecycle {
                context,
                milestone,
                timeout,
                response_tx,
            })
            .map_err(|_| devtools_host_stopped_error())?;
        response_rx
            .await
            .map_err(|_| devtools_host_stopped_error())?
    }

    pub(super) async fn set_javascript_dialog_handler_enabled(
        &self,
        enabled: bool,
    ) -> Result<(), DevToolsError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(
                DevToolsHostServiceRequest::SetJavaScriptDialogHandlerEnabled {
                    enabled,
                    response_tx,
                },
            )
            .map_err(|_| devtools_host_stopped_error())?;
        response_rx
            .await
            .map_err(|_| devtools_host_stopped_error())?
    }

    pub(super) async fn attach_bidi(
        &self,
        socket: WebSocket,
        web_socket_url: String,
        session: BidiFrontendSession,
        session_registry: SharedBidiSessionRegistry,
    ) -> Option<AttachedBidiFrontendLifetime> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(DevToolsHostServiceRequest::AttachBidi {
                socket: Box::new(socket),
                web_socket_url,
                session,
                session_registry,
                response_tx,
            })
            .ok()?;
        response_rx.await.ok().flatten()
    }

    pub(super) async fn shutdown(&self) {
        let (response_tx, response_rx) = oneshot::channel();
        if self
            .tx
            .send(DevToolsHostServiceRequest::Shutdown { response_tx })
            .is_err()
        {
            return;
        }
        let _ = response_rx.await;
    }
}

fn devtools_host_stopped_error() -> DevToolsError {
    DevToolsError::new(
        DevToolsErrorKind::NoSuchSession,
        "DevTools Host service is closed",
    )
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use moli_core::runtime::storage_partition::StoragePartitionState;

    use super::*;

    #[tokio::test]
    async fn cloned_service_handle_keeps_exact_host_actor_alive() {
        let partition = Arc::new(
            StoragePartitionState::open(None)
                .expect("in-memory DevTools Host storage partition should open"),
        );
        let initial_storage_partition =
            CdpInitialStoragePartition::from_storage_partition(partition.as_ref());
        let (host, host_finished) = DevToolsHostServiceHandle::spawn(
            initial_storage_partition,
            NavigationRuntimeConfig::default(),
        );
        let surviving_frontend = host.clone();
        drop(host);

        surviving_frontend
            .set_javascript_dialog_handler_enabled(true)
            .await
            .expect("remaining frontend handle should reach the same live Host actor");
        surviving_frontend.shutdown().await;
        tokio::time::timeout(Duration::from_secs(1), host_finished)
            .await
            .expect("Host actor should finish after explicit service shutdown")
            .expect("Host actor should report completion");
    }
}
