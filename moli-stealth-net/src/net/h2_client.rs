//! Incremental HTTP/2 client built on the fingerprint-configurable `http2` fork.

use bytes::Bytes;
use http2::{
    RecvStream,
    client::{Builder, Connection, SendRequest},
    frame::{PseudoId, PseudoOrder, SettingId, SettingsOrder, StreamDependency, StreamId},
};

use crate::{BoxedStream, H2PseudoHeader, H2Setting, TransportError, TransportFingerprint};

use super::{TransportRequest, pool::H2StreamPermit};

pub(crate) async fn handshake(
    io: BoxedStream,
    fingerprint: &TransportFingerprint,
) -> Result<(SendRequest<Bytes>, Connection<BoxedStream, Bytes>), TransportError> {
    let h2 = &fingerprint.h2;
    let mut builder = Builder::new();
    if let Some(value) = h2.header_table_size {
        builder.header_table_size(value);
    }
    if let Some(value) = h2.enable_push {
        builder.enable_push(value);
    }
    if let Some(value) = h2.max_concurrent_streams {
        builder.max_concurrent_streams(value);
    }
    if let Some(value) = h2.initial_stream_window_size {
        builder.initial_window_size(value);
    }
    if let Some(value) = h2.max_frame_size {
        builder.max_frame_size(value);
    }
    if let Some(value) = h2.max_header_list_size {
        builder.max_header_list_size(value);
    }
    if let Some(value) = h2.connection_window_size {
        builder.initial_connection_window_size(value);
    }
    if !h2.settings_order.is_empty() {
        let mut order = SettingsOrder::builder();
        for setting in &h2.settings_order {
            order = order.push(match setting {
                H2Setting::HeaderTableSize => SettingId::HeaderTableSize,
                H2Setting::EnablePush => SettingId::EnablePush,
                H2Setting::MaxConcurrentStreams => SettingId::MaxConcurrentStreams,
                H2Setting::InitialWindowSize => SettingId::InitialWindowSize,
                H2Setting::MaxFrameSize => SettingId::MaxFrameSize,
                H2Setting::MaxHeaderListSize => SettingId::MaxHeaderListSize,
            });
        }
        builder.settings_order(order.build());
    }
    if !h2.pseudo_header_order.is_empty() {
        let mut order = PseudoOrder::builder();
        for pseudo in &h2.pseudo_header_order {
            order = order.push(match pseudo {
                H2PseudoHeader::Method => PseudoId::Method,
                H2PseudoHeader::Authority => PseudoId::Authority,
                H2PseudoHeader::Scheme => PseudoId::Scheme,
                H2PseudoHeader::Path => PseudoId::Path,
            });
        }
        builder.headers_pseudo_order(order.build());
    }
    if let Some(priority) = h2.headers_priority {
        builder.headers_stream_dependency(StreamDependency::new(
            StreamId::from(priority.stream_dependency),
            priority.weight,
            priority.exclusive,
        ));
    }

    builder
        .handshake(io)
        .await
        .map_err(|error| TransportError::Http2(format!("handshake failed: {error}")))
}

pub(crate) struct H2Response {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: H2Body,
    pub(crate) sent_headers: Vec<(String, String)>,
}

pub(crate) struct H2RequestError {
    pub(crate) error: TransportError,
    pub(crate) connection_unusable: bool,
}

impl H2RequestError {
    fn request(error: TransportError) -> Self {
        Self {
            error,
            connection_unusable: false,
        }
    }

    fn protocol(context: &str, error: http2::Error) -> Self {
        Self {
            connection_unusable: !error.is_reset(),
            error: TransportError::Http2(format!("{context}: {error}")),
        }
    }
}

