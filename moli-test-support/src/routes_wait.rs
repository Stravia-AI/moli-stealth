use super::*;

const WAIT_UNTIL_LIFECYCLE_HTML: &str = "<!doctype html><html><body><script>document.addEventListener('DOMContentLoaded', () => { window.domReady = true; }); window.addEventListener('load', () => { window.loadReady = true; });</script></body></html>";
// `waitUntil: domcontentloaded` returns while the live page can keep running.
// Leave a stable post-DCL gap so cutoff assertions observe the DCL boundary
// instead of racing the later fetch completion under nextest concurrency.
const WAIT_UNTIL_DOMCONTENTLOADED_FETCH_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>document.addEventListener('DOMContentLoaded', () => { setTimeout(() => { fetch('/wait-until-data').then(r => r.text()).then(text => { document.body.setAttribute('data-state', text); const main = document.createElement('main'); main.id = 'late-dcl'; main.textContent = text; document.body.appendChild(main); }); }, 300); });</script></body></html>";
const WAIT_UNTIL_DOMCONTENTLOADED_RUNTIME_SCRIPT_HTML: &str = "<!doctype html><html><head></head><body><script>window.runtimeOwnedDclInjectedOrder=[];document.addEventListener('DOMContentLoaded',()=>{window.runtimeOwnedInOrderLoadOrder=window.runtimeOwnedDclInjectedOrder;window.runtimeOwnedDclInjectedOrder.push('dcl:'+document.readyState);const script=document.createElement('script');script.async=false;script.src='/assets/runtime_owned_in_order_load.js';script.onload=()=>{window.runtimeOwnedDclInjectedOrder.push('load');window.runtimeOwnedDclInjectedLoadOrder=window.runtimeOwnedDclInjectedOrder.join(',');const main=document.createElement('main');main.id='late-dcl-script';main.textContent='script-loaded';document.body.appendChild(main);};document.head.appendChild(script);window.runtimeOwnedDclInjectedOrder.push('after-append');window.runtimeOwnedDclInjectedDclOrder=window.runtimeOwnedDclInjectedOrder.join(',');});window.addEventListener('load',()=>{window.runtimeOwnedDclInjectedOrder.push('window-load');window.runtimeOwnedDclInjectedFinalOrder=window.runtimeOwnedDclInjectedOrder.join(',');});</script></body></html>";
// Mirrors the shutdown-sensitive CLI shape: a runtime-owned external script is
// inserted at DCL, but its fetch intentionally completes later.
const WAIT_UNTIL_DOMCONTENTLOADED_RUNTIME_SCRIPT_SLOW_HTML: &str = "<!doctype html><html><head></head><body><script>window.runtimeOwnedDclInjectedOrder=[];document.addEventListener('DOMContentLoaded',()=>{window.runtimeOwnedInOrderLoadOrder=window.runtimeOwnedDclInjectedOrder;window.runtimeOwnedDclInjectedOrder.push('dcl:'+document.readyState);const script=document.createElement('script');script.async=false;script.src='/assets/runtime_owned_in_order_load_slow.js';script.onload=()=>{window.runtimeOwnedDclInjectedOrder.push('load');window.runtimeOwnedDclInjectedLoadOrder=window.runtimeOwnedDclInjectedOrder.join(',');const main=document.createElement('main');main.id='late-dcl-script-slow';main.textContent='script-loaded-slow';document.body.appendChild(main);};document.head.appendChild(script);window.runtimeOwnedDclInjectedOrder.push('after-append');window.runtimeOwnedDclInjectedDclOrder=window.runtimeOwnedDclInjectedOrder.join(',');});window.addEventListener('load',()=>{window.runtimeOwnedDclInjectedOrder.push('window-load');window.runtimeOwnedDclInjectedFinalOrder=window.runtimeOwnedDclInjectedOrder.join(',');});</script></body></html>";
const WAIT_UNTIL_DOMCONTENTLOADED_RUNTIME_SCRIPT_VERY_SLOW_HTML: &str = "<!doctype html><html><head></head><body><script>window.runtimeOwnedDclInjectedOrder=[];document.addEventListener('DOMContentLoaded',()=>{window.runtimeOwnedInOrderLoadOrder=window.runtimeOwnedDclInjectedOrder;window.runtimeOwnedDclInjectedOrder.push('dcl:'+document.readyState);const script=document.createElement('script');script.async=false;script.src='/assets/runtime_owned_in_order_load_very_slow.js';script.onload=()=>{window.runtimeOwnedDclInjectedOrder.push('load');window.runtimeOwnedDclInjectedLoadOrder=window.runtimeOwnedDclInjectedOrder.join(',');const main=document.createElement('main');main.id='late-dcl-script-very-slow';main.textContent='script-loaded-very-slow';document.body.appendChild(main);};document.head.appendChild(script);window.runtimeOwnedDclInjectedOrder.push('after-append');window.runtimeOwnedDclInjectedDclOrder=window.runtimeOwnedDclInjectedOrder.join(',');});window.addEventListener('load',()=>{window.runtimeOwnedDclInjectedOrder.push('window-load');window.runtimeOwnedDclInjectedFinalOrder=window.runtimeOwnedDclInjectedOrder.join(',');});</script></body></html>";
// 300ms post-load delay (was 75ms): callers assert that the
// `WaitUntil::Load` snapshot does *not* contain the late <main id="late">
// element, while later `NetworkIdle` / `DomStable` snapshots do. Under
// nextest concurrency the load-event snapshot can take 100ms+ to land
// even though the renderer captures sync at the load event itself, so a
// 75ms gap is too tight — the setTimeout fires inside that gap and the
// negative assertion breaks. 300ms leaves a comfortable margin for both
// snapshot timing and the 5s test deadline (timer + fetch RTT << 5s).
const WAIT_UNTIL_DELAYED_FETCH_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>window.addEventListener('load', () => { setTimeout(() => { fetch('/wait-until-data').then(r => r.text()).then(text => { document.body.setAttribute('data-state', text); const main = document.createElement('main'); main.id = 'late'; main.textContent = text; document.body.appendChild(main); }); }, 300); });</script></body></html>";
const WAIT_UNTIL_COMPLETE_DELAYED_DOM_MUTATION_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>window.addEventListener('load', () => { setTimeout(() => { document.body.setAttribute('data-late-complete', 'yes'); const main = document.createElement('main'); main.id = 'late-complete'; main.textContent = 'late-complete'; document.body.appendChild(main); }, 800); });</script></body></html>";
const WAIT_UNTIL_COMPLETE_SLOW_FETCH_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>window.addEventListener('load', () => { fetch('/wait-until-very-slow-data').then(r => r.text()).then(text => { document.body.setAttribute('data-state', text); const main = document.createElement('main'); main.id = 'late-slow-fetch'; main.textContent = text; document.body.appendChild(main); }); });</script></body></html>";
const WAIT_UNTIL_COMPLETE_SLOW_XHR_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>window.addEventListener('load', () => { const xhr = new XMLHttpRequest(); xhr.open('GET', '/wait-until-very-slow-data'); xhr.onload = () => { document.body.setAttribute('data-state', xhr.responseText); const main = document.createElement('main'); main.id = 'late-slow-xhr'; main.textContent = xhr.responseText; document.body.appendChild(main); }; xhr.send(); });</script></body></html>";
const WAIT_UNTIL_DELAYED_JSON_FETCH_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>window.addEventListener('load', () => { setTimeout(() => { fetch('/wait-until-json-data').then(r => r.json()).then(data => { document.body.setAttribute('data-state', data.ret[0]); const main = document.createElement('main'); main.id = 'late-json'; main.textContent = data.data.url; document.body.appendChild(main); }); }, 75); });</script></body></html>";
const WAIT_UNTIL_COOKIE_FETCH_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>window.addEventListener('load', () => { setTimeout(() => { fetch('/wait-until-cookie-data').then(r => r.json()).then(data => { document.body.setAttribute('data-state', data.cookie); const main = document.createElement('main'); main.id = 'late-cookie'; main.textContent = data.cookie; document.body.appendChild(main); }); }, 75); });</script></body></html>";
// See WAIT_UNTIL_DELAYED_FETCH_HTML for the 75 -> 300 ms rationale.
const WAIT_UNTIL_DELAYED_XHR_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>window.addEventListener('load', () => { setTimeout(() => { const xhr = new XMLHttpRequest(); xhr.open('GET', '/wait-until-data'); xhr.onload = () => { document.body.setAttribute('data-state', xhr.responseText); const main = document.createElement('main'); main.id = 'late-xhr'; main.textContent = xhr.responseText; document.body.appendChild(main); }; xhr.send(); }, 300); });</script></body></html>";
const WAIT_UNTIL_XHR_LOCATION_REPLACE_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>window.addEventListener('load', () => { setTimeout(() => { const xhr = new XMLHttpRequest(); xhr.open('GET', '/wait-until-data'); xhr.onload = () => { document.body.setAttribute('data-state', xhr.responseText); location.replace('/location-nav/target?from=wait-response-xhr'); }; xhr.send(); }, 75); });</script></body></html>";
const WAIT_UNTIL_STAGGERED_FETCH_HTML: &str = "<!doctype html><html><body data-first=\"init\" data-second=\"init\"><script>window.addEventListener('load', () => { setTimeout(() => { fetch('/wait-until-data').then(r => r.text()).then(text => { document.body.setAttribute('data-first', text); }); }, 75); setTimeout(() => { fetch('/wait-until-second-data').then(r => r.text()).then(text => { document.body.setAttribute('data-second', text); const main = document.createElement('main'); main.id = 'late-second'; main.textContent = text; document.body.appendChild(main); }); }, 275); });</script></body></html>";
const WAIT_UNTIL_TIMER_CALLBACK_ERROR_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>window.addEventListener('load', () => { setTimeout(() => { document.body.setAttribute('data-before-error', 'yes'); throw new Error('timer boom'); }, 0); setTimeout(() => { document.body.setAttribute('data-after-error', 'yes'); const main = document.createElement('main'); main.id = 'after-error'; main.textContent = 'after-error'; document.body.appendChild(main); }, 20); });</script></body></html>";
const WAIT_UNTIL_INTERVAL_CALLBACK_ERROR_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>window.addEventListener('load', () => { let count = 0; const id = setInterval(() => { count += 1; document.body.setAttribute('data-interval-count', String(count)); if (count === 1) { document.body.setAttribute('data-interval-before-error', 'yes'); throw new Error('interval boom'); } clearInterval(id); document.body.setAttribute('data-interval-after-error', 'yes'); const main = document.createElement('main'); main.id = 'after-interval-error'; main.textContent = 'after-interval-error'; document.body.appendChild(main); }, 20); });</script></body></html>";
const WAIT_UNTIL_TIMER_DRIVER_WRAPPER_TAMPER_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>document.body.setAttribute('data-public-timer-driver-exposed', String('__moliRunNextTimeout' in globalThis)); document.body.setAttribute('data-host-timer-driver-exposed', String('__moliHostRunNextTimeout' in globalThis)); globalThis.__moliRunNextTimeout = () => { throw new Error('tampered timer driver wrapper'); }; window.addEventListener('load', () => { setTimeout(() => { document.body.setAttribute('data-after-tamper', 'yes'); const main = document.createElement('main'); main.id = 'after-tamper'; main.textContent = 'after-tamper'; document.body.appendChild(main); }, 20); });</script></body></html>";
const WAIT_UNTIL_OUTER_HTML_TAMPER_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>Object.defineProperty(document.documentElement, 'outerHTML', { configurable: true, get() { throw new Error('domstable must not read outerHTML'); } }); window.addEventListener('load', () => { setTimeout(() => { document.body.setAttribute('data-after-outerhtml-tamper', 'yes'); const main = document.createElement('main'); main.id = 'after-outerhtml-tamper'; main.textContent = 'after-outerhtml-tamper'; document.body.appendChild(main); }, 20); });</script></body></html>";
// Keep activity frequent enough that network-idle cannot complete before the
// best-effort timeout tests reach their deadline. The fetcher's idle threshold
// is around 500 ms; a 50 ms interval leaves enough margin under nextest load.
const WAIT_UNTIL_INTERVAL_FETCH_HTML: &str = "<!doctype html><html><body data-state=\"init\"><script>window.addEventListener('load', () => { setInterval(() => { fetch('/wait-until-data').then(() => { document.body.setAttribute('data-ping', String((Number(document.body.getAttribute('data-ping') || '0') + 1))); }); }, 50); });</script></body></html>";
const WAIT_UNTIL_INTERVAL_DOM_MUTATION_HTML: &str = "<!doctype html><html><body data-state=\"init\"><main id=\"mutation-count\">0</main><script>window.addEventListener('load', () => { setInterval(() => { const count = Number(document.body.getAttribute('data-mutation-count') || '0') + 1; document.body.setAttribute('data-mutation-count', String(count)); document.getElementById('mutation-count').textContent = String(count); }, 50); });</script></body></html>";

