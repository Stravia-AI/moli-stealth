//! Callable command runner for the Moli CLI.

mod http_error_navigation;

use std::{io::Write, sync::Arc, time::Duration};

use crate::{
    cli::{Cli, Commands, FetchWaitUntil, normalize_args_for_compat},
    config::AppConfig,
    cookie_cache, fetch_dump,
};
use anyhow::Result;
use anyhow::{Context, anyhow};
use clap::{CommandFactory, Parser};
use moli_core::runtime::{
    Browser, FetchedDocument, NavigationRuntimeConfig, PageVmInitStage, RenderedDomWaitUntil,
    storage_partition::StoragePartitionState,
};
use moli_fetch::{Request, ensure_http_status_success};
use moli_protocol_server::ProtocolServer;

use self::http_error_navigation::{fetch_with_http_error_navigation, is_http_error_status};

pub async fn run_from_env() -> Result<()> {
    let cli = Cli::parse_from(normalize_args_for_compat(std::env::args_os()));
    let config = AppConfig::from_cli(&cli).context("failed to build app configuration")?;
    crate::telemetry::init(&config.log_filter);
    let mut stdout = std::io::stdout();
    run_cli_with_config(cli, config, &mut stdout).await
}

pub async fn run_cli<W: Write>(stdout: &mut W, cli: Cli) -> Result<()> {
    let config = AppConfig::from_cli(&cli).context("failed to build app configuration")?;
    run_cli_with_config(cli, config, stdout).await
}

pub async fn run_cli_with_config<W: Write>(
    cli: Cli,
    config: AppConfig,
    stdout: &mut W,
) -> Result<()> {
    match cli.command.clone() {
        Commands::Help => {
            let mut command = Cli::command();
            command
                .write_long_help(&mut *stdout)
                .context("failed to print CLI help")?;
            writeln!(stdout).context("failed to write CLI help trailing newline")?;
            return Ok(());
        }
        Commands::Version => {
            writeln!(stdout, "{}", env!("CARGO_PKG_VERSION"))
                .context("failed to write CLI version")?;
            return Ok(());
        }
        _ => {}
    }

    match cli.command {
        Commands::Fetch(args) => {
            let browser = Browser::new(config.browser.clone())
                .context("failed to initialize browser runtime")?;
            load_cookie_state(&browser, &config)?;
            let timeout = std::time::Duration::from_millis(args.timeout);
            let navigation_grace = Duration::from_millis(args.redirect_wait_ms);
            let request = build_fetch_request(&args.url, &config)?;
            let fetch_result = match args.wait_until {
                FetchWaitUntil::Done => {
                    fetch_with_http_error_navigation(
                        &browser,
                        request,
                        RenderedDomWaitUntil::Done,
                        timeout,
                        navigation_grace,
                    )
                    .await
                }
                FetchWaitUntil::DomContentLoaded => {
                    fetch_with_http_error_navigation(
                        &browser,
                        request,
                        RenderedDomWaitUntil::DomContentLoaded,
                        timeout,
                        navigation_grace,
                    )
                    .await
                }
                FetchWaitUntil::Load => {
                    fetch_with_http_error_navigation(
                        &browser,
                        request,
                        RenderedDomWaitUntil::Load,
                        timeout,
                        navigation_grace,
                    )
                    .await
                }
                FetchWaitUntil::NetworkIdle => {
                    browser
                        .fetch_request_document_allow_http_error_with_wait_until(
                            request,
                            RenderedDomWaitUntil::NetworkIdle,
                            timeout,
                        )
                        .await
                }
                FetchWaitUntil::DomStable => {
                    browser
                        .fetch_request_document_allow_http_error_with_wait_until(
                            request,
                            RenderedDomWaitUntil::DomStable,
                            timeout,
                        )
                        .await
                }
            };
            let fetched_document = match fetch_result {
                Ok(document) => document,
                Err(error) => {
                    finalize_fetch_browser(browser);
                    return Err(error).with_context(|| anyhow!("failed to fetch `{}`", args.url));
                }
            };

            let mut page = match fetched_document {
                FetchedDocument::Page(page) => page,
                FetchedDocument::Raw(raw_document) => {
                    if lifecycle_stage_for_fetch_wait(args.wait_until).is_some()
                        && is_http_error_status(raw_document.status())
                    {
                        let status_error = ensure_http_status_success(
                            raw_document.final_url().as_str(),
                            raw_document.status(),
                            false,
                        );
                        finalize_fetch_browser(browser);
                        return status_error
                            .context(
                                "HTTP error response is not an executable document and cannot navigate",
                            )
                            .with_context(|| anyhow!("failed to fetch `{}`", args.url));
                    }
                    if config.fetch.response_wait.is_some()
                        || args.wait_selector.is_some()
                        || args.wait_script.is_some()
                        || args.wait_script_file.is_some()
                        || args.delay_ms > 0
                    {
                        finalize_fetch_browser(browser);
                        return Err(anyhow!(
                            "raw non-HTML document fetch does not support page wait options"
                        ));
                    }
                    let rendered =
                        fetch_dump::render_raw_document_dump(&raw_document, &config.fetch)
                            .context("failed to render raw fetch output")?;
                    stdout
                        .write_all(&rendered)
                        .context("failed to write raw fetch output")?;
                    let _ = stdout.flush();
                    finalize_fetch_browser(browser);
                    return Ok(());
                }
            };

            if lifecycle_stage_for_fetch_wait(args.wait_until).is_some()
                && is_http_error_status(page.status())
            {
                let error = ensure_http_status_success(
                    page.final_url().as_str(),
                    page.status(),
                    false,
                )
                .context(
                    "navigation from the HTTP error document reached another HTTP error document",
                )
                .expect_err("HTTP error status must fail success validation");
                if let Err(close_error) = page.close_async().await {
                    tracing::warn!(
                        error = %close_error,
                        "failed to close fetched page after HTTP error navigation failure"
                    );
                }
                finalize_fetch_browser(browser);
                return Err(error).with_context(|| anyhow!("failed to fetch `{}`", args.url));
            }

            if let Some(response_wait) = config.fetch.response_wait.clone() {
                browser
                    .wait_for_subresource_response(&mut page, response_wait, timeout)
                    .await
                    .context("failed while waiting for subresource response")?;
            }

            if let Some(selector) = args.wait_selector.as_deref() {
                browser
                    .wait_for_selector(&mut page, selector, timeout)
                    .await
                    .with_context(|| anyhow!("failed while waiting for selector `{selector}`"))?;
            }

            let wait_script = match (
                args.wait_script.as_deref(),
                args.wait_script_file.as_deref(),
            ) {
                (Some(_), Some(_)) => {
                    return Err(anyhow!(
                        "`--wait-script` and `--wait-script-file` are mutually exclusive"
                    ));
                }
                (Some(script), None) => Some(script.to_owned()),
                (None, Some(path)) => Some(
                    std::fs::read_to_string(path)
                        .with_context(|| anyhow!("failed to read wait script file `{path}`"))?,
                ),
                (None, None) => None,
            };

            if let Some(script) = wait_script.as_deref() {
                browser
                    .wait_for_script_truthy(&mut page, script, timeout)
                    .await
                    .context("failed while waiting for script to become truthy")?;
            }

            if args.delay_ms > 0 {
                browser
                    .wait_for_page_delay(&mut page, std::time::Duration::from_millis(args.delay_ms))
                    .await
                    .context("failed while waiting for page delay")?;
            }

            let rendered = fetch_dump::render_page_output_async(&mut page, &config.fetch)
                .await
                .context("failed to render fetch output")?;
            stdout
                .write_all(&rendered)
                .context("failed to write fetch output")?;
            let _ = stdout.flush();
            if let Err(error) = page.close_async().await {
                tracing::warn!(error = %error, "failed to close fetched page before browser shutdown");
            }
            finalize_fetch_browser(browser);
        }
        Commands::Serve(_) => {
            let storage_partition =
                Arc::new(StoragePartitionState::open(config.browser.profile_dir())?);
            storage_partition.import_cookies(load_cookie_state_cookies(&config)?)?;
            let server = ProtocolServer::new_with_storage_partition_and_runtime_config(
                config.server.clone(),
                storage_partition,
                NavigationRuntimeConfig::from(&config.browser),
            );
            server.serve().await.context("protocol server failed")?;
        }
        Commands::Help | Commands::Version => unreachable!(),
    }

    Ok(())
}

