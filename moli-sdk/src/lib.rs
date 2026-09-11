//! 预编译 Moli SDK。浏览器、网络及 Cookie 实现运行在库自己的所有者线程。
//!
//! 丢弃操作 Future 请求取消；`close().await` 等待所属资源回收，Drop 只安排清理。
//! 子资源持有其父资源：丢弃 Session 不会关闭仍在使用的 Browser、Transport 或 CookieStore，
//! 丢弃 Browser 不会关闭仍在使用的 Page，ResponseBody 同样保持 Transport 存活。
//! 最后一个资源及其子资源释放后才安排 Drop 清理；所有权只指向父资源，不形成循环。
//! 显式 `close().await` 则立即取消该资源及全部子资源，无论是否还有其他克隆句柄。
//! 丢弃正在等待的正文读取会关闭该正文，不能在可能已部分消费后继续读取。
//! 所有方法不要求宿主使用 Tokio；浏览器指纹在进程首次初始化后固定。
mod bridge;
use bridge::{Handle, Runtime};
use moli_sdk_types::wire::Command;
pub use moli_sdk_types::{
    BrowserConfig, ConnectionConfig, Cookie, CookieContext, CookieWriteResult, Document, Error,
    ErrorKind, EvaluateOptions, ExecutionContext, Fingerprint, FingerprintOverrides, Headers,
    HttpVersion, LayoutMetrics, NavigationOptions, PageState, Request, ResourceLoading,
    ResponseMetadata, Result, SessionConfig, TransportConfig, WaitUntil,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct Session {
    handle: Arc<Handle>,
}
impl Session {
    pub async fn new(config: SessionConfig) -> Result<Self> {
        let handle = Handle::new(Runtime::open()?, 0);
        handle.invoke(Command::Initialize(config)).await?;
        Ok(Self { handle })
    }
    pub async fn browser(&self, config: BrowserConfig) -> Result<Browser> {
        Ok(Browser {
            handle: self
                .handle
                .invoke(Command::Browser(config))
                .await?
                .take_resource()?,
        })
    }
    /// 仅创建传输连接池，不创建浏览器或 V8 上下文。
    pub async fn transport(&self, config: TransportConfig) -> Result<Transport> {
        Ok(Transport {
            handle: self
                .handle
                .invoke(Command::Transport(config))
                .await?
                .take_resource()?,
        })
    }
    pub async fn cookies(&self) -> Result<CookieStore> {
        Ok(CookieStore {
            handle: self
                .handle
                .invoke(Command::Cookies)
                .await?
                .take_resource()?,
        })
    }
    pub async fn close(&self) -> Result<()> {
        self.handle.close().await
    }
}
#[derive(Clone)]
pub struct Browser {
    handle: Arc<Handle>,
}
impl Browser {
    pub async fn fetch(&self, url: impl Into<String>, options: NavigationOptions) -> Result<Page> {
        Ok(Page {
            handle: self
                .handle
                .invoke(Command::Fetch {
                    url: url.into(),
                    options,
                })
                .await?
                .take_resource()?,
        })
    }
    pub async fn close(&self) -> Result<()> {
        self.handle.close().await
    }
}
#[derive(Clone)]
pub struct Page {
    handle: Arc<Handle>,
}
impl Page {
    /// 结果是现有求值协议的 JSON（包含 value/type 等字段）；JS 异常返回 JavaScript 错误。
    pub async fn evaluate(
        &self,
        expression: impl Into<String>,
        options: EvaluateOptions,
    ) -> Result<serde_json::Value> {
        self.handle
            .invoke(Command::Evaluate {
                expression: expression.into(),
                options,
            })
            .await?
            .decode()
    }
    pub async fn create_isolated_world(&self, name: impl Into<String>) -> Result<ExecutionContext> {
        self.handle
            .invoke(Command::IsolatedWorld { name: name.into() })
            .await?
            .decode()
    }
    /// 不自动跟随导航；可用来判定旧执行上下文和待处理的 JS 导航。
    pub async fn state(&self, context: Option<ExecutionContext>) -> Result<PageState> {
        self.handle
            .invoke(Command::State { context })
            .await?
            .decode()
    }
    /// 跟随已排队的 JS 导航，再通过原生序列化获取文档和最终 URL。
    pub async fn document(&self) -> Result<Document> {
        self.handle.invoke(Command::Document).await?.decode()
    }
    pub async fn layout_metrics(&self) -> Result<LayoutMetrics> {
        self.handle.invoke(Command::Layout).await?.decode()
    }
    /// 真实布局及 CPU 绘制的视口 PNG；需要 BrowserConfig::real_layout。
    pub async fn screenshot_png(&self) -> Result<Vec<u8>> {
        Ok(self.handle.invoke(Command::Screenshot).await?.binary)
    }
    pub async fn close(&self) -> Result<()> {
        self.handle.close().await
    }
}
#[derive(Clone)]
pub struct Transport {
    handle: Arc<Handle>,
}
impl Transport {
    /// 单跳请求，在最终响应头到达时返回，不缓冲完整正文，也不处理业务重定向。
    pub async fn execute(&self, request: Request) -> Result<Response> {
        let mut reply = self.handle.invoke(Command::Execute(request)).await?;
        Ok(Response {
            metadata: reply.decode()?,
            body: ResponseBody {
                handle: reply.take_resource()?,
            },
        })
    }
    pub async fn close(&self) -> Result<()> {
        self.handle.close().await
    }
}
pub struct Response {
    pub metadata: ResponseMetadata,
    pub body: ResponseBody,
}
pub struct ResponseBody {
    handle: Arc<Handle>,
}
impl ResponseBody {
    /// 返回原始二进制响应块。None 表示 EOF；丢弃未读完正文取消该请求。
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>> {
        struct ReadLease(Option<Arc<Handle>>);
        impl Drop for ReadLease {
            fn drop(&mut self) {
                if let Some(handle) = &self.0 {
                    handle.cancel();
                }
            }
        }
        let mut lease = ReadLease(Some(self.handle.clone()));
        let mut reply = self.handle.invoke(Command::Chunk).await?;
        let has_chunk = reply.decode::<bool>()?;
        lease.0 = None;
        if has_chunk {
            Ok(Some(reply.binary))
        } else {
            Ok(None)
        }
    }
    pub async fn close(&self) -> Result<()> {
        self.handle.close().await
    }
}
#[derive(Clone)]
pub struct CookieStore {
    handle: Arc<Handle>,
}
impl CookieStore {
    /// 按现有 Moli Cookie 规则选取，保留顺序和同名 Cookie。
    pub async fn select(
        &self,
        url: impl Into<String>,
        context: CookieContext,
    ) -> Result<Vec<Cookie>> {
        self.handle
            .invoke(Command::CookieSelect {
                url: url.into(),
                context,
            })
            .await?
            .decode()
    }
    pub async fn store_response(
        &self,
        url: impl Into<String>,
        headers: Headers,
        context: CookieContext,
    ) -> Result<Vec<CookieWriteResult>> {
        self.handle
            .invoke(Command::CookieStore {
                url: url.into(),
                headers,
                context,
            })
            .await?
            .decode()
    }
    pub async fn close(&self) -> Result<()> {
        self.handle.close().await
    }
}
