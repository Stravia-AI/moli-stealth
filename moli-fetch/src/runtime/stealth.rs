use std::net::SocketAddr;

use moli_browser_profile::DEFAULT_USER_AGENT;
use moli_stealth_net::{ChromeMethod, ChromeRequest, ChromeResponse, ChromeTransport};

use super::*;
use crate::blocking::resolve_allowed_target_ips;

pub(super) fn should_use(config: &FetchConfig, request: &Request, url: &Url) -> bool {
    url.scheme() == "https"
        && matches!(request.method(), "GET" | "get" | "POST" | "post")
        && request.auth().is_none()
        && config.proxy_bearer_token().is_none()
        && config.tls_verify_host()
        && config.user_agent() == DEFAULT_USER_AGENT
}

pub(super) fn new_transport(config: &FetchConfig) -> ChromeTransport {
    ChromeTransport::new(config.http_proxy(), config.http_no_proxy())
        .expect("failed to start Chrome transport")
}

impl RuntimeOwner {
    fn execute_stealth_request(
        &self,
        url: &Url,
        method: &str,
        body: Option<&[u8]>,
        headers: &[(String, String)],
        timeout: Duration,
    ) -> Result<ChromeResponse> {
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow!("request URL has no port: `{url}`"))?;
        let resolved_addresses = resolve_allowed_target_ips(&self.config, url)?
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect();
        let method = match method {
            "GET" | "get" => ChromeMethod::Get,
            "POST" | "post" => ChromeMethod::Post(body.unwrap_or_default()),
            _ => return Err(anyhow!("unsupported Chrome transport method `{method}`")),
        };
        self.stealth_transport
            .execute(ChromeRequest {
                url,
                method,
                headers,
                resolved_addresses,
                timeout,
            })
            .map_err(|error| anyhow!("browser transport failed for {url}: {error}"))
    }

    pub(super) fn complete_stealth_buffered_attempt(
        &self,
        mut job: RuntimeJob,
        outgoing_headers: &[(String, String)],
        request_cookie_report: Option<StoredCookieQueryReport>,
        request_extra_info: Option<NetworkRequestExtraInfo>,
        response_policy: ClientHintResponsePolicy,
    ) -> std::result::Result<JobOutcome, (RuntimeResponseTx, anyhow::Error)> {
        let timeout = job.request.effective_request_timeout(&self.config);
        let response = match self.execute_stealth_request(
            &job.current_url,
            job.request.method(),
            job.request.body_bytes(),
            outgoing_headers,
            timeout,
        ) {
            Ok(response) => response,
            Err(error) => return Err((job.response_tx, error)),
        };
        if let Some(limit) = self.config.http_max_response_size()
            && response.body.len() > limit
        {
            let current_url = job.current_url.clone();
            return Err((
                job.response_tx,
                anyhow!("response exceeded configured limit of {limit} bytes for {current_url}"),
            ));
        }
        let final_url = match Url::parse(&response.url) {
            Ok(url) => url,
            Err(error) => {
                return Err((
                    job.response_tx,
                    anyhow!(
                        "browser transport returned invalid URL `{}`: {error}",
                        response.url
                    ),
                ));
            }
        };
        let status = response.status;
        let headers = response.headers;
        let cookie_set_reports = if job.request.allows_credentials_for_url(&final_url) {
            match store_response_cookies(
                &self.cookie_store,
                &final_url,
                &headers,
                &job.current_cookie_context,
            ) {
                Ok(reports) => reports,
                Err(error) => return Err((job.response_tx, error)),
            }
        } else {
            Vec::new()
        };

        if response_policy.observe_response(&final_url, &headers)
            == ClientHintResponseAction::RestartNavigation
        {
            job.redirect_chain
                .push(critical_client_hint_restart_redirect_info(
                    final_url,
                    network_response_extra_info(
                        request_extra_info
                            .expect("Critical-CH restart requires top-level request metadata"),
                        status,
                        headers,
                        cookie_set_reports,
                    ),
                ));
            return Ok(JobOutcome::Retry(Box::new(job)));
        }

        let next_url = match next_followed_redirect_url_from_parts(
            &final_url,
            status,
            &headers,
            job.redirect_count,
            job.request.follow_redirects,
        ) {
            Ok(next_url) => next_url,
            Err(error) => return Err((job.response_tx, error)),
        };
        if let Some(next_url) = next_url
            && job.request.follow_redirects
        {
            let redirect_has_extra_info = request_extra_info.is_some();
            job.redirect_chain.push(RedirectInfo {
                from_url: final_url,
                to_url: next_url.clone(),
                status,
                headers: headers.clone(),
                network_extra_info_available: redirect_has_extra_info,
                request_extra_info: None,
                response_extra_info: request_extra_info.map(|request_extra_info| {
                    network_response_extra_info(
                        request_extra_info,
                        status,
                        headers,
                        cookie_set_reports.clone(),
                    )
                }),
                redirect_has_extra_info,
                request_cookie_report,
                cookie_set_reports,
                from_cache: false,
                negotiated_http_version: Some(NegotiatedHttpVersion::Http2),
            });
            job.current_cookie_context = advance_cookie_request_context(
                job.current_cookie_context,
                &job.request.url,
                &next_url,
            );
            job.request.apply_redirect_status(status);
            job.current_url = next_url;
            job.origin_key = origin_key_for_url(&job.current_url);
            job.redirect_count += 1;
            job.http_version = RequestHttpVersion::PreferHttp2;
            return Ok(JobOutcome::Retry(Box::new(job)));
        }

        let redirected = !job.redirect_chain.is_empty();
        let response = RawResponse::from_head_and_body(
            ResponseHead {
                final_url,
                status,
                headers,
                request_cookie_report,
                cookie_set_reports,
                redirected,
                redirect_chain: job.redirect_chain,
                from_cache: false,
                negotiated_http_version: Some(NegotiatedHttpVersion::Http2),
            },
            response.body,
        )
        .with_network_request_extra_info(request_extra_info);
        Ok(JobOutcome::Complete(
            job.response_tx,
            Box::new(CompletedBufferedResponse::Raw(response)),
        ))
    }

    pub(super) fn complete_stealth_streaming_attempt(
        &self,
        state: &mut OwnerState,
        mut job: StreamingRuntimeJob,
        outgoing_headers: &[(String, String)],
        request_cookie_report: Option<StoredCookieQueryReport>,
        request_extra_info: Option<NetworkRequestExtraInfo>,
        response_policy: ClientHintResponsePolicy,
    ) -> std::result::Result<StreamingJobOutcome, (Box<StreamingRuntimeJob>, anyhow::Error)> {
        if job.cancel_handle.is_cancelled() {
            return Err((Box::new(job), anyhow!("streaming request cancelled")));
        }
        let timeout = job.request.effective_request_timeout(&self.config);
        let response = match self.execute_stealth_request(
            &job.current_url,
            "GET",
            None,
            outgoing_headers,
            timeout,
        ) {
            Ok(response) => response,
            Err(error) => return Err((Box::new(job), error)),
        };
        if let Some(limit) = self.config.http_max_response_size()
            && response.body.len() > limit
        {
            let current_url = job.current_url.clone();
            return Err((
                Box::new(job),
                anyhow!(
                    "response exceeded configured limit of {limit} bytes for {}",
                    current_url
                ),
            ));
        }

        let final_url = match Url::parse(&response.url) {
            Ok(url) => url,
            Err(error) => {
                return Err((
                    Box::new(job),
                    anyhow!(
                        "browser transport returned invalid URL `{}`: {error}",
                        response.url
                    ),
                ));
            }
        };
        let status = response.status;
        let headers = response.headers;
        let cookie_set_reports = if job.request.allows_credentials_for_url(&final_url) {
            match store_response_cookies(
                &self.cookie_store,
                &final_url,
                &headers,
                &job.current_cookie_context,
            ) {
                Ok(reports) => reports,
                Err(error) => return Err((Box::new(job), error)),
            }
        } else {
            Vec::new()
        };

        if response_policy.observe_response(&final_url, &headers)
            == ClientHintResponseAction::RestartNavigation
        {
            job.redirect_chain
                .push(critical_client_hint_restart_redirect_info(
                    final_url,
                    network_response_extra_info(
                        request_extra_info
                            .expect("Critical-CH restart requires top-level request metadata"),
                        status,
                        headers,
                        cookie_set_reports,
                    ),
                ));
            job.http_version = RequestHttpVersion::PreferHttp2;
            self.start_streaming_job_or_reply(state, job);
            return Ok(StreamingJobOutcome::Complete);
        }

        let next_url = match next_followed_redirect_url_from_parts(
            &final_url,
            status,
            &headers,
            job.redirect_count,
            job.request.follow_redirects,
        ) {
            Ok(next_url) => next_url,
            Err(error) => return Err((Box::new(job), error)),
        };
        if let Some(next_url) = next_url
            && job.request.follow_redirects
        {
            let redirect_has_extra_info = request_extra_info.is_some();
            job.redirect_chain.push(RedirectInfo {
                from_url: final_url,
                to_url: next_url.clone(),
                status,
                headers: headers.clone(),
                network_extra_info_available: redirect_has_extra_info,
                request_extra_info: None,
                response_extra_info: request_extra_info.map(|request_extra_info| {
                    network_response_extra_info(
                        request_extra_info,
                        status,
                        headers,
                        cookie_set_reports.clone(),
                    )
                }),
                redirect_has_extra_info,
                request_cookie_report,
                cookie_set_reports,
                from_cache: false,
                negotiated_http_version: Some(NegotiatedHttpVersion::Http2),
            });
            job.current_cookie_context = advance_cookie_request_context(
                job.current_cookie_context,
                &job.request.url,
                &next_url,
            );
            job.request.apply_redirect_status(status);
            job.current_url = next_url;
            job.origin_key = origin_key_for_url(&job.current_url);
            job.redirect_count += 1;
            job.http_version = RequestHttpVersion::PreferHttp2;
            self.start_streaming_job_or_reply(state, job);
            return Ok(StreamingJobOutcome::Complete);
        }

        let redirected = !job.redirect_chain.is_empty();
        let response = Response::from_head_and_lossy_body_bytes(
            ResponseHead {
                final_url,
                status,
                headers,
                request_cookie_report,
                cookie_set_reports,
                redirected,
                redirect_chain: job.redirect_chain.clone(),
                from_cache: false,
                negotiated_http_version: Some(NegotiatedHttpVersion::Http2),
            },
            response.body,
        )
        .with_network_request_extra_info(request_extra_info);
        complete_stealth_streaming_html_job(job, response);
        Ok(StreamingJobOutcome::Complete)
    }
}

