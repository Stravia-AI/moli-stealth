use anyhow::{Context, Result};
use moli_core::{
    page::{
        PageInputExt, RendererDragData, RendererDragDataItem, RendererDraggedDirectory,
        RendererDraggedFile,
    },
    protocol_types::ScriptRunOutcome,
    runtime::{Browser, BrowserConfig as AppConfig, RenderedDomWaitUntil},
};
use moli_wpt_compat::{
    WPT_REPORT_COMPLETE_EXPRESSION, WptCasePlan, WptCasePlanAction, WptCaseRequest, WptCaseRun,
    WptFixtureServer, WptManifestAction, WptManifestDragDirectory, WptManifestDragFile,
    WptOverallStatus, WptPageReport, WptSubtest, WptSuiteReport, WptWaitUntil, case_request,
    collect_wpt_report_snapshot_expression, completed_case_plan_from_report,
    decode_page_report_value, load_case_plan, load_selected_case_plans, prepare_case_plan,
};
use std::sync::OnceLock;

pub async fn run_selected_case(
    server: &WptFixtureServer,
    case_id: &str,
) -> Result<Option<WptSuiteReport>> {
    let plans: Vec<WptCasePlan> = selected_case_plans()?
        .iter()
        .filter(|plan| plan.test.id == case_id)
        .cloned()
        .collect();
    if plans.is_empty() {
        return Ok(None);
    }
    Ok(Some(run_case_plans(server, plans).await?))
}

pub async fn run_exact_case(
    server: &WptFixtureServer,
    case_id: &str,
) -> Result<Option<WptSuiteReport>> {
    let Some(plan) = load_case_plan(case_id)? else {
        return Ok(None);
    };
    Ok(Some(run_case_plans(server, vec![plan]).await?))
}

fn selected_case_plans() -> Result<&'static [WptCasePlan]> {
    static SELECTED_PLANS: OnceLock<std::result::Result<Vec<WptCasePlan>, String>> =
        OnceLock::new();
    SELECTED_PLANS
        .get_or_init(|| load_selected_case_plans().map_err(|error| format!("{error:#}")))
        .as_ref()
        .map(Vec::as_slice)
        .map_err(|error| anyhow::anyhow!(error.clone()))
}

async fn run_case_plans(
    server: &WptFixtureServer,
    plans: Vec<WptCasePlan>,
) -> Result<WptSuiteReport> {
    let mut runs = vec![None; plans.len()];
    let mut executable_cases = Vec::new();

    for (index, plan) in plans.into_iter().enumerate() {
        match prepare_case_plan(plan)? {
            WptCasePlanAction::Skip(run) => runs[index] = Some(run),
            WptCasePlanAction::Execute(plan) => {
                let request = case_request(server, &plan.test)?;
                executable_cases.push((index, *plan, request));
            }
        }
    }

    for (index, plan, request) in executable_cases {
        let run = run_case(plan, request).await?;
        // WPT cases intentionally create and tear down many independent browser
        // instances. Give async teardown callbacks a scheduling turn before the
        // next case starts so long runs do not inherit stale worker/port work.
        tokio::task::yield_now().await;
        runs[index] = Some(run);
    }

    Ok(WptSuiteReport::new(
        runs.into_iter()
            .map(|run| run.expect("prepared WPT case should have a run result"))
            .collect(),
    ))
}

async fn run_case(plan: WptCasePlan, request: WptCaseRequest) -> Result<WptCaseRun> {
    let mut config = AppConfig::default();
    config.fetch_mut().set_tls_verify_host(false);
    // Some WPT fixtures intentionally use a non-trustworthy `.test` origin to
    // test secure-context gating. Keep all fixture traffic local even when the
    // developer shell has HTTP(S)_PROXY configured.
    config.fetch_mut().set_http_no_proxy(Some("*".to_owned()));
    config
        .fetch_mut()
        .set_http_host_resolve(wpt_fixture_host_resolve_entries(&request.url)?);
    let browser = Browser::new(config)?;
    let mut page = browser
        .fetch_with_wait_until(
            &request.url,
            rendered_wait_until(request.wait_until),
            request.timeout,
        )
        .await
        .with_context(|| format!("failed to fetch WPT case {}", plan.test.id))?;
    let mut action_failures = apply_case_actions(&browser, &mut page, &request.actions)
        .await
        .with_context(|| format!("failed to apply WPT actions for {}", plan.test.id))?;
    let page_report = collect_page_report(&browser, &mut page, request.timeout)
        .await
        .with_context(|| format!("failed to collect WPT report for {}", plan.test.id))?;
    let mut script_failures = page
        .script_execution()
        .runs()
        .iter()
        .filter_map(|run| match run.outcome() {
            ScriptRunOutcome::Failed(message) => Some(format!("script run failed: {message}")),
            _ => None,
        })
        .collect::<Vec<_>>();
    script_failures.append(&mut action_failures);

    Ok(completed_case_plan_from_report(
        plan,
        &page_report,
        script_failures,
    ))
}

