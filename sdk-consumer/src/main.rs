mod browser;
mod fixture;
#[cfg(target_os = "linux")]
mod fonts;
mod network;

use moli_sdk::{
    BrowserConfig, CookieContext, EvaluateOptions, HttpVersion, NavigationOptions, Request,
    ResourceLoading, Session, SessionConfig, TransportConfig,
};
use std::time::Duration;

async fn binary_http_and_cookie() {
    let fixture = fixture::Fixture::start().await;
    let session = Session::new(SessionConfig::default()).await.unwrap();
    let transport = session.transport(TransportConfig::default()).await.unwrap();
    let mut request = Request::get(fixture.url("/echo"));
    request.method = "POST".into();
    request.body = vec![0, 255, 1, 0, 127];
    let mut response = transport.execute(request).await.unwrap();
    assert_eq!(response.metadata.status, 200);
    assert_eq!(response.metadata.version, HttpVersion::Http11);
    assert_eq!(
        response
            .metadata
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>(),
        [
            "first=one; Path=/; SameSite=Lax",
            "second=two; Path=/; SameSite=Lax"
        ]
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    assert_eq!(bytes, [0, 255, 1, 0, 127]);
    let connection = response
        .metadata
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("x-fixture-connection"))
        .unwrap()
        .1
        .clone();
    let cookies = session.cookies().await.unwrap();
    cookies
        .store_response(
            fixture.url("/echo"),
            response.metadata.headers,
            CookieContext::top_level_navigation("GET"),
        )
        .await
        .unwrap();
    let selected = cookies
        .select(
            fixture.url("/echo"),
            CookieContext::top_level_navigation("GET"),
        )
        .await
        .unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|cookie| (cookie.name.as_str(), cookie.value.as_str()))
            .collect::<Vec<_>>(),
        [("first", "one"), ("second", "two")]
    );
    let cross_site = CookieContext {
        cross_site: true,
        ..CookieContext::subresource("GET")
    };
    assert!(
        cookies
            .select(fixture.url("/echo"), cross_site)
            .await
            .unwrap()
            .is_empty(),
        "SameSite=Lax cookies leaked into a cross-site subresource"
    );
    assert!(
        cookies
            .select(
                "http://different.invalid/",
                CookieContext::top_level_navigation("GET")
            )
            .await
            .unwrap()
            .is_empty(),
        "host-only cookies leaked into a different host"
    );
    let isolated = session.cookies().await.unwrap();
    assert!(
        isolated
            .select(
                fixture.url("/echo"),
                CookieContext::top_level_navigation("GET")
            )
            .await
            .unwrap()
            .is_empty(),
        "independent Cookie stores shared state"
    );
    isolated.close().await.unwrap();
    let mut following = Request::get(fixture.url("/echo"));
    following.headers.push((
        "cookie".into(),
        selected
            .iter()
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect::<Vec<_>>()
            .join("; "),
    ));
    let mut response = transport.execute(following).await.unwrap();
    assert_eq!(
        response
            .metadata
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-fixture-connection"))
            .unwrap()
            .1,
        connection,
        "fully consumed response did not permit connection reuse"
    );
    while response.body.chunk().await.unwrap().is_some() {}
    assert_eq!(
        fixture.observations.requests.lock().await.last().unwrap().1["cookie"],
        "first=one; second=two"
    );
    cookies.close().await.unwrap();
    transport.close().await.unwrap();
    session.close().await.unwrap();
}

