use super::{Output, State};
use moli_cookie_jar::{BrowserCookieStore, NetworkCookieRequestContext};
use moli_core::{
    page::Page,
    runtime::{Browser, RenderedDomWaitUntil},
};
use moli_sdk_types::{wire::Command, *};
use moli_stealth_net::{ResponseBody, Transport, TransportFingerprint};
use tokio::sync::Mutex;

pub(super) enum Resource {
    Session,
    Browser(Box<Browser>),
    Page(Mutex<Page>),
    Transport(Transport),
    Body(Mutex<ResponseBody>),
    Cookies(Mutex<BrowserCookieStore>),
}
impl Resource {
    pub async fn close(self) -> Result<()> {
        match self {
            Self::Page(page) => page
                .into_inner()
                .close_async()
                .await
                .map_err(|e| error(ErrorKind::Cleanup, e)),
            Self::Browser(browser) => (*browser).close().map_err(|e| error(ErrorKind::Cleanup, e)),
            Self::Transport(transport) => transport
                .close()
                .await
                .map_err(|e| error(ErrorKind::Cleanup, e)),
            _ => Ok(()),
        }
    }
}
fn error(kind: ErrorKind, error: impl std::fmt::Display) -> Error {
    Error::new(kind, error.to_string())
}
fn browser_error(error: impl std::fmt::Display) -> Error {
    self::error(ErrorKind::Browser, error)
}
fn transport_error(error: impl std::fmt::Display) -> Error {
    self::error(ErrorKind::Transport, error)
}
fn input_error(error: impl std::fmt::Display) -> Error {
    self::error(ErrorKind::InvalidInput, error)
}
fn cookie_context(url: &url::Url, value: CookieContext) -> Result<NetworkCookieRequestContext> {
    let mut context = if value.top_level_navigation {
        NetworkCookieRequestContext::top_level_navigation(&value.method)
    } else {
        NetworkCookieRequestContext::subresource(&value.method)
    };
    if let Some(initiator) = value.initiator_url {
        context =
            context.with_initiator_url(url, &url::Url::parse(&initiator).map_err(input_error)?);
    }
    if let Some(site) = value.site_for_cookies_url {
        context =
            context.with_site_for_cookies_url(url, &url::Url::parse(&site).map_err(input_error)?);
    }
    if let Some(top) = value.top_frame_origin_url {
        context =
            context.with_top_frame_origin_url(url, &url::Url::parse(&top).map_err(input_error)?);
    }
    if value.cross_site {
        context = context.with_cross_site_context();
    }
    Ok(context)
}
fn wait_until(value: WaitUntil) -> RenderedDomWaitUntil {
    match value {
        WaitUntil::DomContentLoaded => RenderedDomWaitUntil::DomContentLoaded,
        WaitUntil::Load => RenderedDomWaitUntil::Load,
        WaitUntil::NetworkIdle => RenderedDomWaitUntil::NetworkIdle,
        WaitUntil::DomStable => RenderedDomWaitUntil::DomStable,
        WaitUntil::Done => RenderedDomWaitUntil::Done,
    }
}
fn browser_config(value: BrowserConfig) -> Result<moli_core::runtime::BrowserConfig> {
    use moli_page_types::{LayoutPolicy, OptionalResourceFetchMask as Mask};
    let mut config = moli_core::runtime::BrowserConfig::default();
    config.set_subframe_loading_enabled(value.subframes);
    config.set_layout_policy(if value.real_layout {
        LayoutPolicy::OnDemand
    } else {
        LayoutPolicy::Mock
    });
    let mut mask = Mask::NONE;
    for (enabled, flag) in [
        (value.resources.images, Mask::IMAGE),
        (value.resources.fonts, Mask::FONT),
        (value.resources.audio, Mask::AUDIO),
        (value.resources.video, Mask::VIDEO),
        (value.resources.media, Mask::MEDIA),
        (value.resources.text_tracks, Mask::TEXT_TRACK),
    ] {
        mask.set(flag, enabled);
    }
    config.set_optional_resource_fetch_mask(mask);
    config.set_profile_dir(value.profile_directory);
    for script in value.document_start_scripts {
        config.add_document_start_script(script);
    }
    let fetch = config.fetch_mut();
    if let Some(user_agent) = value.user_agent {
        fetch.set_user_agent(user_agent);
    }
    fetch.set_default_request_headers(value.default_headers);
    fetch.set_http_proxy(value.proxy);
    fetch.set_http_no_proxy(value.no_proxy);
    fetch.set_network_blocking(
        value.block_private_networks,
        value
            .blocked_cidrs
            .iter()
            .map(|cidr| cidr.parse().map_err(input_error))
            .collect::<Result<Vec<_>>>()?,
    );
    fetch.set_obey_robots(value.obey_robots);
    Ok(config)
}

