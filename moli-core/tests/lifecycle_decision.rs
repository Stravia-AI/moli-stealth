use moli_test_support as support;

use anyhow::{Result, anyhow};
use moli_core::{
    page::Page,
    runtime::{
        Browser, BrowserConfig as AppConfig, FetchedDocument, PageVmInitStage,
        RenderedDomWaitUntil, RendererLifecycleDecision,
    },
    testing::JsValueSnapshot,
};
use moli_fetch::Request;
use parking_lot::Mutex;
use std::sync::Arc;
use support::FixtureServer;
use tokio::time::Duration;

fn executable_page(document: FetchedDocument) -> Result<Page> {
    match document {
        FetchedDocument::Page(page) => Ok(page),
        FetchedDocument::Raw(document) => Err(anyhow!(
            "expected an executable Page, got raw document status {}",
            document.status()
        )),
    }
}

fn diagnostic_global<'a>(page: &'a Page, name: &str) -> Option<&'a JsValueSnapshot> {
    page.script_execution().global(name)
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_decider_finishes_without_extra_owner_command() -> Result<()> {
    let server = FixtureServer::spawn().await?;
    let browser = Browser::new(AppConfig::default())?;
    let url = server.url("/wait-until-domcontentloaded-runtime-script-slow");

    let observed_targets = Arc::new(Mutex::new(Vec::new()));
    let observed_targets_for_decider = observed_targets.clone();
    let mut page = executable_page(
        browser
            .fetch_document_with_lifecycle_decider(
                Request::get(&url)?,
                RenderedDomWaitUntil::DomContentLoaded,
                Duration::from_secs(5),
                Duration::ZERO,
                move |target| {
                    observed_targets_for_decider.lock().push(target);
                    Ok(RendererLifecycleDecision::Finish)
                },
            )
            .await?,
    )?;
    assert_eq!(page.status(), 200);
    {
        let observed_targets = observed_targets.lock();
        assert_eq!(observed_targets.len(), 1);
        assert_eq!(observed_targets[0].stage, PageVmInitStage::DomContentLoaded);
        assert_eq!(observed_targets[0].status, 200);
        assert_eq!(observed_targets[0].final_url.as_str(), url);
    }
    assert!(
        !page
            .serialize_html_async()
            .await?
            .contains("id=\"late-dcl-script-slow\"")
    );

    browser
        .wait_for_page_delay(&mut page, Duration::from_millis(500))
        .await?;
    assert!(
        page.serialize_html_async()
            .await?
            .contains("id=\"late-dcl-script-slow\"")
    );

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_decider_supports_static_about_blank() -> Result<()> {
    let browser = Browser::new(AppConfig::default())?;
    let observed_target = Arc::new(Mutex::new(None));
    let observed_target_for_decider = observed_target.clone();
    let page = executable_page(
        browser
            .fetch_document_with_lifecycle_decider(
                Request::get("about:blank")?,
                RenderedDomWaitUntil::Done,
                Duration::from_secs(1),
                Duration::ZERO,
                move |target| {
                    *observed_target_for_decider.lock() = Some(target);
                    Ok(RendererLifecycleDecision::Finish)
                },
            )
            .await?,
    )?;

    assert_eq!(page.status(), 200);
    assert_eq!(page.final_url().as_str(), "about:blank");
    let observed_target = observed_target.lock();
    let observed_target = observed_target.as_ref().unwrap();
    assert_eq!(observed_target.stage, PageVmInitStage::Load);
    assert_eq!(observed_target.status, 200);
    assert_eq!(observed_target.final_url.as_str(), "about:blank");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn follow_budget_does_not_extend_initial_stage_timeout() -> Result<()> {
    let server = FixtureServer::spawn().await?;
    let browser = Browser::new(AppConfig::default())?;
    let decider_was_called = Arc::new(Mutex::new(false));
    let decider_was_called_in_hook = decider_was_called.clone();

    let error = browser
        .fetch_document_with_lifecycle_decider(
            Request::get(&server.url("/wait-until-domcontentloaded-runtime-script-very-slow"))?,
            RenderedDomWaitUntil::Load,
            Duration::from_millis(100),
            Duration::from_secs(5),
            move |_| {
                *decider_was_called_in_hook.lock() = true;
                Ok(RendererLifecycleDecision::Finish)
            },
        )
        .await
        .expect_err("a follow budget must not relax the initial Load deadline");

    assert!(
        format!("{error:#}").contains("timed out after 100 ms"),
        "error={error:#}"
    );
    assert!(!*decider_was_called.lock());
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_decider_error_and_panic_retire_only_pending_page() -> Result<()> {
    let browser = Browser::new(AppConfig::default())?;

    let error = browser
        .fetch_document_with_lifecycle_decider(
            Request::get("about:blank")?,
            RenderedDomWaitUntil::Done,
            Duration::from_secs(1),
            Duration::ZERO,
            |_| Err(anyhow!("policy rejected target")),
        )
        .await
        .expect_err("a decision error must fail page creation");
    assert!(
        format!("{error:#}").contains("policy rejected target"),
        "error={error:#}"
    );

    let error = browser
        .fetch_document_with_lifecycle_decider(
            Request::get("about:blank")?,
            RenderedDomWaitUntil::Done,
            Duration::from_secs(1),
            Duration::ZERO,
            |_| -> Result<RendererLifecycleDecision> { panic!("policy panic sentinel") },
        )
        .await
        .expect_err("a decision panic must fail page creation without unwinding the owner");
    assert!(
        format!("{error:#}").contains("lifecycle decider panicked: policy panic sentinel"),
        "error={error:#}"
    );

    // The panic is contained to the failed pending Page; the same renderer
    // owner must remain usable for the next creation.
    let page = executable_page(
        browser
            .fetch_request_document_allow_http_error(Request::get("about:blank")?)
            .await?,
    )?;
    assert_eq!(page.status(), 200);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn http_error_navigation_wait_follows_same_url_reload_to_domcontentloaded() -> Result<()> {
    let server = FixtureServer::spawn().await?;
    let browser = Browser::new(AppConfig::default())?;
    let url = server.url("/wait-until-http-error-navigation");

    let observed_target = Arc::new(Mutex::new(None));
    let observed_target_for_decider = observed_target.clone();
    let page = executable_page(
        browser
            .fetch_document_with_lifecycle_decider(
                Request::get(&url)?,
                RenderedDomWaitUntil::DomContentLoaded,
                Duration::from_secs(5),
                Duration::from_secs(6),
                move |target| {
                    *observed_target_for_decider.lock() = Some(target);
                    Ok(RendererLifecycleDecision::FollowNextDocument {
                        navigation_grace_ms: 1_000,
                    })
                },
            )
            .await?,
    )?;
    {
        let observed_target = observed_target.lock();
        let observed_target = observed_target.as_ref().unwrap();
        assert_eq!(observed_target.stage, PageVmInitStage::DomContentLoaded);
        assert_eq!(observed_target.status, 403);
        assert_eq!(observed_target.final_url.as_str(), url);
    }
    assert_eq!(page.status(), 200);
    assert_eq!(page.final_url().as_str(), url);
    assert_eq!(
        diagnostic_global(&page, "httpErrorNavigationDcl"),
        Some(&JsValueSnapshot::Bool(true))
    );

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn http_error_navigation_wait_follows_same_url_reload_to_load() -> Result<()> {
    let server = FixtureServer::spawn().await?;
    let browser = Browser::new(AppConfig::default())?;
    let url = server.url("/wait-until-http-error-navigation");

    let observed_target = Arc::new(Mutex::new(None));
    let observed_target_for_decider = observed_target.clone();
    let page = executable_page(
        browser
            .fetch_document_with_lifecycle_decider(
                Request::get(&url)?,
                RenderedDomWaitUntil::Load,
                Duration::from_secs(5),
                Duration::from_secs(6),
                move |target| {
                    *observed_target_for_decider.lock() = Some(target);
                    Ok(RendererLifecycleDecision::FollowNextDocument {
                        navigation_grace_ms: 1_000,
                    })
                },
            )
            .await?,
    )?;
    {
        let observed_target = observed_target.lock();
        let observed_target = observed_target.as_ref().unwrap();
        assert_eq!(observed_target.stage, PageVmInitStage::Load);
        assert_eq!(observed_target.status, 403);
        assert_eq!(observed_target.final_url.as_str(), url);
    }
    assert_eq!(page.status(), 200);
    assert_eq!(page.final_url().as_str(), url);
    assert_eq!(
        diagnostic_global(&page, "httpErrorNavigationDcl"),
        Some(&JsValueSnapshot::Bool(true))
    );
    assert_eq!(
        diagnostic_global(&page, "httpErrorNavigationLoad"),
        Some(&JsValueSnapshot::Bool(true))
    );
    assert_eq!(
        diagnostic_global(&page, "httpErrorNavigationSlowScript"),
        Some(&JsValueSnapshot::Bool(true))
    );

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn http_error_navigation_wait_reports_no_navigation_without_refetching() -> Result<()> {
    let server = FixtureServer::spawn().await?;
    let browser = Browser::new(AppConfig::default())?;
    let url = server.url("/net/upstream/xhr/404-then-200");

    let error = browser
        .fetch_document_with_lifecycle_decider(
            Request::get(&url)?,
            RenderedDomWaitUntil::Load,
            Duration::from_secs(5),
            Duration::from_millis(1_100),
            |target| {
                assert_eq!(target.status, 404);
                Ok(RendererLifecycleDecision::FollowNextDocument {
                    navigation_grace_ms: 100,
                })
            },
        )
        .await
        .expect_err("a static 404 document must not be refetched or accepted");
    let error = format!("{error:#}");
    assert!(error.contains("404 Not Found"), "error={error}");
    assert!(error.contains("100 ms grace period"), "error={error}");

    server.shutdown().await;
    Ok(())
}