fn complete_stealth_streaming_html_job(job: StreamingRuntimeJob, response: Response) {
    let network_request_extra_info = response.network_request_extra_info().cloned();
    let (head, body) = response.into_text_parts();
    if let Some(started_tx) = job.started_tx {
        let _ = started_tx.send(Ok(StreamingHtmlResponseStart {
            final_url: head.final_url,
            status: head.status,
            headers: head.headers,
            request_cookie_report: head.request_cookie_report,
            cookie_set_reports: head.cookie_set_reports,
            redirected: head.redirected,
            redirect_chain: head.redirect_chain,
            from_cache: head.from_cache,
            negotiated_http_version: head.negotiated_http_version,
            network_request_extra_info,
        }));
    }
    if let Some(body_tx) = job.body_tx
        && !body.is_empty()
    {
        let _ = body_tx.send(body);
    }
    let _ = job.completion_tx.send(Ok(()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_only_handles_coherent_default_chrome_requests() {
        let config = FetchConfig::default();
        let request = Request::get("https://example.test/").expect("valid request");
        assert!(should_use(&config, &request, &request.url));

        let mut custom = config.clone();
        custom.set_user_agent("CustomAgent/1.0".to_owned());
        assert!(!should_use(&custom, &request, &request.url));

        let http = Request::get("http://example.test/").expect("valid request");
        assert!(!should_use(&config, &http, &http.url));
    }
}
