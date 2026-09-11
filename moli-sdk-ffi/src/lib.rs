//! SDK 私有 C 连接层。所有 Rust 对象和分配均留在各自一侧。
mod operations;
use futures_util::{
    FutureExt,
    stream::{FuturesUnordered, StreamExt},
};
use moli_sdk_types::{
    Error, ErrorKind, Result,
    wire::{Command, Reply},
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::Arc,
};
use tokio::sync::{RwLock, mpsc, watch};

type Callback = unsafe extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize);
struct Completion {
    callback: Callback,
    context: *mut c_void,
    delivered: bool,
}
// Context is opaque host-owned notification storage; it is accessed only by its callback.
unsafe impl Send for Completion {}
impl Completion {
    fn finish(mut self, result: Result<Output>) {
        let (reply, binary) = match result {
            Ok(output) => (Ok(output.reply), output.binary),
            Err(error) => (Err(error), bytes::Bytes::new()),
        };
        let encoded =
            serde_json::to_vec(&reply).expect("SDK response contains only serializable JSON");
        self.delivered = true;
        unsafe {
            (self.callback)(
                self.context,
                encoded.as_ptr(),
                encoded.len(),
                binary.as_ptr(),
                binary.len(),
            )
        };
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        if !self.delivered {
            let encoded = br#"{"Err":{"kind":"Internal","message":"Moli owner stopped before completing the operation"}}"#;
            unsafe {
                (self.callback)(
                    self.context,
                    encoded.as_ptr(),
                    encoded.len(),
                    std::ptr::null(),
                    0,
                )
            };
        }
    }
}
struct Endpoint {
    sender: mpsc::UnboundedSender<Message>,
}
struct Operation {
    cancelled: watch::Sender<bool>,
}
impl Operation {
    fn new() -> Self {
        Self {
            cancelled: watch::channel(false).0,
        }
    }
    fn cancel(&self) {
        self.cancelled.send_replace(true);
    }
    async fn cancelled(&self) {
        let mut receiver = self.cancelled.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}
enum Message {
    Execute {
        id: u64,
        command: Box<Command>,
        operation: Arc<Operation>,
        completion: Completion,
    },
    Release(u64),
}
pub(crate) struct Output {
    reply: Reply,
    binary: bytes::Bytes,
}
impl Output {
    fn value(value: impl serde::Serialize) -> Result<Self> {
        Ok(Self {
            reply: Reply {
                value: serde_json::to_value(value).map_err(internal)?,
                resource: None,
            },
            binary: bytes::Bytes::new(),
        })
    }
    fn resource(id: u64, value: impl serde::Serialize) -> Result<Self> {
        let mut output = Self::value(value)?;
        output.reply.resource = Some(id);
        Ok(output)
    }
    fn binary(value: bool, binary: bytes::Bytes) -> Result<Self> {
        let mut output = Self::value(value)?;
        output.binary = binary;
        Ok(output)
    }
}
fn internal(error: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Internal, error.to_string())
}
fn closed() -> Error {
    Error::new(ErrorKind::Closed, "资源已关闭")
}
struct Slot {
    parent: u64,
    cancel: Operation,
    value: RwLock<Option<operations::Resource>>,
}
struct State {
    slots: RefCell<HashMap<u64, Rc<Slot>>>,
    next: Cell<u64>,
    initialized: Cell<bool>,
}
impl State {
    fn new() -> Self {
        Self {
            slots: RefCell::new(HashMap::from([(
                0,
                Rc::new(Slot {
                    parent: 0,
                    cancel: Operation::new(),
                    value: RwLock::new(Some(operations::Resource::Session)),
                }),
            )])),
            next: Cell::new(1),
            initialized: Cell::new(false),
        }
    }
    fn get(&self, id: u64) -> Result<Rc<Slot>> {
        self.slots.borrow().get(&id).cloned().ok_or_else(closed)
    }
    fn insert(&self, parent: u64, resource: operations::Resource) -> Result<u64> {
        let parent_slot = self.get(parent)?;
        if *parent_slot.cancel.cancelled.borrow() {
            return Err(closed());
        }
        let id = self.next.get();
        self.next
            .set(id.checked_add(1).ok_or_else(|| internal("资源 ID 耗尽"))?);
        self.slots.borrow_mut().insert(
            id,
            Rc::new(Slot {
                parent,
                cancel: Operation::new(),
                value: RwLock::new(Some(resource)),
            }),
        );
        Ok(id)
    }
    async fn close(&self, id: u64) -> Result<Output> {
        let mut ids = vec![id];
        {
            let slots = self.slots.borrow();
            let mut index = 0;
            while index < ids.len() {
                let parent = ids[index];
                for (&child, slot) in slots.iter() {
                    if child != parent && slot.parent == parent {
                        ids.push(child);
                    }
                }
                index += 1;
            }
            for id in &ids {
                if let Some(slot) = slots.get(id) {
                    slot.cancel.cancel();
                }
            }
        }
        let mut failure = None;
        // Descendants first. Every running operation holds this same slot read lock;
        // cancellation drops its future before cleanup acquires the resource.
        for id in ids.into_iter().rev() {
            let slot = self.slots.borrow().get(&id).cloned();
            if let Some(slot) = slot {
                if let Some(resource) = slot.value.write().await.take()
                    && let Err(error) = resource.close().await
                {
                    failure.get_or_insert(error);
                }
                self.slots.borrow_mut().remove(&id);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Output::value(()),
        }
    }
}

async fn serve(
    mut receiver: mpsc::UnboundedReceiver<Message>,
) -> Vec<(Completion, Result<Output>)> {
    let state = Rc::new(State::new());
    let mut tasks = FuturesUnordered::new();
    let mut shutdown = Vec::new();
    loop {
        tokio::select! {
            message = receiver.recv() => match message {
                Some(Message::Release(0)) | None => break,
                Some(Message::Release(id)) => { let state = state.clone(); tasks.push(tokio::task::spawn_local(async move { let _ = state.close(id).await; })); }
                Some(Message::Execute { id, command, operation, completion }) => {
                    let command = *command;
                    if id == 0 && matches!(command, Command::Close) {
                        shutdown.push(completion);
                        break;
                    }
                    let state = state.clone();
                    tasks.push(tokio::task::spawn_local(async move {
                        let result = AssertUnwindSafe(async {
                            if matches!(command, Command::Close) { return state.close(id).await; }
                            let slot = state.get(id)?;
                            tokio::select! {
                                biased;
                                _ = operation.cancelled() => Err(Error::new(ErrorKind::Cancelled, "操作已取消")),
                                _ = slot.cancel.cancelled() => Err(closed()),
                                result = async {
                                    let resource = slot.value.read().await;
                                    let resource = resource.as_ref().ok_or_else(closed)?;
                                    operations::execute(&state, id, resource, command).await
                                } => result,
                            }
                        }).catch_unwind().await.unwrap_or_else(|_| Err(internal("Moli 操作发生 panic")));
                        completion.finish(result);
                    }));
                }
            },
            _ = tasks.next(), if !tasks.is_empty() => {},
        }
    }
    receiver.close();
    while let Some(message) = receiver.recv().await {
        if let Message::Execute {
            command,
            completion,
            ..
        } = message
        {
            if matches!(*command, Command::Close) {
                shutdown.push(completion);
            } else {
                completion.finish(Err(closed()));
            }
        }
    }
    let cleanup = AssertUnwindSafe(state.close(0))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(Error::new(ErrorKind::Cleanup, "Moli 清理发生 panic")));
    while tasks.next().await.is_some() {}
    let error = cleanup.err();
    shutdown
        .into_iter()
        .map(|completion| {
            (
                completion,
                match &error {
                    Some(error) => Err(error.clone()),
                    None => Output::value(()),
                },
            )
        })
        .collect()
}

