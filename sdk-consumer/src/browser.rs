use moli_sdk::{
    BrowserConfig, ErrorKind, EvaluateOptions, NavigationOptions, ResourceLoading, Session,
    SessionConfig, WaitUntil,
};
use std::time::Duration;
use tokio::time::timeout;

fn config() -> BrowserConfig {
    BrowserConfig {
        block_private_networks: false,
        obey_robots: false,
        resources: ResourceLoading::all(),
        ..Default::default()
    }
}

pub async fn documents_and_contexts() {
    let fixture = crate::fixture::Fixture::start().await;
    let session = Session::new(SessionConfig::default()).await.unwrap();
    let browser = session.browser(config()).await.unwrap();
    let error_page = browser
        .fetch(fixture.url("/error"), NavigationOptions::default())
        .await
        .unwrap();
    assert!(
        error_page
            .document()
            .await
            .unwrap()
            .html
            .contains("retained error page")
    );
    error_page.close().await.unwrap();
    assert!(
        browser
            .fetch(
                fixture.url("/error"),
                NavigationOptions {
                    allow_http_errors: false,
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );

    let page = browser
        .fetch(fixture.url("/tamper"), NavigationOptions::default())
        .await
        .unwrap();
    let context = page.create_isolated_world("inspection").await.unwrap();
    let options = EvaluateOptions {
        context: Some(context),
        ..Default::default()
    };
    let inspected = page
        .evaluate(
            "JSON.stringify({title:document.title,html:document.documentElement.outerHTML})",
            options.clone(),
        )
        .await
        .unwrap();
    let value: serde_json::Value =
        serde_json::from_str(inspected["value"].as_str().unwrap()).unwrap();
    assert_eq!(value["title"], "isolated document");
    assert!(value["html"].as_str().unwrap().contains("id=\"real\""));
    let error = page
        .evaluate("throw new Error('fixture exception')", options.clone())
        .await
        .expect_err("JavaScript exception became success");
    assert_eq!(error.kind, ErrorKind::JavaScript);
    assert!(error.message.contains("fixture exception"));
    let navigated = page
        .evaluate(
            format!(
                "location.assign({})",
                serde_json::to_string(&fixture.url("/page")).unwrap()
            ),
            options,
        )
        .await
        .expect_err("navigation retained the old isolated execution context");
    assert_eq!(navigated.kind, ErrorKind::ContextInvalidated);
    let document = page.document().await.unwrap();
    assert_eq!(document.url, fixture.url("/page"));
    assert!(document.html.contains("rendered document"));
    assert_eq!(
        page.state(Some(context)).await.unwrap().context_valid,
        Some(false)
    );
    let stale = page
        .evaluate(
            "document.title",
            EvaluateOptions {
                context: Some(context),
                follow_navigation: false,
                ..Default::default()
            },
        )
        .await
        .expect_err("stale execution context was accepted");
    assert_eq!(stale.kind, ErrorKind::ContextInvalidated);
    let rebound = page.create_isolated_world("inspection").await.unwrap();
    assert_eq!(
        page.evaluate(
            "document.title",
            EvaluateOptions {
                context: Some(rebound),
                ..Default::default()
            }
        )
        .await
        .unwrap()["value"],
        "SDK document"
    );
    page.close().await.unwrap();

    let page = browser
        .fetch(fixture.url("/navigate"), NavigationOptions::default())
        .await
        .unwrap();
    assert_eq!(page.document().await.unwrap().url, fixture.url("/page"));
    page.close().await.unwrap();
    session.close().await.unwrap();
}

pub async fn dynamic_resources_and_cookie() {
    let fixture = crate::fixture::Fixture::start().await;
    let session = Session::new(SessionConfig::default()).await.unwrap();
    let browser = session
        .browser(BrowserConfig {
            user_agent: Some("Moli-SDK-Identity-Fixture".into()),
            default_headers: vec![("x-sdk-fixture".into(), "identity".into())],
            ..config()
        })
        .await
        .unwrap();
    let page = browser
        .fetch(fixture.url("/page"), NavigationOptions::default())
        .await
        .unwrap();
    let world = page
        .create_isolated_world("resource-observer")
        .await
        .unwrap();
    let ready = page.evaluate("new Promise(resolve => { const observer=new MutationObserver(check); function check(){if(document.body.dataset.frame==='ready' && document.body.dataset.worker==='worker-ready'){observer.disconnect();resolve(document.cookie);}} observer.observe(document.body,{attributes:true});check(); })", EvaluateOptions { context: Some(world), await_promise: true, ..Default::default() });
    let result = timeout(Duration::from_secs(15), ready)
        .await
        .expect("frame or worker did not progress")
        .unwrap();
    assert!(
        result["value"]
            .as_str()
            .unwrap()
            .contains("dynamic=present")
    );
    page.close().await.unwrap();
    let cookie_page = browser
        .fetch(fixture.url("/cookie-view"), NavigationOptions::default())
        .await
        .unwrap();
    assert!(
        cookie_page
            .document()
            .await
            .unwrap()
            .html
            .contains("dynamic=present")
    );
    cookie_page.close().await.unwrap();
    let loaded = browser
        .fetch(
            fixture.url("/resources"),
            NavigationOptions {
                wait_until: WaitUntil::Load,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        fixture
            .observations
            .requests
            .lock()
            .await
            .iter()
            .any(|(path, _)| path == "pixel"),
        "enabled image was never requested"
    );
    assert_eq!(
        loaded
            .evaluate("navigator.userAgent", EvaluateOptions::default())
            .await
            .unwrap()["value"],
        "Moli-SDK-Identity-Fixture"
    );
    {
        let requests = fixture.observations.requests.lock().await;
        let (_, headers) = requests.iter().find(|(path, _)| path == "pixel").unwrap();
        assert_eq!(headers["user-agent"], "Moli-SDK-Identity-Fixture");
        assert_eq!(headers["x-sdk-fixture"], "identity");
    }
    loaded.close().await.unwrap();
    browser.close().await.unwrap();

    fixture.observations.requests.lock().await.clear();
    let restricted = session
        .browser(BrowserConfig {
            resources: ResourceLoading::default(),
            ..config()
        })
        .await
        .unwrap();
    let page = restricted
        .fetch(
            fixture.url("/resources"),
            NavigationOptions {
                wait_until: WaitUntil::Load,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        !fixture
            .observations
            .requests
            .lock()
            .await
            .iter()
            .any(|(path, _)| path == "pixel"),
        "disabled image still caused an outbound request"
    );
    page.close().await.unwrap();
    restricted.close().await.unwrap();
    let blocked = session
        .browser(BrowserConfig {
            block_private_networks: true,
            ..config()
        })
        .await
        .unwrap();
    assert!(
        blocked
            .fetch(fixture.url("/page"), NavigationOptions::default())
            .await
            .is_err(),
        "private-network blocking did not affect actual navigation"
    );
    session.close().await.unwrap();
}

pub async fn close_cancels_pending_and_retains_children() {
    let fixture = crate::fixture::Fixture::start().await;
    // 临时 Session 和 Browser 均在语句结束时释放，Page 仍拥有有效父资源。
    let page = Session::new(SessionConfig::default())
        .await
        .unwrap()
        .browser(config())
        .await
        .unwrap()
        .fetch(fixture.url("/tamper"), NavigationOptions::default())
        .await
        .unwrap();
    let context = page.create_isolated_world("pending").await.unwrap();
    let waiting = page.evaluate(
        "new Promise(() => { fetch('/echo', {method:'POST',body:'pending-evaluation'}); })",
        EvaluateOptions {
            context: Some(context),
            await_promise: true,
            ..Default::default()
        },
    );
    tokio::pin!(waiting);
    tokio::select! {
        _ = &mut waiting => panic!("pending promise unexpectedly completed"),
        _ = fixture.observations.echo_seen.notified() => {},
    }
    timeout(Duration::from_secs(10), page.close())
        .await
        .expect("explicit close blocked behind pending evaluation")
        .unwrap();
    let error = timeout(Duration::from_secs(3), waiting)
        .await
        .unwrap()
        .expect_err("closed page evaluation became success");
    assert!(matches!(
        error.kind,
        ErrorKind::Closed | ErrorKind::Cancelled
    ));
    assert_eq!(
        page.document()
            .await
            .expect_err("closed page remained usable")
            .kind,
        ErrorKind::Closed
    );
    page.close().await.unwrap();
}

pub async fn close_reports_storage_failure() {
    let root = std::env::temp_dir().join(format!(
        "moli-sdk-profile-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let profile = root.join("profile");
    let session = Session::new(SessionConfig::default()).await.unwrap();
    let browser = session
        .browser(BrowserConfig {
            profile_directory: Some(profile.clone()),
            ..config()
        })
        .await
        .unwrap();
    // 在公开 profile 路径制造真实 I/O 失败，不依赖内部存储文件布局。
    std::fs::rename(&profile, root.join("moved-profile")).unwrap();
    std::fs::write(&profile, b"not a directory").unwrap();
    let error = browser
        .close()
        .await
        .expect_err("browser close swallowed profile flush failure");
    assert_eq!(error.kind, ErrorKind::Cleanup);
    session.close().await.unwrap();
}