pub(super) fn add_wait_routes(router: Router) -> Router {
    router
        .route("/wait-until-lifecycle", get(wait_until_lifecycle_page))
        .route(
            "/wait-until-domcontentloaded-fetch",
            get(wait_until_domcontentloaded_fetch_page),
        )
        .route(
            "/wait-until-domcontentloaded-runtime-script",
            get(wait_until_domcontentloaded_runtime_script_page),
        )
        .route(
            "/wait-until-domcontentloaded-runtime-script-slow",
            get(wait_until_domcontentloaded_runtime_script_slow_page),
        )
        .route(
            "/wait-until-domcontentloaded-runtime-script-very-slow",
            get(wait_until_domcontentloaded_runtime_script_very_slow_page),
        )
        .route(
            "/wait-until-delayed-fetch",
            get(wait_until_delayed_fetch_page),
        )
        .route(
            "/wait-until-complete-delayed-dom-mutation",
            get(wait_until_complete_delayed_dom_mutation_page),
        )
        .route(
            "/wait-until-complete-slow-fetch",
            get(wait_until_complete_slow_fetch_page),
        )
        .route(
            "/wait-until-complete-slow-xhr",
            get(wait_until_complete_slow_xhr_page),
        )
        .route(
            "/wait-until-delayed-json-fetch",
            get(wait_until_delayed_json_fetch_page),
        )
        .route(
            "/wait-until-cookie-fetch",
            get(wait_until_cookie_fetch_page),
        )
        .route("/wait-until-delayed-xhr", get(wait_until_delayed_xhr_page))
        .route(
            "/wait-until-xhr-location-replace",
            get(wait_until_xhr_location_replace_page),
        )
        .route(
            "/wait-until-staggered-fetch",
            get(wait_until_staggered_fetch_page),
        )
        .route(
            "/wait-until-timer-callback-error",
            get(wait_until_timer_callback_error_page),
        )
        .route(
            "/wait-until-interval-callback-error",
            get(wait_until_interval_callback_error_page),
        )
        .route(
            "/wait-until-timer-driver-wrapper-tamper",
            get(wait_until_timer_driver_wrapper_tamper_page),
        )
        .route(
            "/wait-until-outer-html-tamper",
            get(wait_until_outer_html_tamper_page),
        )
        .route(
            "/wait-until-interval-fetch",
            get(wait_until_interval_fetch_page),
        )
        .route(
            "/wait-until-interval-dom-mutation",
            get(wait_until_interval_dom_mutation_page),
        )
        .route("/wait-until-data", get(wait_until_data_page))
        .route("/wait-until-json-data", get(wait_until_json_data_page))
        .route("/wait-until-cookie-data", get(wait_until_cookie_data_page))
        .route("/wait-until-slow-data", get(wait_until_slow_data_page))
        .route(
            "/wait-until-very-slow-data",
            get(wait_until_very_slow_data_page),
        )
        .route("/wait-until-second-data", get(wait_until_second_data_page))
}