/// # Safety
/// `data` must be writable for `capacity` bytes, or null for a size-only query.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moli_sdk_abi(data: *mut u8, capacity: usize) -> usize {
    let identity = include_str!("../../moli-sdk/abi.txt").trim().as_bytes();
    if capacity >= identity.len() && !data.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(identity.as_ptr(), data, identity.len()) };
    }
    identity.len()
}
#[unsafe(no_mangle)]
pub extern "C" fn moli_sdk_open() -> *mut c_void {
    catch_unwind(|| {
        let (sender, receiver) = mpsc::unbounded_channel();
        let thread = std::thread::Builder::new()
            .name("moli-sdk-owner".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => {
                        let completions = {
                            let local = tokio::task::LocalSet::new();
                            runtime.block_on(local.run_until(serve(receiver)))
                        };
                        drop(runtime);
                        // Explicit session close is acknowledged only after runtime teardown.
                        for (completion, result) in completions {
                            completion.finish(result);
                        }
                    }
                    Err(error) => {
                        let mut receiver = receiver;
                        while let Some(message) = receiver.blocking_recv() {
                            if let Message::Execute { completion, .. } = message {
                                completion.finish(Err(Error::new(
                                    ErrorKind::Initialization,
                                    error.to_string(),
                                )));
                            }
                        }
                    }
                }
            });
        if thread.is_err() {
            return std::ptr::null_mut();
        }
        Box::into_raw(Box::new(Endpoint { sender })).cast()
    })
    .unwrap_or(std::ptr::null_mut())
}
/// # Safety
/// `runtime` must remain live through this call. Input slices must be readable
/// for their lengths. The callback owns `context` exactly once after submission,
/// may run synchronously, and must not unwind. Callback slices are borrowed only
/// for that invocation. The returned operation must be released exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moli_sdk_submit(
    runtime: *mut c_void,
    id: u64,
    data: *const u8,
    length: usize,
    binary: *const u8,
    binary_length: usize,
    callback: Callback,
    context: *mut c_void,
) -> *mut c_void {
    let completion = Completion {
        callback,
        context,
        delivered: false,
    };
    catch_unwind(AssertUnwindSafe(|| {
        let operation = Arc::new(Operation::new());
        let command = serde_json::from_slice::<Box<Command>>(unsafe {
            std::slice::from_raw_parts(data, length)
        });
        match command {
            Ok(mut command) => {
                if let Command::Execute(request) = command.as_mut() {
                    request.body = if binary_length == 0 {
                        Vec::new()
                    } else {
                        unsafe { std::slice::from_raw_parts(binary, binary_length) }.to_vec()
                    };
                }
                let endpoint = unsafe { &*runtime.cast::<Endpoint>() };
                if let Err(error) = endpoint.sender.send(Message::Execute {
                    id,
                    command,
                    operation: operation.clone(),
                    completion,
                }) && let Message::Execute { completion, .. } = error.0
                {
                    completion.finish(Err(closed()));
                }
            }
            Err(error) => {
                completion.finish(Err(Error::new(ErrorKind::InvalidInput, error.to_string())))
            }
        }
        Arc::into_raw(operation).cast_mut().cast()
    }))
    .unwrap_or(std::ptr::null_mut())
}
/// # Safety
/// `operation` must be null or a live operation returned by `moli_sdk_submit`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moli_sdk_cancel(operation: *mut c_void) {
    if !operation.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            unsafe { &*operation.cast::<Operation>() }.cancel()
        }));
    }
}
/// # Safety
/// `operation` must be null or an unreleased operation; no calls may use it afterward.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moli_sdk_release_operation(operation: *mut c_void) {
    if !operation.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            drop(unsafe { Arc::from_raw(operation.cast::<Operation>()) })
        }));
    }
}
/// # Safety
/// `runtime` must be a live endpoint. Resource IDs belong to that endpoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moli_sdk_release_resource(runtime: *mut c_void, id: u64) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _ = unsafe { &*runtime.cast::<Endpoint>() }
            .sender
            .send(Message::Release(id));
    }));
}
/// # Safety
/// `runtime` must be an unreleased endpoint with no concurrent or future users.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moli_sdk_release_runtime(runtime: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        drop(unsafe { Box::from_raw(runtime.cast::<Endpoint>()) })
    }));
}