fn build_fetch_request(url: &str, config: &AppConfig) -> Result<Request> {
    let mut request = Request::get(url)?;
    // Keep CLI-provided headers scoped to the initial document navigation.
    request.request_headers = config.fetch.request_headers.clone();
    Ok(request)
}

fn lifecycle_stage_for_fetch_wait(wait_until: FetchWaitUntil) -> Option<PageVmInitStage> {
    // Only waits defined by a concrete Document lifecycle milestone opt into
    // HTTP-error navigation recovery. `done` uses the fetch path's historical
    // load boundary; network-idle and DOM-stable remain post-load page-state
    // waits and keep their existing HTTP-error dump behavior.
    match wait_until {
        FetchWaitUntil::DomContentLoaded => Some(PageVmInitStage::DomContentLoaded),
        FetchWaitUntil::Load | FetchWaitUntil::Done => Some(PageVmInitStage::Load),
        FetchWaitUntil::NetworkIdle | FetchWaitUntil::DomStable => None,
    }
}

fn load_cookie_state(browser: &Browser, config: &AppConfig) -> Result<()> {
    browser.import_cookies(load_cookie_state_cookies(config)?)?;
    Ok(())
}

fn load_cookie_state_cookies(config: &AppConfig) -> Result<Vec<moli_cookie_jar::StoredCookie>> {
    let mut cookies = Vec::new();
    for path in &config.fetch.cookie_files {
        let loaded = cookie_cache::load_cookie_file(path)
            .with_context(|| anyhow!("failed to load cookie file `{path}`"))?;
        cookies.extend(loaded);
    }
    Ok(cookies)
}

fn finalize_fetch_browser(browser: Browser) {
    // Fetch is a one-shot CLI path, but the browser must still be dropped in an
    // orderly way. Letting network threads survive until process exit can race
    // OpenSSL global cleanup with libcurl transfers still in progress.
    // Browser::drop owns profile cookie writeback when --profile-dir is set.
    drop(browser);
}
