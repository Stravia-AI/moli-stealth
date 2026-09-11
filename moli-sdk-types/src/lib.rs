//! SDK 的轻量值类型；不依赖浏览器、传输实现或异步运行时。
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, time::Duration};

pub type Headers = Vec<(String, String)>;
pub type Result<T, E = Error> = std::result::Result<T, E>;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ErrorKind {
    Abi,
    Initialization,
    InvalidInput,
    Closed,
    Cancelled,
    Transport,
    JavaScript,
    ContextInvalidated,
    Browser,
    Cleanup,
    Internal,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}
impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum Fingerprint {
    Ordinary,
    #[default]
    Chrome,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionConfig {
    pub fingerprint: Fingerprint,
    pub fingerprint_overrides: FingerprintOverrides,
}
/// 仅在 Session 初始化时应用；不同配置需要新进程，不能在页面运行中切换。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FingerprintOverrides {
    pub tls_cipher_list: Option<String>,
    pub tls_curves: Option<String>,
    pub tls_signature_algorithms: Option<String>,
    pub h2_header_table_size: Option<u32>,
    pub h2_enable_push: Option<bool>,
    pub h2_max_concurrent_streams: Option<u32>,
    pub h2_initial_window_size: Option<u32>,
    pub h2_max_frame_size: Option<u32>,
    pub h2_max_header_list_size: Option<u32>,
    pub h2_connection_window_size: Option<u32>,
}
/// None 允许现有环境发现；Some("") 显式禁用代理或所有绕过。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub connect_timeout: Option<Duration>,
    pub resolved_addresses: Option<Vec<SocketAddr>>,
}
impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            proxy: Some(String::new()),
            no_proxy: Some(String::new()),
            connect_timeout: Some(Duration::from_secs(30)),
            resolved_addresses: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserConfig {
    pub subframes: bool,
    pub resources: ResourceLoading,
    pub real_layout: bool,
    pub proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub block_private_networks: bool,
    pub blocked_cidrs: Vec<String>,
    pub obey_robots: bool,
    pub document_start_scripts: Vec<String>,
    pub profile_directory: Option<std::path::PathBuf>,
    pub user_agent: Option<String>,
    pub default_headers: Headers,
}
impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            subframes: true,
            resources: ResourceLoading::default(),
            real_layout: true,
            proxy: Some(String::new()),
            no_proxy: Some(String::new()),
            block_private_networks: true,
            blocked_cidrs: Vec::new(),
            obey_robots: true,
            document_start_scripts: Vec::new(),
            profile_directory: None,
            user_agent: None,
            default_headers: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceLoading {
    pub images: bool,
    pub fonts: bool,
    pub audio: bool,
    pub video: bool,
    pub media: bool,
    pub text_tracks: bool,
}
impl ResourceLoading {
    pub fn all() -> Self {
        Self {
            images: true,
            fonts: true,
            audio: true,
            video: true,
            media: true,
            text_tracks: true,
        }
    }
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum WaitUntil {
    #[default]
    DomContentLoaded,
    Load,
    NetworkIdle,
    DomStable,
    Done,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavigationOptions {
    pub wait_until: WaitUntil,
    pub timeout: Duration,
    pub allow_http_errors: bool,
}
impl Default for NavigationOptions {
    fn default() -> Self {
        Self {
            wait_until: WaitUntil::DomContentLoaded,
            timeout: Duration::from_secs(30),
            allow_http_errors: true,
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionContext(pub i64);
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateOptions {
    pub context: Option<ExecutionContext>,
    pub await_promise: bool,
    pub follow_navigation: bool,
}
impl Default for EvaluateOptions {
    fn default() -> Self {
        Self {
            context: None,
            await_promise: false,
            follow_navigation: true,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub url: String,
    pub html: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageState {
    pub url: String,
    pub pending_navigation: bool,
    pub context_valid: Option<bool>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutMetrics {
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub page_x: f64,
    pub page_y: f64,
    pub content_width: f64,
    pub content_height: f64,
    pub device_pixel_ratio: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportConfig {
    pub tls_verify: bool,
    pub max_connections: Option<usize>,
    pub max_host_connections: Option<usize>,
    pub max_h2_streams: Option<usize>,
}
impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            tls_verify: true,
            max_connections: None,
            max_host_connections: None,
            max_h2_streams: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub url: String,
    pub method: String,
    pub headers: Headers,
    pub body: Vec<u8>,
    pub connection: ConnectionConfig,
    pub http1_only: bool,
}
impl Request {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: "GET".into(),
            headers: Vec::new(),
            body: Vec::new(),
            connection: ConnectionConfig::default(),
            http1_only: false,
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HttpVersion {
    Http09,
    Http10,
    Http11,
    Http2,
    Http3,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMetadata {
    pub status: u16,
    pub version: HttpVersion,
    pub headers: Headers,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookieContext {
    pub method: String,
    pub top_level_navigation: bool,
    pub initiator_url: Option<String>,
    pub site_for_cookies_url: Option<String>,
    pub top_frame_origin_url: Option<String>,
    pub cross_site: bool,
}
impl CookieContext {
    pub fn top_level_navigation(method: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            top_level_navigation: true,
            initiator_url: None,
            site_for_cookies_url: None,
            top_frame_origin_url: None,
            cross_site: false,
        }
    }
    pub fn subresource(method: impl Into<String>) -> Self {
        Self {
            top_level_navigation: false,
            ..Self::top_level_navigation(method)
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookieWriteResult {
    pub accepted: bool,
}

/// 内部序列化连接协议，不是跨版本兼容承诺。
#[doc(hidden)]
pub mod wire {
    use super::*;
    #[derive(Debug, Serialize, Deserialize)]
    pub enum Command {
        Initialize(SessionConfig),
        Browser(BrowserConfig),
        Transport(TransportConfig),
        Cookies,
        Fetch {
            url: String,
            options: NavigationOptions,
        },
        Evaluate {
            expression: String,
            options: EvaluateOptions,
        },
        IsolatedWorld {
            name: String,
        },
        State {
            context: Option<ExecutionContext>,
        },
        Document,
        Layout,
        Screenshot,
        Execute(Request),
        Chunk,
        CookieSelect {
            url: String,
            context: CookieContext,
        },
        CookieStore {
            url: String,
            headers: Headers,
            context: CookieContext,
        },
        Close,
    }
    #[derive(Debug, Serialize, Deserialize)]
    pub struct Reply {
        pub value: serde_json::Value,
        pub resource: Option<u64>,
    }
}
