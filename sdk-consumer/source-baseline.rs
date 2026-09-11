use std::time::Duration;

use moli_cookie_jar::{BrowserCookieStore, NetworkCookieRequestContext};
use moli_core::runtime::{Browser, BrowserConfig, RenderedDomWaitUntil};
use moli_stealth_net::{Transport, TransportConfig, TransportFingerprint, TransportRequest};

mod fixture;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = fixture::Fixture::start().await;
            moli_stealth_net::initialize_process_fingerprint(TransportFingerprint::chrome())
                .unwrap();
            let transport = Transport::new(TransportConfig {
                fingerprint: TransportFingerprint::chrome(),
                ..Default::default()
            })
            .unwrap();
            let url = fixture.url("/echo").parse().unwrap();
            let mut request = TransportRequest::new(url, "POST");
            request.body = Some(vec![0, 255, 1, 0, 127]);
            request.connection.proxy = Some(String::new());
            request.connection.no_proxy = Some(String::new());
            request.connection.connect_timeout = Some(Duration::from_secs(5));
            let mut response = transport.execute(request).await.unwrap();
            assert_eq!(response.status, 200);
            let mut body = Vec::new();
            while let Some(chunk) = response.body.chunk().await.unwrap() {
                body.extend_from_slice(&chunk);
            }
            assert_eq!(body, [0, 255, 1, 0, 127]);
            let connection = response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("x-fixture-connection"))
                .unwrap()
                .1
                .clone();
            let url = fixture.url("/echo").parse().unwrap();
            let context = NetworkCookieRequestContext::top_level_navigation("GET");
            let mut cookies = BrowserCookieStore::default();
            cookies.store_response_headers_with_context_reports(&url, &response.headers, &context);
            let report = cookies.cookie_access_report_for_request(&url, context);
            assert_eq!(
                report
                    .included_cookies
                    .iter()
                    .map(|entry| (entry.cookie.name.as_str(), entry.cookie.value.as_str()))
                    .collect::<Vec<_>>(),
                [("first", "one"), ("second", "two")]
            );
            assert!(
                cookies
                    .cookie_access_report_for_request(
                        &url,
                        NetworkCookieRequestContext::subresource("GET").with_cross_site_context()
                    )
                    .included_cookies
                    .is_empty()
            );
            assert!(
                cookies
                    .cookie_access_report_for_request(
                        &"http://different.invalid/".parse().unwrap(),
                        NetworkCookieRequestContext::top_level_navigation("GET")
                    )
                    .included_cookies
                    .is_empty()
            );
            assert!(
                BrowserCookieStore::default()
                    .cookie_access_report_for_request(
                        &url,
                        NetworkCookieRequestContext::top_level_navigation("GET")
                    )
                    .included_cookies
                    .is_empty()
            );
            let mut following = TransportRequest::new(url, "GET");
            following.connection.proxy = Some(String::new());
            following.connection.no_proxy = Some(String::new());
            following.headers.push((
                "cookie".into(),
                report
                    .included_cookies
                    .iter()
                    .map(|entry| format!("{}={}", entry.cookie.name, entry.cookie.value))
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
            let mut response = transport.execute(following).await.unwrap();
            assert_eq!(
                response
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("x-fixture-connection"))
                    .unwrap()
                    .1,
                connection
            );
            while response.body.chunk().await.unwrap().is_some() {}
            assert_eq!(
                fixture.observations.requests.lock().await.last().unwrap().1["cookie"],
                "first=one; second=two"
            );
            let mut config = BrowserConfig::default();
            config.set_layout_policy(moli_core::LayoutPolicy::OnDemand);
            config.set_subframe_loading_enabled(true);
            config.set_optional_resource_fetch_mask(moli_core::OptionalResourceFetchMask::all());
            config.fetch_mut().set_http_proxy(Some(String::new()));
            config.fetch_mut().set_http_no_proxy(Some(String::new()));
            config.fetch_mut().set_network_blocking(false, Vec::new());
            config.fetch_mut().set_obey_robots(false);
            let browser = Browser::new(config).unwrap();
            let mut page = browser
                .fetch_allow_http_error_with_wait_until(
                    &fixture.url("/page"),
                    RenderedDomWaitUntil::DomContentLoaded,
                    Duration::from_secs(15),
                )
                .await
                .unwrap();
            let context = page
                .create_isolated_world_async("sdk-benchmark", false)
                .await
                .unwrap();
            let result = page
                .evaluate_runtime_expression_in_execution_context_with_await_async(
                    context,
                    "document.title",
                    false,
                )
                .await
                .unwrap();
            assert_eq!(result["value"], "SDK document");
            assert!(
                page.serialize_html_async()
                    .await
                    .unwrap()
                    .contains("rendered document")
            );
            page.close_async().await.unwrap();
            drop(browser);
            drop(transport);
            println!("browser-http-cookie benchmark workload passed");
        })
        .await;
}
