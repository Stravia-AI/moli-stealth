use super::clean_output;
use anyhow::Result;
use axum::{
    Router,
    extract::State,
    http::{StatusCode, Uri},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

// Reduced from Anubis 1.26.2 as served by lore.kernel.org. Its async module
// suspends on a locale fetch while a later inline script is parser-blocked by
// an earlier stylesheet. Once the stylesheet becomes available, the parser
// must resume before a later fetch continuation can observe the following DOM.
const CHALLENGE_PAGE: &str = r#"<!doctype html>
<html>
  <head>
    <title>local anubis challenge</title>
    <script>globalThis.__anubisOrder = ["bootstrap"];</script>
    <link rel="stylesheet" href="/anubis/blocked.css" onload="__anubisOrder.push('link-load'); fetch('/anubis/style-activated')">
  </head>
  <body>
    <main id="challenge">
      <p id="status">calculating</p>
      <script async type="module" src="/anubis/main.mjs"></script>
      <script src="/anubis/parser-continuation.js"></script>
      <script>document.documentElement.dataset.executionOrder = __anubisOrder.join(',');</script>
    </main>
  </body>
</html>"#;

const MAIN_MODULE: &str = r#"
const locale = await (await fetch('/anubis/locale.json')).json();
globalThis.__anubisOrder.push('module-continuation');
const progress = document.getElementById('progress');
document.documentElement.dataset.progressAtModuleContinuation = progress ? 'present' : 'missing';
progress.style.display = 'block';
document.getElementById('status').textContent = locale.ready;
location.replace(
  '/anubis/passed?order=' + encodeURIComponent(__anubisOrder.join(',')) +
  '&style=' + encodeURIComponent(document.documentElement.dataset.styleAtParserContinuation || 'missing')
);
"#;

const PARSER_CONTINUATION: &str = r#"
globalThis.__anubisOrder.push('parser-continuation');
document.documentElement.dataset.parserContinuation = 'ran';
document.documentElement.dataset.styleAtParserContinuation =
  getComputedStyle(document.getElementById('status')).color;
const progress = document.createElement('div');
progress.id = 'progress';
progress.innerHTML = '<div class="bar-inner"></div>';
document.currentScript.after(progress);
"#;

#[derive(Default)]
struct Event {
    signaled: AtomicBool,
    notify: Notify,
}

impl Event {
    fn signal(&self) {
        if !self.signaled.swap(true, Ordering::Release) {
            self.notify.notify_waiters();
        }
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.signaled.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

#[derive(Clone, Default)]
struct FixtureState {
    stylesheet_requested: Arc<Event>,
    parser_continuation_requested: Arc<Event>,
    locale_requested: Arc<Event>,
    style_activated: Arc<Event>,
}

struct AnubisAsyncModuleFixture {
    base_url: String,
    task: JoinHandle<()>,
}

impl AnubisAsyncModuleFixture {
    async fn spawn() -> Result<Self> {
        let state = FixtureState::default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let app = Router::new()
            .route("/anubis/challenge", get(|| async { Html(CHALLENGE_PAGE) }))
            .route("/anubis/blocked.css", get(blocked_stylesheet))
            .route(
                "/anubis/main.mjs",
                get(|| async { javascript_response(MAIN_MODULE) }),
            )
            .route("/anubis/parser-continuation.js", get(parser_continuation))
            .route("/anubis/style-activated", get(style_activated))
            .route("/anubis/locale.json", get(locale))
            .route("/anubis/passed", get(passed))
            .with_state(state);
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Anubis async-module fixture should serve");
        });
        Ok(Self {
            base_url: format!("http://{addr}"),
            task,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

async fn passed(uri: Uri) -> Html<String> {
    Html(format!(
        "<!doctype html><title>passed</title><main id='passed' data-query='{}'>passed</main>",
        uri.query().unwrap_or_default()
    ))
}

impl Drop for AnubisAsyncModuleFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn blocked_stylesheet(State(state): State<FixtureState>) -> Response {
    state.stylesheet_requested.signal();
    // A parser-blocking external classic script starts its fetch only after
    // the HTML parser has reached that script boundary. Waiting for this
    // request proves the parser is suspended behind this stylesheet before
    // the stylesheet terminal is released; the test does not depend on a
    // sleep being long enough for the parser to get there.
    state.parser_continuation_requested.wait().await;
    state.locale_requested.wait().await;
    (
        StatusCode::OK,
        [
            ("content-type", "text/css; charset=utf-8"),
            ("cache-control", "no-store"),
        ],
        "#status { color: rgb(1, 2, 3); } #progress { display: none; }",
    )
        .into_response()
}

async fn parser_continuation(State(state): State<FixtureState>) -> Response {
    state.parser_continuation_requested.signal();
    javascript_response(PARSER_CONTINUATION)
}

async fn style_activated(State(state): State<FixtureState>) -> StatusCode {
    state.style_activated.signal();
    StatusCode::NO_CONTENT
}

async fn locale(State(state): State<FixtureState>) -> Response {
    state.stylesheet_requested.wait().await;
    state.locale_requested.signal();
    // Keep the async module suspended until the stylesheet's actual DOM event
    // runs. The onload confirmation request is a browser-observed causal edge:
    // it avoids assuming that a server handler returning means response bytes
    // have already reached the renderer. If link-event work can overtake the
    // parser, releasing this response exposes the same Anubis failure.
    state.style_activated.wait().await;
    (
        StatusCode::OK,
        [
            ("content-type", "application/json"),
            ("cache-control", "no-store"),
        ],
        r#"{"ready":"worker-ready"}"#,
    )
        .into_response()
}

fn javascript_response(source: &'static str) -> Response {
    (
        StatusCode::OK,
        [
            ("content-type", "application/javascript; charset=utf-8"),
            ("cache-control", "no-store"),
        ],
        source,
    )
        .into_response()
}

#[test]
fn cli_async_module_does_not_overtake_stylesheet_unblocked_parser() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let server = runtime.block_on(AnubisAsyncModuleFixture::spawn())?;
    let url = server.url("/anubis/challenge");
    let output = Command::new(env!("CARGO_BIN_EXE_moli"))
        .args([
            "fetch",
            "--http-no-proxy",
            "*",
            "--wait-until",
            "done",
            "--delay-ms",
            "250",
            "--timeout",
            "3000",
            "--dump",
            "html",
            &url,
        ])
        .output()?;
    let stdout = clean_output(&output.stdout);
    let stderr = clean_output(&output.stderr);

    assert!(
        output.status.success(),
        "local Anubis fetch failed: stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("id=\"passed\""),
        "async module resumed before the stylesheet-unblocked parser inserted #progress: stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("order=bootstrap%2Cparser-continuation%2Clink-load%2Cmodule-continuation"),
        "stylesheet/parser ordering diverged from Chromium: stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("style=rgb(1%2C%202%2C%203)"),
        "the parser resumed before the completed stylesheet affected computed style: stdout={stdout}\nstderr={stderr}"
    );
    Ok(())
}