pub(super) async fn execute(
    state: &State,
    id: u64,
    resource: &Resource,
    command: Command,
) -> Result<Output> {
    if !state.initialized.get() && !matches!(command, Command::Initialize(_)) {
        return Err(Error::new(
            ErrorKind::Initialization,
            "必须先初始化 SDK Session",
        ));
    }
    match (resource, command) {
        (Resource::Session, Command::Initialize(config)) => {
            use moli_stealth_net::H2Setting;
            let mut fingerprint = match config.fingerprint {
                Fingerprint::Chrome => TransportFingerprint::chrome(),
                Fingerprint::Ordinary => TransportFingerprint::default(),
            };
            let overrides = config.fingerprint_overrides;
            if let Some(value) = overrides.tls_cipher_list {
                fingerprint.tls.cipher_list = Some(value);
            }
            if let Some(value) = overrides.tls_curves {
                fingerprint.tls.curves_list = Some(value);
            }
            if let Some(value) = overrides.tls_signature_algorithms {
                fingerprint.tls.signature_algorithms = Some(value);
            }
            for (setting, value) in [
                (H2Setting::HeaderTableSize, overrides.h2_header_table_size),
                (
                    H2Setting::EnablePush,
                    overrides.h2_enable_push.map(u32::from),
                ),
                (
                    H2Setting::MaxConcurrentStreams,
                    overrides.h2_max_concurrent_streams,
                ),
                (
                    H2Setting::InitialWindowSize,
                    overrides.h2_initial_window_size,
                ),
                (H2Setting::MaxFrameSize, overrides.h2_max_frame_size),
                (
                    H2Setting::MaxHeaderListSize,
                    overrides.h2_max_header_list_size,
                ),
            ] {
                if let Some(value) = value {
                    fingerprint.set_h2_setting(setting, Some(value));
                }
            }
            if let Some(value) = overrides.h2_connection_window_size {
                fingerprint.h2.connection_window_size = Some(value);
            }
            moli_stealth_net::initialize_process_fingerprint(fingerprint)
                .map_err(|e| error(ErrorKind::Initialization, e))?;
            state.initialized.set(true);
            Output::value(())
        }
        (Resource::Session, Command::Browser(config)) => {
            let browser = Browser::new(browser_config(config)?)
                .map_err(|e| error(ErrorKind::Initialization, e))?;
            Output::resource(state.insert(id, Resource::Browser(Box::new(browser)))?, ())
        }
        (Resource::Session, Command::Transport(config)) => {
            let transport = Transport::new(moli_stealth_net::TransportConfig {
                fingerprint: moli_stealth_net::process_fingerprint().clone(),
                tls_verify: config.tls_verify,
                max_connections: config.max_connections,
                max_host_connections: config.max_host_connections,
                max_h2_streams: config.max_h2_streams,
            })
            .map_err(transport_error)?;
            Output::resource(state.insert(id, Resource::Transport(transport))?, ())
        }
        (Resource::Session, Command::Cookies) => Output::resource(
            state.insert(
                id,
                Resource::Cookies(Mutex::new(BrowserCookieStore::default())),
            )?,
            (),
        ),
        (Resource::Browser(browser), Command::Fetch { url, options }) => {
            let page = if options.allow_http_errors {
                browser
                    .fetch_allow_http_error_with_wait_until(
                        &url,
                        wait_until(options.wait_until),
                        options.timeout,
                    )
                    .await
            } else {
                browser
                    .fetch_with_wait_until(&url, wait_until(options.wait_until), options.timeout)
                    .await
            }
            .map_err(browser_error)?;
            Output::resource(state.insert(id, Resource::Page(Mutex::new(page)))?, ())
        }
        (
            Resource::Page(page),
            Command::Evaluate {
                expression,
                options,
            },
        ) => {
            let mut page = page.lock().await;
            if let Some(context) = options.context
                && !page
                    .has_isolated_execution_context_id_async(context.0)
                    .await
                    .map_err(browser_error)?
            {
                return Err(Error::new(
                    ErrorKind::ContextInvalidated,
                    "执行上下文已因导航失效",
                ));
            }
            let evaluated = match (options.context, options.follow_navigation) {
                (Some(context), true) => page.evaluate_runtime_expression_in_execution_context_with_await_async(context.0, &expression, options.await_promise).await,
                (Some(context), false) => page.evaluate_runtime_expression_in_execution_context_without_navigation_follow_with_await_async(context.0, &expression, options.await_promise).await,
                (None, true) => page.evaluate_runtime_expression_with_await_async(&expression, options.await_promise).await,
                (None, false) => page.evaluate_runtime_expression_without_navigation_follow_with_await_async(&expression, options.await_promise).await,
            };
            if let Some(context) = options.context
                && !page
                    .has_isolated_execution_context_id_async(context.0)
                    .await
                    .map_err(browser_error)?
            {
                return Err(Error::new(
                    ErrorKind::ContextInvalidated,
                    "求值期间执行上下文已失效",
                ));
            }
            let evaluated = evaluated.map_err(|e| error(ErrorKind::JavaScript, e))?;
            if let Some(exception) = evaluated.get("exception") {
                return Err(Error::new(ErrorKind::JavaScript, exception.to_string()));
            }
            Output::value(evaluated)
        }
        (Resource::Page(page), Command::IsolatedWorld { name }) => Output::value(ExecutionContext(
            page.lock()
                .await
                .create_isolated_world_async(&name, false)
                .await
                .map_err(browser_error)?,
        )),
        (Resource::Page(page), Command::State { context }) => {
            let mut page = page.lock().await;
            let pending_navigation = page
                .has_pending_location_navigation()
                .await
                .map_err(browser_error)?;
            let context_valid = match context {
                Some(context) => Some(
                    page.has_isolated_execution_context_id_async(context.0)
                        .await
                        .map_err(browser_error)?,
                ),
                None => None,
            };
            Output::value(PageState {
                url: page.final_url().to_string(),
                pending_navigation,
                context_valid,
            })
        }
        (Resource::Page(page), Command::Document) => {
            let mut page = page.lock().await;
            page.evaluate_runtime_expression_async("void 0")
                .await
                .map_err(browser_error)?;
            let html = page.serialize_html_async().await.map_err(browser_error)?;
            Output::value(Document {
                url: page.final_url().to_string(),
                html,
            })
        }
        (Resource::Page(page), Command::Layout) => {
            let mut page = page.lock().await;
            let completion = page
                .start_layout_metrics()
                .map_err(browser_error)?
                .wait()
                .await
                .map_err(browser_error)?;
            let metrics = page
                .finish_layout_metrics(completion)
                .map_err(browser_error)?;
            Output::value(LayoutMetrics {
                viewport_width: metrics.viewport_width,
                viewport_height: metrics.viewport_height,
                page_x: metrics.page_x,
                page_y: metrics.page_y,
                content_width: metrics.content_width,
                content_height: metrics.content_height,
                device_pixel_ratio: metrics.device_pixel_ratio,
            })
        }
        (Resource::Page(page), Command::Screenshot) => {
            let mut page = page.lock().await;
            let completion = page
                .start_capture_screenshot()
                .map_err(browser_error)?
                .wait()
                .await
                .map_err(browser_error)?;
            match page
                .finish_capture_screenshot(completion)
                .map_err(browser_error)?
            {
                moli_core::page::RendererCaptureScreenshotReply::Captured(image) => {
                    Output::binary(true, bytes::Bytes::from_owner(image.bytes))
                }
                moli_core::page::RendererCaptureScreenshotReply::LayoutDisabled => Err(Error::new(
                    ErrorKind::InvalidInput,
                    "截图需要 real_layout 配置",
                )),
                moli_core::page::RendererCaptureScreenshotReply::NoDocument => {
                    Err(Error::new(ErrorKind::Browser, "页面没有可绘制文档"))
                }
            }
        }
        (Resource::Transport(transport), Command::Execute(request)) => {
            let mut outgoing = moli_stealth_net::TransportRequest::new(
                url::Url::parse(&request.url).map_err(input_error)?,
                request.method,
            );
            outgoing.headers = request.headers;
            outgoing.body = if request.body.is_empty() {
                None
            } else {
                Some(request.body)
            };
            outgoing.http1_only = request.http1_only;
            outgoing.connection.proxy = request.connection.proxy;
            outgoing.connection.no_proxy = request.connection.no_proxy;
            outgoing.connection.connect_timeout = request.connection.connect_timeout;
            outgoing.connection.resolved_addresses = request.connection.resolved_addresses;
            let response = transport.execute(outgoing).await.map_err(transport_error)?;
            let version = match response.version {
                http::Version::HTTP_09 => HttpVersion::Http09,
                http::Version::HTTP_10 => HttpVersion::Http10,
                http::Version::HTTP_11 => HttpVersion::Http11,
                http::Version::HTTP_2 => HttpVersion::Http2,
                http::Version::HTTP_3 => HttpVersion::Http3,
                other => return Err(transport_error(format!("不支持的 HTTP 版本: {other:?}"))),
            };
            Output::resource(
                state.insert(id, Resource::Body(Mutex::new(response.body)))?,
                ResponseMetadata {
                    status: response.status,
                    version,
                    headers: response.headers,
                },
            )
        }
        (Resource::Body(body), Command::Chunk) => {
            match body.lock().await.chunk().await.map_err(transport_error)? {
                Some(chunk) => Output::binary(true, chunk),
                None => Output::binary(false, bytes::Bytes::new()),
            }
        }
        (Resource::Cookies(store), Command::CookieSelect { url, context }) => {
            let url = url::Url::parse(&url).map_err(input_error)?;
            let report = store
                .lock()
                .await
                .cookie_access_report_for_request(&url, cookie_context(&url, context)?);
            Output::value(
                report
                    .included_cookies
                    .into_iter()
                    .map(|entry| Cookie {
                        name: entry.cookie.name,
                        value: entry.cookie.value,
                    })
                    .collect::<Vec<_>>(),
            )
        }
        (
            Resource::Cookies(store),
            Command::CookieStore {
                url,
                headers,
                context,
            },
        ) => {
            let url = url::Url::parse(&url).map_err(input_error)?;
            let reports = store
                .lock()
                .await
                .store_response_headers_with_context_reports(
                    &url,
                    &headers,
                    &cookie_context(&url, context)?,
                );
            Output::value(
                reports
                    .into_iter()
                    .map(|report| CookieWriteResult {
                        accepted: report.is_accepted(),
                    })
                    .collect::<Vec<_>>(),
            )
        }
        _ => Err(Error::new(ErrorKind::InvalidInput, "操作与资源类型不匹配")),
    }
}
