//! Connection reuse and independent connection-cap accounting.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use http2::client::SendRequest;
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};

use crate::{ConnectedStream, TransportError};

const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct OriginKey {
    pub(crate) route: String,
    pub(crate) host: String,
}

pub(crate) struct ConnectionPermits {
    _total: Option<OwnedSemaphorePermit>,
    _host: Option<OwnedSemaphorePermit>,
}

pub(crate) struct PooledH1 {
    pub(crate) connected: ConnectedStream,
    pub(crate) _permits: ConnectionPermits,
}

#[derive(Clone)]
pub(crate) struct PooledH2 {
    pub(crate) sender: SendRequest<Bytes>,
    stream_slots: Option<Arc<Semaphore>>,
    identity: Arc<()>,
    _wake_pool: WakePoolOnDrop,
}

pub(crate) struct H2StreamPermit {
    _limit: Option<OwnedSemaphorePermit>,
    _identity: Arc<()>,
    _wake_pool: WakePoolOnDrop,
}

// Declared after the sender, semaphore and identity fields so their release is
// visible before a capacity waiter checks whether this connection is idle.
#[derive(Clone)]
struct WakePoolOnDrop(Arc<Notify>);

impl Drop for WakePoolOnDrop {
    fn drop(&mut self) {
        self.0.notify_waiters();
    }
}

impl PooledH2 {
    pub(crate) async fn acquire_stream(&self) -> Result<H2StreamPermit, TransportError> {
        let limit = match &self.stream_slots {
            Some(slots) => slots
                .clone()
                .acquire_owned()
                .await
                .map(Some)
                .map_err(|_| TransportError::Cancelled)?,
            None => None,
        };
        Ok(H2StreamPermit {
            _limit: limit,
            _identity: Arc::clone(&self.identity),
            _wake_pool: self._wake_pool.clone(),
        })
    }

    fn is_idle(&self) -> bool {
        Arc::strong_count(&self.identity) == 1
    }

    fn is_same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }
}

struct H1Entry {
    connection: PooledH1,
    last_used: Instant,
}

struct H2Entry {
    connection: PooledH2,
    last_used: Instant,
}

struct PoolState {
    h1: HashMap<OriginKey, Vec<H1Entry>>,
    h2: HashMap<OriginKey, H2Entry>,
}

#[derive(Clone)]
pub(crate) struct ConnectionPool {
    state: Arc<Mutex<PoolState>>,
    idle_returned: Arc<Notify>,
    total: Option<Arc<Semaphore>>,
    per_host_limit: Option<usize>,
    host_semaphores: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
    h2_connection_gates: Arc<Mutex<HashMap<OriginKey, Arc<Semaphore>>>>,
}