async fn wait_until_lifecycle_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_LIFECYCLE_HTML)
}

async fn wait_until_domcontentloaded_fetch_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_DOMCONTENTLOADED_FETCH_HTML)
}

async fn wait_until_domcontentloaded_runtime_script_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_DOMCONTENTLOADED_RUNTIME_SCRIPT_HTML)
}

async fn wait_until_domcontentloaded_runtime_script_slow_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_DOMCONTENTLOADED_RUNTIME_SCRIPT_SLOW_HTML)
}

async fn wait_until_domcontentloaded_runtime_script_very_slow_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_DOMCONTENTLOADED_RUNTIME_SCRIPT_VERY_SLOW_HTML)
}

async fn wait_until_delayed_fetch_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_DELAYED_FETCH_HTML)
}

async fn wait_until_complete_delayed_dom_mutation_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_COMPLETE_DELAYED_DOM_MUTATION_HTML)
}

async fn wait_until_complete_slow_fetch_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_COMPLETE_SLOW_FETCH_HTML)
}

async fn wait_until_complete_slow_xhr_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_COMPLETE_SLOW_XHR_HTML)
}

async fn wait_until_delayed_json_fetch_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_DELAYED_JSON_FETCH_HTML)
}

async fn wait_until_cookie_fetch_page() -> Response {
    (
        [(
            SET_COOKIE,
            HeaderValue::from_static("trace_login=fixture; Path=/; SameSite=Lax"),
        )],
        Html(WAIT_UNTIL_COOKIE_FETCH_HTML),
    )
        .into_response()
}