async fn benchmark() {
    binary_http_and_cookie().await;
    let fixture = fixture::Fixture::start().await;
    let session = Session::new(SessionConfig::default()).await.unwrap();
    let browser = session
        .browser(BrowserConfig {
            block_private_networks: false,
            obey_robots: false,
            resources: ResourceLoading::all(),
            ..Default::default()
        })
        .await
        .unwrap();
    let page = browser
        .fetch(
            fixture.url("/page"),
            NavigationOptions {
                timeout: Duration::from_secs(15),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let context = page.create_isolated_world("sdk-benchmark").await.unwrap();
    let result = page
        .evaluate(
            "document.title",
            EvaluateOptions {
                context: Some(context),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(result["value"], "SDK document");
    assert!(
        page.document()
            .await
            .unwrap()
            .html
            .contains("rendered document")
    );
    page.close().await.unwrap();
    browser.close().await.unwrap();
    session.close().await.unwrap();
    println!("browser-http-cookie benchmark workload passed");
}

#[tokio::main]
async fn main() {
    let command = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    match command.as_str() {
        "reject-runtime-abi" => {
            let error = Session::new(SessionConfig::default())
                .await
                .err()
                .expect("runtime accepted an incompatible implementation behind a valid manifest");
            assert_eq!(error.kind, moli_sdk::ErrorKind::Abi);
            println!("runtime rejected incompatible ABI before opening a Session");
        }
        "all" => {
            benchmark().await;
            network::streaming_and_cancel().await;
            network::pinned_single_hop_and_proxy().await;
            network::tls_verification().await;
            network::connection_timeout_releases_handshake().await;
            network::h2_close_reclaims_connection().await;
            network::drop_final_handle().await;
            browser::documents_and_contexts().await;
            browser::dynamic_resources_and_cookie().await;
            browser::close_cancels_pending_and_retains_children().await;
            browser::close_reports_storage_failure().await;
            #[cfg(target_os = "linux")]
            fonts::verify().await;
        }
        "benchmark" => benchmark().await,
        "browser" => {
            browser::documents_and_contexts().await;
            browser::dynamic_resources_and_cookie().await;
            browser::close_cancels_pending_and_retains_children().await;
            browser::close_reports_storage_failure().await;
        }
        "http" => {
            binary_http_and_cookie().await;
            network::streaming_and_cancel().await;
            network::pinned_single_hop_and_proxy().await;
            network::tls_verification().await;
            network::connection_timeout_releases_handshake().await;
            network::h2_close_reclaims_connection().await;
            network::drop_final_handle().await;
        }
        #[cfg(target_os = "linux")]
        "fonts" => fonts::verify().await,
        _ => panic!("unknown consumer scenario: {command}"),
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn binary_request_repeated_headers_and_cookie_affect_next_request() {
        super::binary_http_and_cookie().await;
    }
    #[tokio::test]
    async fn body_streams_before_eof_and_cancel_releases_request() {
        super::network::streaming_and_cancel().await;
    }
    #[tokio::test]
    async fn connection_pinning_single_hop_and_explicit_proxy_bypass() {
        super::network::pinned_single_hop_and_proxy().await;
    }
    #[tokio::test]
    async fn tls_is_verified_unless_explicitly_disabled_for_fixture() {
        super::network::tls_verification().await;
    }
    #[tokio::test]
    async fn connection_timeout_ends_stalled_tls_and_transport_remains_usable() {
        super::network::connection_timeout_releases_handshake().await;
    }
    #[tokio::test]
    async fn h2_close_ends_owned_streams_without_stopping_session() {
        super::network::h2_close_reclaims_connection().await;
    }
    #[tokio::test]
    async fn last_child_drop_cleans_up_without_blocking_host() {
        super::network::drop_final_handle().await;
    }
    #[tokio::test]
    async fn isolated_worlds_follow_navigation_and_reject_stale_contexts() {
        super::browser::documents_and_contexts().await;
    }
    #[tokio::test]
    async fn browser_resource_policies_and_dynamic_cookie_affect_real_requests() {
        super::browser::dynamic_resources_and_cookie().await;
    }
    #[tokio::test]
    async fn page_survives_temporary_parents_and_close_cancels_pending_promise() {
        super::browser::close_cancels_pending_and_retains_children().await;
    }
    #[tokio::test]
    async fn explicit_browser_close_reports_profile_flush_failure() {
        super::browser::close_reports_storage_failure().await;
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn installed_fonts_render_latin_cjk_and_fallback() {
        super::fonts::verify().await;
    }
}
