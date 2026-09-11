use crate::{Error, ErrorKind, Result};
use futures_channel::oneshot;
use moli_sdk_types::wire::{Command, Reply};
use serde::de::DeserializeOwned;
use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

type Callback = unsafe extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize);
unsafe extern "C" {
    fn moli_sdk_abi(data: *mut u8, capacity: usize) -> usize;
    fn moli_sdk_open() -> *mut c_void;
    fn moli_sdk_submit(
        runtime: *mut c_void,
        resource: u64,
        data: *const u8,
        length: usize,
        binary: *const u8,
        binary_length: usize,
        callback: Callback,
        context: *mut c_void,
    ) -> *mut c_void;
    fn moli_sdk_cancel(operation: *mut c_void);
    fn moli_sdk_release_operation(operation: *mut c_void);
    fn moli_sdk_release_resource(runtime: *mut c_void, resource: u64);
    fn moli_sdk_release_runtime(runtime: *mut c_void);
}

pub(crate) struct Runtime(*mut c_void);
// The pointer names only an implementation-owned, thread-safe command sender.
unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}
impl Runtime {
    pub fn open() -> Result<Arc<Self>> {
        let expected = include_str!("../abi.txt").trim().as_bytes();
        let mut actual = vec![0; expected.len()];
        let length = unsafe { moli_sdk_abi(actual.as_mut_ptr(), actual.len()) };
        if length != expected.len() || actual != expected {
            return Err(Error::new(ErrorKind::Abi, "SDK 与静态实现 ABI 身份不匹配"));
        }
        let pointer = unsafe { moli_sdk_open() };
        if pointer.is_null() {
            return Err(Error::new(
                ErrorKind::Initialization,
                "无法创建 Moli 所有者线程",
            ));
        }
        Ok(Arc::new(Self(pointer)))
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe { moli_sdk_release_runtime(self.0) };
    }
}

pub(crate) struct Handle {
    pub runtime: Arc<Runtime>,
    pub id: u64,
    closed: AtomicBool,
    _parent: Option<Arc<Handle>>,
}
impl Handle {
    pub fn new(runtime: Arc<Runtime>, id: u64) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            id,
            closed: AtomicBool::new(false),
            _parent: None,
        })
    }
    fn child(parent: Arc<Handle>, id: u64) -> Arc<Self> {
        Arc::new(Self {
            runtime: parent.runtime.clone(),
            id,
            closed: AtomicBool::new(false),
            _parent: Some(parent),
        })
    }
    pub async fn invoke(self: &Arc<Self>, command: Command) -> Result<Completed> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::new(ErrorKind::Closed, "资源已关闭"));
        }
        call(self.clone(), command).await
    }
    pub fn cancel(&self) {
        self.closed.store(true, Ordering::Release);
        unsafe { moli_sdk_release_resource(self.runtime.0, self.id) };
    }
    pub async fn close(self: &Arc<Self>) -> Result<()> {
        // Repeated close still asks the owner for acknowledgement, including when a
        // previous close future was dropped before cleanup finished.
        self.closed.store(true, Ordering::Release);
        call(self.clone(), Command::Close).await?.decode()
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { moli_sdk_release_resource(self.runtime.0, self.id) };
    }
}

pub(crate) struct Completed {
    reply: Reply,
    pub binary: Vec<u8>,
    resource: Option<Arc<Handle>>,
}
impl Completed {
    pub fn decode<T: DeserializeOwned>(&mut self) -> Result<T> {
        serde_json::from_value(std::mem::take(&mut self.reply.value))
            .map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))
    }
    pub fn take_resource(&mut self) -> Result<Arc<Handle>> {
        self.resource
            .take()
            .ok_or_else(|| Error::new(ErrorKind::Internal, "实现未返回资源句柄"))
    }
}
struct Notification {
    sender: oneshot::Sender<Result<Completed>>,
    parent: Arc<Handle>,
}
unsafe extern "C" fn complete(
    context: *mut c_void,
    data: *const u8,
    length: usize,
    binary: *const u8,
    binary_length: usize,
) {
    // Exactly one callback owns this Box, even after the receiver disappears.
    // Payload memory remains implementation-owned and valid only during this call.
    let notification = unsafe { Box::from_raw(context.cast::<Notification>()) };
    let result = serde_json::from_slice::<Result<Reply>>(unsafe {
        std::slice::from_raw_parts(data, length)
    })
    .map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))
    .and_then(|r| r)
    .map(|reply| {
        let resource = reply
            .resource
            .map(|id| Handle::child(notification.parent.clone(), id));
        let binary = if binary_length == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(binary, binary_length) }.to_vec()
        };
        Completed {
            reply,
            binary,
            resource,
        }
    });
    // Failed send drops owned resource leases, so completed-but-unobserved
    // browser/page/body creation cannot strand implementation resources.
    let _ = notification.sender.send(result);
}
struct Operation(*mut c_void);
unsafe impl Send for Operation {}
impl Drop for Operation {
    fn drop(&mut self) {
        unsafe {
            moli_sdk_cancel(self.0);
            moli_sdk_release_operation(self.0);
        }
    }
}
async fn call(parent: Arc<Handle>, mut command: Command) -> Result<Completed> {
    let binary = match &mut command {
        Command::Execute(request) => std::mem::take(&mut request.body),
        _ => Vec::new(),
    };
    let data = serde_json::to_vec(&command)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, e.to_string()))?;
    let (sender, receiver) = oneshot::channel();
    let context = Box::into_raw(Box::new(Notification {
        sender,
        parent: parent.clone(),
    }))
    .cast();
    let operation = unsafe {
        moli_sdk_submit(
            parent.runtime.0,
            parent.id,
            data.as_ptr(),
            data.len(),
            binary.as_ptr(),
            binary.len(),
            complete,
            context,
        )
    };
    drop(data);
    drop(binary);
    drop(command);
    let _operation = Operation(operation);
    receiver
        .await
        .map_err(|_| Error::new(ErrorKind::Internal, "实现未完成操作通知"))?
}