impl ConnectionPool {
    pub(crate) fn new(
        total: Option<usize>,
        per_host: Option<usize>,
    ) -> Result<Self, TransportError> {
        if matches!(total, Some(0)) {
            return Err(TransportError::InvalidInput(
                "max_connections must be greater than zero".into(),
            ));
        }
        if matches!(per_host, Some(0)) {
            return Err(TransportError::InvalidInput(
                "max_host_connections must be greater than zero".into(),
            ));
        }
        Ok(Self {
            state: Arc::new(Mutex::new(PoolState {
                h1: HashMap::new(),
                h2: HashMap::new(),
            })),
            idle_returned: Arc::new(Notify::new()),
            total: total.map(|limit| Arc::new(Semaphore::new(limit))),
            per_host_limit: per_host,
            host_semaphores: Arc::new(Mutex::new(HashMap::new())),
            h2_connection_gates: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub(crate) fn new_h2_connection(
        &self,
        sender: SendRequest<Bytes>,
        max_streams: Option<usize>,
    ) -> PooledH2 {
        PooledH2 {
            sender,
            stream_slots: max_streams.map(|limit| Arc::new(Semaphore::new(limit))),
            identity: Arc::new(()),
            _wake_pool: WakePoolOnDrop(Arc::clone(&self.idle_returned)),
        }
    }

    pub(crate) async fn take_h1(&self, key: &OriginKey) -> Option<PooledH1> {
        let mut state = self.state.lock().await;
        let entries = state.h1.get_mut(key)?;
        while let Some(entry) = entries.pop() {
            if entry.last_used.elapsed() < IDLE_TIMEOUT {
                return Some(entry.connection);
            }
        }
        state.h1.remove(key);
        None
    }

    pub(crate) async fn put_h1(&self, key: OriginKey, connection: PooledH1) {
        let mut state = self.state.lock().await;
        state.h1.entry(key).or_default().push(H1Entry {
            connection,
            last_used: Instant::now(),
        });
        drop(state);
        self.idle_returned.notify_waiters();
    }

    pub(crate) async fn get_h2(&self, key: &OriginKey) -> Option<PooledH2> {
        let mut state = self.state.lock().await;
        let entry = state.h2.get_mut(key)?;
        if entry.connection.is_idle() && entry.last_used.elapsed() >= IDLE_TIMEOUT {
            state.h2.remove(key);
            return None;
        }
        entry.last_used = Instant::now();
        Some(entry.connection.clone())
    }

    pub(crate) async fn put_h2(&self, key: OriginKey, connection: PooledH2) {
        self.state.lock().await.h2.insert(
            key,
            H2Entry {
                connection,
                last_used: Instant::now(),
            },
        );
        self.idle_returned.notify_waiters();
    }

    pub(crate) async fn evict_h2(&self, key: &OriginKey, connection: &PooledH2) {
        let mut state = self.state.lock().await;
        if state
            .h2
            .get(key)
            .is_some_and(|entry| entry.connection.is_same_connection(connection))
        {
            state.h2.remove(key);
        }
    }

    /// Serializes connection establishment for one origin. Waiters hold no
    /// physical connection permits and re-check the pool after acquiring it.
    pub(crate) async fn acquire_h2_connection_gate(
        &self,
        key: &OriginKey,
    ) -> Result<OwnedSemaphorePermit, TransportError> {
        let gate = {
            let mut gates = self.h2_connection_gates.lock().await;
            gates
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Semaphore::new(1)))
                .clone()
        };
        gate.acquire_owned()
            .await
            .map_err(|_| TransportError::Cancelled)
    }

    /// Reserve a physical connection slot. Idle entries are discarded before
    /// waiting, so an idle origin cannot monopolize a global connection cap.
    pub(crate) async fn acquire(&self, host: &str) -> Result<ConnectionPermits, TransportError> {
        loop {
            let idle_returned = self.idle_returned.notified();
            tokio::pin!(idle_returned);
            idle_returned.as_mut().enable();
            self.evict_idle_for_capacity(host).await;

            // A completed response can park its permit in the idle pool after
            // eviction. Wake existing waiters instead of waiting forever for
            // a semaphore permit that only another eviction can release.
            tokio::select! {
                result = self.acquire_permits(host) => return result,
                () = &mut idle_returned => {}
            }
        }
    }

    async fn acquire_permits(&self, host: &str) -> Result<ConnectionPermits, TransportError> {
        let host_limit = if let Some(limit) = self.per_host_limit {
            let semaphore = {
                let mut semaphores = self.host_semaphores.lock().await;
                semaphores
                    .entry(host.to_owned())
                    .or_insert_with(|| Arc::new(Semaphore::new(limit)))
                    .clone()
            };
            Some(
                semaphore
                    .acquire_owned()
                    .await
                    .map_err(|_| TransportError::Cancelled)?,
            )
        } else {
            None
        };

        let total = match &self.total {
            Some(limit) => Some(
                limit
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| TransportError::Cancelled)?,
            ),
            None => None,
        };

        Ok(ConnectionPermits {
            _total: total,
            _host: host_limit,
        })
    }

    async fn evict_idle_for_capacity(&self, host: &str) {
        let total_full = self
            .total
            .as_ref()
            .is_some_and(|limit| limit.available_permits() == 0);
        let host_full = if let Some(limit) = self.per_host_limit {
            let semaphore = self.host_semaphores.lock().await.get(host).cloned();
            semaphore.is_some_and(|value| value.available_permits() == 0 && limit != 0)
        } else {
            false
        };
        if !total_full && !host_full {
            return;
        }

        let mut state = self.state.lock().await;
        let key = state
            .h1
            .keys()
            .find(|key| total_full || key.host == host)
            .cloned();
        if let Some(key) = key {
            state.h1.remove(&key);
            return;
        }
        let key = state
            .h2
            .iter()
            .find(|(key, entry)| (total_full || key.host == host) && entry.connection.is_idle())
            .map(|(key, _)| key.clone());
        if let Some(key) = key {
            state.h2.remove(&key);
        }
    }
}