async fn wait_until_delayed_xhr_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_DELAYED_XHR_HTML)
}

async fn wait_until_xhr_location_replace_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_XHR_LOCATION_REPLACE_HTML)
}

async fn wait_until_staggered_fetch_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_STAGGERED_FETCH_HTML)
}

async fn wait_until_timer_callback_error_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_TIMER_CALLBACK_ERROR_HTML)
}

async fn wait_until_interval_callback_error_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_INTERVAL_CALLBACK_ERROR_HTML)
}

async fn wait_until_timer_driver_wrapper_tamper_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_TIMER_DRIVER_WRAPPER_TAMPER_HTML)
}

async fn wait_until_outer_html_tamper_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_OUTER_HTML_TAMPER_HTML)
}

async fn wait_until_interval_fetch_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_INTERVAL_FETCH_HTML)
}

async fn wait_until_interval_dom_mutation_page() -> Html<&'static str> {
    Html(WAIT_UNTIL_INTERVAL_DOM_MUTATION_HTML)
}

async fn wait_until_data_page() -> Response {
    (
        [(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        "settled",
    )
        .into_response()
}

async fn wait_until_json_data_page() -> Response {
    (
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        r#"{"api":"fixture.detail","ret":["SUCCESS"],"data":{"url":"/item/42"}}"#,
    )
        .into_response()
}

async fn wait_until_cookie_data_page(headers: HeaderMap) -> Response {
    let cookie = if has_cookie(&headers, "trace_login=fixture") {
        "present"
    } else {
        "missing"
    };
    (
        [
            (CONTENT_TYPE, HeaderValue::from_static("application/json")),
            (
                SET_COOKIE,
                HeaderValue::from_static("trace_seen=1; Path=/; SameSite=Lax"),
            ),
        ],
        format!(r#"{{"api":"fixture.cookie","cookie":"{cookie}"}}"#),
    )
        .into_response()
}

async fn wait_until_slow_data_page() -> Response {
    sleep(Duration::from_millis(300)).await;
    (
        [(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        "settled-slow",
    )
        .into_response()
}

async fn wait_until_very_slow_data_page() -> Response {
    sleep(Duration::from_millis(1500)).await;
    (
        [(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        "settled-very-slow",
    )
        .into_response()
}

async fn wait_until_second_data_page() -> Response {
    (
        [(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        "settled-second",
    )
        .into_response()
}