fn wpt_fixture_host_resolve_entries(url: &str) -> Result<Vec<String>> {
    let url = url::Url::parse(url).with_context(|| format!("failed to parse WPT URL {url}"))?;
    let Some(port) = url.port_or_known_default() else {
        return Ok(Vec::new());
    };
    Ok(vec![
        format!("localhost:{port}:127.0.0.1"),
        format!("127.0.0.1:{port}:127.0.0.1"),
        format!("moli-wpt-insecure.test:{port}:127.0.0.1"),
    ])
}

async fn apply_case_actions(
    browser: &Browser,
    page: &mut moli_core::page::Page,
    actions: &[WptManifestAction],
) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for (index, action) in actions.iter().enumerate() {
        match action {
            WptManifestAction::Evaluate { expression } => {
                let value = page.evaluate_runtime_expression_async(expression).await?;
                if let Some(exception) = runtime_exception_message(&value) {
                    failures.push(format!("WPT action {index} evaluate failed: {exception}"));
                }
            }
            WptManifestAction::Delay { ms } => {
                browser
                    .wait_for_page_delay(page, std::time::Duration::from_millis(*ms))
                    .await?;
            }
            WptManifestAction::InsertText { text } => {
                if !page.insert_text_into_active_control_async(text).await? {
                    failures.push(format!(
                        "WPT action {index} insert-text did not edit the active control"
                    ));
                }
            }
            WptManifestAction::DispatchDrag {
                event,
                x,
                y,
                modifiers,
                items,
                files,
                directories,
                drag_operations_mask,
            } => {
                if event == "dragCancel" {
                    page.clear_active_drag_data_transfer_async().await?;
                    continue;
                }
                let data = RendererDragData {
                    items: items
                        .iter()
                        .map(|item| RendererDragDataItem {
                            mime_type: item.mime_type.clone(),
                            data: item.data.clone(),
                            title: item.title.clone(),
                            base_url: item.base_url.clone(),
                        })
                        .collect(),
                    files: files
                        .iter()
                        .map(renderer_dragged_file_from_manifest)
                        .collect(),
                    directories: directories
                        .iter()
                        .map(renderer_dragged_directory_from_manifest)
                        .collect(),
                    drag_operations_mask: *drag_operations_mask,
                };
                if !page
                    .dispatch_drag_event_at_point_async(*x, *y, event, data, *modifiers)
                    .await?
                {
                    failures.push(format!(
                        "WPT action {index} dispatch-drag did not hit a target"
                    ));
                }
            }
        }
    }
    Ok(failures)
}

fn renderer_dragged_file_from_manifest(file: &WptManifestDragFile) -> RendererDraggedFile {
    RendererDraggedFile {
        bytes: file.text.as_bytes().to_vec(),
        mime_type: file.mime_type.clone(),
        name: file.name.clone(),
        last_modified: file.last_modified,
    }
}

fn renderer_dragged_directory_from_manifest(
    directory: &WptManifestDragDirectory,
) -> RendererDraggedDirectory {
    let mut files = directory
        .files
        .iter()
        .map(renderer_dragged_file_from_manifest)
        .collect::<Vec<_>>();
    if let Some(generated) = &directory.generated_files {
        files.extend((1..=generated.count).map(|index| {
            RendererDraggedFile {
                bytes: format!("{}{}", generated.text_prefix, index)
                    .as_bytes()
                    .to_vec(),
                mime_type: generated.mime_type.clone(),
                name: format!(
                    "{}{:03}{}",
                    generated.name_prefix, index, generated.extension
                ),
                last_modified: generated.last_modified_start + index as f64 - 1.0,
            }
        }));
    }

    RendererDraggedDirectory {
        name: directory.name.clone(),
        files,
        directories: directory
            .directories
            .iter()
            .map(renderer_dragged_directory_from_manifest)
            .collect(),
    }
}

