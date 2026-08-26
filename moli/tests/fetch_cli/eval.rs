use super::{BinaryDocumentFixtureServer, clean_output, run_moli, unique_temp_file_path};
use anyhow::Result;
use moli_test_support::FixtureServer;
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

fn run_eval(url: &str, expression: &str, extra_args: &[&str]) -> Result<super::Output> {
    run_eval_source(url, "--eval", OsStr::new(expression), extra_args)
}

fn run_eval_file(url: &str, path: &Path, extra_args: &[&str]) -> Result<super::Output> {
    run_eval_source(url, "--eval-file", path.as_os_str(), extra_args)
}

fn run_eval_source(
    url: &str,
    source_flag: &str,
    source: &OsStr,
    extra_args: &[&str],
) -> Result<super::Output> {
    let mut args = vec![
        OsString::from("moli"),
        OsString::from("fetch"),
        OsString::from("--log-level"),
        OsString::from("error"),
        OsString::from("--http-no-proxy"),
        OsString::from("*"),
        OsString::from("--wait-until"),
        OsString::from("load"),
        OsString::from(source_flag),
        source.to_owned(),
    ];
    args.extend(extra_args.iter().map(|arg| OsString::from(*arg)));
    args.push(OsString::from(url));
    run_moli(args)
}

#[test]
fn eval_uses_standard_document_apis_and_writes_text() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let server = runtime.block_on(FixtureServer::spawn())?;
    let url = server.url("/static");
    let output = run_eval(
        &url,
        r#"document.querySelector("main").id = "target"; document.getElementById("target").outerHTML"#,
        &[],
    )?;
    runtime.block_on(server.shutdown());

    assert!(
        output.status.success(),
        "stderr={}",
        clean_output(&output.stderr)
    );
    assert_eq!(
        clean_output(&output.stdout),
        "<main id=\"target\">fixture static</main>\n"
    );
    Ok(())
}

#[test]
fn eval_writes_objects_as_compact_json() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let server = runtime.block_on(FixtureServer::spawn())?;
    let url = server.url("/static");
    let output = run_eval(
        &url,
        r#"({ tag: document.querySelector("main").tagName.toLowerCase(), text: document.querySelector("main").textContent.trim() })"#,
        &[],
    )?;
    runtime.block_on(server.shutdown());

    assert!(
        output.status.success(),
        "stderr={}",
        clean_output(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        value,
        serde_json::json!({ "tag": "main", "text": "fixture static" })
    );
    assert!(output.stdout.ends_with(b"\n"));
    Ok(())
}

#[test]
fn eval_awaits_a_promise_result() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let server = runtime.block_on(FixtureServer::spawn())?;
    let url = server.url("/static");
    let output = run_eval(
        &url,
        r#"new Promise(resolve => setTimeout(() => resolve([...document.querySelectorAll("main")].map(node => node.textContent.trim())), 10))"#,
        &[],
    )?;
    runtime.block_on(server.shutdown());

    assert!(
        output.status.success(),
        "stderr={}",
        clean_output(&output.stderr)
    );
    assert_eq!(clean_output(&output.stdout), "[\"fixture static\"]\n");
    Ok(())
}

#[test]
fn eval_file_executes_large_multiline_scripts() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let server = runtime.block_on(FixtureServer::spawn())?;
    let url = server.url("/static");
    let script_file = unique_temp_file_path("eval-file-large", "extract.js")?;
    let script = format!(
        "/*{}*/\n\
         const main = document.querySelector('main');\n\
         const text = main.textContent.trim();\n\
         ({{ tag: main.tagName.toLowerCase(), text }})",
        "padding".repeat(24 * 1024),
    );
    assert!(script.len() > 128 * 1024);
    std::fs::write(&script_file, script)?;

    let output = run_eval_file(&url, &script_file, &[])?;
    runtime.block_on(server.shutdown());
    if let Some(directory) = script_file.parent() {
        let _ = std::fs::remove_dir_all(directory);
    }

    assert!(
        output.status.success(),
        "stderr={}",
        clean_output(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        value,
        serde_json::json!({ "tag": "main", "text": "fixture static" })
    );
    Ok(())
}

#[test]
fn eval_file_reports_read_errors_before_fetching() -> Result<()> {
    let script_file = unique_temp_file_path("eval-file-missing", "missing.js")?;
    let output = run_eval_file(
        "https://eval-file-must-not-fetch.invalid/",
        &script_file,
        &[],
    )?;
    if let Some(directory) = script_file.parent() {
        let _ = std::fs::remove_dir_all(directory);
    }

    let stdout = clean_output(&output.stdout);
    let stderr = clean_output(&output.stderr);
    assert!(!output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.is_empty(), "stdout={stdout}");
    assert!(
        stderr.contains("failed to read --eval-file") && stderr.contains("missing.js"),
        "stderr={stderr}"
    );
    Ok(())
}

#[test]
fn eval_remains_available_when_page_javascript_is_disabled() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let server = runtime.block_on(FixtureServer::spawn())?;
    let url = server.url("/static");
    let output = run_eval(
        &url,
        r#"document.querySelector("main").textContent.trim()"#,
        &["--disable-js"],
    )?;
    runtime.block_on(server.shutdown());

    assert!(
        output.status.success(),
        "stderr={}",
        clean_output(&output.stderr)
    );
    assert_eq!(clean_output(&output.stdout), "fixture static\n");
    Ok(())
}

#[test]
fn eval_reports_javascript_exceptions_as_command_failures() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let server = runtime.block_on(FixtureServer::spawn())?;
    let url = server.url("/static");
    let output = run_eval(&url, r#"throw new Error("extraction failed")"#, &[])?;
    runtime.block_on(server.shutdown());

    let stdout = clean_output(&output.stdout);
    let stderr = clean_output(&output.stderr);
    assert!(!output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.is_empty(), "stdout={stdout}");
    assert!(
        stderr.contains("JavaScript evaluation failed: Error: extraction failed"),
        "stderr={stderr}"
    );
    Ok(())
}

#[test]
fn eval_rejects_raw_non_html_documents() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let server = runtime.block_on(BinaryDocumentFixtureServer::spawn())?;
    let url = server.url("/inline.pdf");
    let output = run_eval(&url, "document.title", &[])?;
    runtime.block_on(server.shutdown());

    let stdout = clean_output(&output.stdout);
    let stderr = clean_output(&output.stderr);
    assert!(!output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.is_empty(), "stdout={stdout}");
    assert!(
        stderr.contains("raw non-HTML document fetch does not support --eval"),
        "stderr={stderr}"
    );
    Ok(())
}
