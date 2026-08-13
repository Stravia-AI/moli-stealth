use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use moli_protocol::{
    CdpTargetHostLifecycleDelta, CdpTargetHostLifecycleObserver, DevToolsTargetInfo,
    DevToolsTargetKind,
};
use parking_lot::Mutex;

use crate::cdp_frontend::CdpFrontendEndpoint;

use super::DEFAULT_TARGET_ID;

#[derive(Clone, Default)]
pub(super) struct SharedCdpAgentHostDirectory {
    inner: Arc<Mutex<CdpAgentHostDirectoryState>>,
    next_owner_id: Arc<AtomicU64>,
}

#[derive(Default)]
struct CdpAgentHostDirectoryState {
    page_hosts: HashMap<String, CdpPageAgentHost>,
}

#[derive(Clone)]
struct CdpPageAgentHost {
    owner_id: u64,
    target_info: DevToolsTargetInfo,
    endpoint: CdpFrontendEndpoint,
}

#[derive(Clone)]
pub(super) struct CdpPageAgentHostRoute {
    pub(super) endpoint: CdpFrontendEndpoint,
    pub(super) target_info: DevToolsTargetInfo,
}

impl SharedCdpAgentHostDirectory {
    pub(super) fn allocate_owner_id(&self) -> u64 {
        self.next_owner_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(super) fn lifecycle_observer(
        &self,
        owner_id: u64,
        endpoint: CdpFrontendEndpoint,
    ) -> CdpTargetHostLifecycleObserver {
        let directory = self.clone();
        CdpTargetHostLifecycleObserver::new(move |delta| {
            directory.apply_delta(owner_id, &endpoint, delta);
        })
    }

    pub(super) fn lookup_page(&self, target_id: &str) -> Option<CdpPageAgentHostRoute> {
        let state = self.inner.lock();
        let host = state.page_hosts.get(target_id)?;
        (host.target_info.kind == DevToolsTargetKind::Page).then(|| CdpPageAgentHostRoute {
            endpoint: host.endpoint.clone(),
            target_info: host.target_info.clone(),
        })
    }

    pub(super) fn page_target_infos(&self) -> Vec<DevToolsTargetInfo> {
        let state = self.inner.lock();
        let mut target_infos = state
            .page_hosts
            .values()
            .filter(|host| host.target_info.kind == DevToolsTargetKind::Page)
            .map(|host| host.target_info.clone())
            .collect::<Vec<_>>();
        target_infos.sort_by(|left, right| {
            let left_id = left.target_id.as_ref().map(|id| id.as_str()).unwrap_or("");
            let right_id = right.target_id.as_ref().map(|id| id.as_str()).unwrap_or("");
            left_id.cmp(right_id)
        });
        target_infos
    }

    pub(super) fn remove_owner(&self, owner_id: u64) {
        self.inner
            .lock()
            .page_hosts
            .retain(|_, host| host.owner_id != owner_id);
    }

    fn apply_delta(
        &self,
        owner_id: u64,
        endpoint: &CdpFrontendEndpoint,
        delta: CdpTargetHostLifecycleDelta,
    ) {
        match delta {
            CdpTargetHostLifecycleDelta::Created(target_info) => {
                let Some(target_id) = page_target_id(&target_info) else {
                    return;
                };
                if target_id == DEFAULT_TARGET_ID {
                    return;
                }
                let mut state = self.inner.lock();
                if let Some(existing) = state.page_hosts.get(&target_id)
                    && existing.owner_id != owner_id
                {
                    tracing::warn!(
                        target_id,
                        existing_owner_id = existing.owner_id,
                        owner_id,
                        "refusing to replace a live CDP page agent host"
                    );
                    return;
                }
                state.page_hosts.insert(
                    target_id,
                    CdpPageAgentHost {
                        owner_id,
                        target_info,
                        endpoint: endpoint.clone(),
                    },
                );
            }
            CdpTargetHostLifecycleDelta::InfoChanged(target_info) => {
                let Some(target_id) = page_target_id(&target_info) else {
                    return;
                };
                let mut state = self.inner.lock();
                if let Some(host) = state.page_hosts.get_mut(&target_id)
                    && host.owner_id == owner_id
                {
                    host.target_info = target_info;
                }
            }
            CdpTargetHostLifecycleDelta::Destroyed { target_id } => {
                let endpoint = {
                    let mut state = self.inner.lock();
                    if state
                        .page_hosts
                        .get(&target_id)
                        .is_some_and(|host| host.owner_id == owner_id)
                    {
                        state
                            .page_hosts
                            .remove(&target_id)
                            .map(|host| host.endpoint)
                    } else {
                        None
                    }
                };
                if let Some(endpoint) = endpoint {
                    endpoint.target_destroyed(target_id);
                }
            }
        }
    }
}

fn page_target_id(target_info: &DevToolsTargetInfo) -> Option<String> {
    if target_info.kind != DevToolsTargetKind::Page {
        return None;
    }
    target_info
        .target_id
        .as_ref()
        .map(|target_id| target_id.as_str().to_owned())
}