fn runtime_exception_message(value: &serde_json::Value) -> Option<String> {
    value
        .get("exception")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

async fn collect_page_report(
    browser: &Browser,
    page: &mut moli_core::page::Page,
    timeout: std::time::Duration,
) -> Result<WptPageReport> {
    let mut last_progress = current_wpt_report_snapshot(page)
        .await
        .map(|report| wpt_report_progress_key(&report));

    loop {
        if let Err(error) = browser
            .wait_for_script_truthy(page, WPT_REPORT_COMPLETE_EXPRESSION, timeout)
            .await
        {
            if !is_script_truthy_timeout(&error) {
                return Err(error);
            }
            let snapshot = current_wpt_report_snapshot(page).await;
            if let Some(report) = snapshot {
                if wpt_report_satisfies_complete_wait_expression(&report) {
                    return Ok(report);
                }
                let progress = wpt_report_progress_key(&report);
                if last_progress.as_ref() != Some(&progress) {
                    last_progress = Some(progress);
                    continue;
                }
                return Err(error).with_context(|| partial_wpt_report_message(&report));
            }
            return Err(error);
        }
        let value = page
            .evaluate_runtime_expression_async(collect_wpt_report_snapshot_expression())
            .await?;
        return decode_page_report_value(&value);
    }
}

async fn current_wpt_report_snapshot(page: &mut moli_core::page::Page) -> Option<WptPageReport> {
    page.evaluate_runtime_expression_async(collect_wpt_report_snapshot_expression())
        .await
        .ok()
        .and_then(|value| decode_page_report_value(&value).ok())
}

fn is_script_truthy_timeout(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .to_string()
            .contains("timed out waiting for script to become truthy")
    })
}

fn wpt_report_satisfies_complete_wait_expression(report: &WptPageReport) -> bool {
    report.complete && (!report.tests.is_empty() || report.status.status != "OK")
}

fn wpt_report_progress_key(
    report: &WptPageReport,
) -> (bool, String, Vec<(String, String, String)>) {
    (
        report.complete,
        report.status.status.clone(),
        report
            .tests
            .iter()
            .map(|test| (test.name.clone(), test.status.clone(), test.message.clone()))
            .collect(),
    )
}

fn partial_wpt_report_message(report: &WptPageReport) -> String {
    format!(
        "partial WPT report before timeout: complete={}, status={} {:?}, tests={}",
        report.complete,
        report.status.status,
        report.status.message,
        report
            .tests
            .iter()
            .map(|test| format!("{}={}", test.name, test.status))
            .collect::<Vec<_>>()
            .join("; ")
    )
}

fn rendered_wait_until(wait_until: WptWaitUntil) -> RenderedDomWaitUntil {
    match wait_until {
        WptWaitUntil::DomContentLoaded => RenderedDomWaitUntil::DomContentLoaded,
        WptWaitUntil::Load => RenderedDomWaitUntil::Load,
        WptWaitUntil::NetworkIdle => RenderedDomWaitUntil::NetworkIdle,
        WptWaitUntil::DomStable => RenderedDomWaitUntil::DomStable,
        WptWaitUntil::Done => RenderedDomWaitUntil::Done,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(complete: bool, status: &str, tests: Vec<WptSubtest>) -> WptPageReport {
        WptPageReport {
            complete,
            status: WptOverallStatus {
                status: status.to_owned(),
                message: String::new(),
            },
            tests,
        }
    }

    fn passing_subtest() -> WptSubtest {
        WptSubtest {
            name: "subtest".to_owned(),
            status: "PASS".to_owned(),
            message: String::new(),
        }
    }

    #[test]
    fn complete_snapshot_matches_wait_expression() {
        assert!(wpt_report_satisfies_complete_wait_expression(&report(
            true,
            "OK",
            vec![passing_subtest()]
        )));
        assert!(wpt_report_satisfies_complete_wait_expression(&report(
            true,
            "ERROR",
            Vec::new()
        )));
    }

    #[test]
    fn incomplete_snapshot_still_times_out() {
        assert!(!wpt_report_satisfies_complete_wait_expression(&report(
            false,
            "PENDING",
            vec![passing_subtest()]
        )));
        assert!(!wpt_report_satisfies_complete_wait_expression(&report(
            true,
            "OK",
            Vec::new()
        )));
    }

    #[test]
    fn report_progress_key_changes_when_subtests_advance() {
        let pending = report(false, "PENDING", Vec::new());
        let progressed = report(false, "PENDING", vec![passing_subtest()]);

        assert_ne!(
            wpt_report_progress_key(&pending),
            wpt_report_progress_key(&progressed)
        );
    }
}