pub(crate) async fn send_request(
    sender: SendRequest<Bytes>,
    transport_request: &TransportRequest,
    body: Option<Vec<u8>>,
    stream_permit: H2StreamPermit,
    priority: Option<crate::H2HeadersPriority>,
) -> Result<H2Response, H2RequestError> {
    let method = http::Method::from_bytes(transport_request.method.as_bytes()).map_err(|_| {
        H2RequestError::request(TransportError::InvalidInput(format!(
            "invalid HTTP method `{}`",
            transport_request.method
        )))
    })?;
    let headers = &transport_request.headers;
    let explicit_authority = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.as_str());
    let mut uri = transport_request
        .url
        .as_str()
        .parse::<http::Uri>()
        .map_err(|error| {
            H2RequestError::request(TransportError::InvalidInput(format!(
                "invalid request URI: {error}"
            )))
        })?;
    if let Some(authority) = explicit_authority {
        let mut parts = uri.into_parts();
        parts.authority = Some(authority.parse().map_err(|error| {
            H2RequestError::request(TransportError::InvalidInput(format!(
                "invalid Host header: {error}"
            )))
        })?);
        uri = http::Uri::from_parts(parts).map_err(|error| {
            H2RequestError::request(TransportError::InvalidInput(format!(
                "invalid request URI: {error}"
            )))
        })?;
    }
    let authority = uri.authority().map(ToString::to_string).ok_or_else(|| {
        H2RequestError::request(TransportError::InvalidInput(
            "HTTP/2 request URI has no authority".into(),
        ))
    })?;
    let mut request = http::Request::builder()
        .version(http::Version::HTTP_2)
        .method(method.clone())
        .uri(uri);
    let mut sent_headers = Vec::with_capacity(headers.len() + 2);
    sent_headers.push(("host".into(), authority));
    let has_content_length = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-length"));
    if let Some(bytes) = &body
        && !has_content_length
    {
        let value = bytes.len().to_string();
        request = request.header("content-length", &value);
        sent_headers.push(("content-length".into(), value));
    }
    for (name, value) in headers {
        if name.starts_with(':') {
            return Err(H2RequestError::request(TransportError::InvalidInput(
                "HTTP/2 pseudo-headers are derived from the request URL".into(),
            )));
        }
        if name.eq_ignore_ascii_case("host")
            || is_connection_specific(name, headers)
            || (name.eq_ignore_ascii_case("te")
                && value
                    .split(',')
                    .any(|token| !token.trim().eq_ignore_ascii_case("trailers")))
        {
            continue;
        }
        let name = name.to_ascii_lowercase();
        request = request.header(name.as_str(), value.as_str());
        sent_headers.push((name, value.clone()));
    }
    let mut request = request.body(()).map_err(|error| {
        H2RequestError::request(TransportError::InvalidInput(format!(
            "invalid request: {error}"
        )))
    })?;
    if let Some(priority) = priority {
        request.extensions_mut().insert(StreamDependency::new(
            StreamId::from(priority.stream_dependency),
            priority.weight,
            priority.exclusive,
        ));
    }

    let mut ready = sender
        .ready()
        .await
        .map_err(|error| H2RequestError::protocol("connection is not ready", error))?;
    let end_stream = body.as_ref().is_none_or(Vec::is_empty);
    let (response, mut request_body) = ready
        .send_request(request, end_stream)
        .map_err(|error| H2RequestError::protocol("request send failed", error))?;
    if let Some(observer) = &transport_request.observer {
        observer.request_sent(&sent_headers);
    }
    if let Some(body) = body.filter(|body| !body.is_empty()) {
        request_body
            .send_data(Bytes::from(body), true)
            .map_err(|error| H2RequestError::protocol("request body send failed", error))?;
    }

    let response = response
        .await
        .map_err(|error| H2RequestError::protocol("response failed", error))?;
    let (parts, stream) = response.into_parts();
    let headers = parts
        .headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect::<Vec<_>>();
    let status = parts.status.as_u16();
    if let Some(observer) = &transport_request.observer {
        observer.response_received(status, &headers);
    }
    let no_body = method == http::Method::HEAD
        || status == 101
        || status == 204
        || status == 205
        || status == 304;
    Ok(H2Response {
        status,
        headers,
        body: H2Body {
            stream: Some(stream),
            stream_permit: Some(stream_permit),
            pending_capacity: 0,
            no_body,
        },
        sent_headers,
    })
}

fn is_connection_specific(name: &str, headers: &[(String, String)]) -> bool {
    name.eq_ignore_ascii_case("connection")
        || name.eq_ignore_ascii_case("keep-alive")
        || name.eq_ignore_ascii_case("proxy-connection")
        || name.eq_ignore_ascii_case("transfer-encoding")
        || name.eq_ignore_ascii_case("upgrade")
        || headers
            .iter()
            .filter(|(candidate, _)| candidate.eq_ignore_ascii_case("connection"))
            .flat_map(|(_, value)| value.split(','))
            .any(|token| token.trim().eq_ignore_ascii_case(name))
}

pub(crate) struct H2Body {
    stream: Option<RecvStream>,
    stream_permit: Option<H2StreamPermit>,
    pending_capacity: usize,
    no_body: bool,
}

impl H2Body {
    pub(crate) async fn chunk(&mut self) -> Result<Option<Bytes>, TransportError> {
        if self.no_body {
            self.stream.take();
            self.stream_permit.take();
            return Ok(None);
        }
        let Some(stream) = self.stream.as_mut() else {
            return Ok(None);
        };
        if self.pending_capacity != 0 {
            stream
                .flow_control()
                .release_capacity(self.pending_capacity)
                .map_err(|error| TransportError::Http2(format!("flow control failed: {error}")))?;
            self.pending_capacity = 0;
        }
        match stream.data().await {
            Some(Ok(bytes)) => {
                self.pending_capacity = bytes.len();
                Ok(Some(bytes))
            }
            Some(Err(error)) => Err(TransportError::Http2(format!(
                "response body failed: {error}"
            ))),
            None => {
                self.stream.take();
                self.stream_permit.take();
                Ok(None)
            }
        }
    }

    pub(crate) async fn drain(mut self) -> Result<(), TransportError> {
        while self.chunk().await?.is_some() {}
        Ok(())
    }
}
