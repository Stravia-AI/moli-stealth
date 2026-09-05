//! Incremental HTTP/1.x request and response framing.

use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::pool::{ConnectionPool, OriginKey, PooledH1};
use crate::{ConnectedStream, TransportError};

const MAX_HEAD_BYTES: usize = 64 * 1024;
const READ_CHUNK: usize = 16 * 1024;

#[derive(Debug, Clone, Copy)]
pub(crate) enum BodyFraming {
    Empty,
    Fixed(u64),
    Chunked,
    CloseDelimited,
}

pub(crate) struct ResponseHead {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) version: http::Version,
    pub(crate) framing: BodyFraming,
    pub(crate) buffered: BytesMut,
    pub(crate) reusable: bool,
}

pub(crate) async fn write_request(
    connected: &mut ConnectedStream,
    request: &super::TransportRequest,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Result<Vec<(String, String)>, TransportError> {
    let method = http::Method::from_bytes(request.method.as_bytes()).map_err(|_| {
        TransportError::InvalidInput(format!("invalid HTTP method `{}`", request.method))
    })?;
    let url = &request.url;
    let host = url
        .host_str()
        .ok_or_else(|| TransportError::InvalidInput("request URL has no host".into()))?;
    let host_for_authority = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let authority = match url.port() {
        Some(port) => format!("{host_for_authority}:{port}"),
        None => host_for_authority,
    };
    let target = if connected.absolute_form {
        url.as_str().to_owned()
    } else {
        let mut target = url.path().to_owned();
        if target.is_empty() {
            target.push('/');
        }
        if let Some(query) = url.query() {
            target.push('?');
            target.push_str(query);
        }
        target
    };

    let transfer_encodings = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"))
        .flat_map(|(_, value)| value.split(','))
        .map(str::trim)
        .collect::<Vec<_>>();
    let request_chunked = !transfer_encodings.is_empty()
        && transfer_encodings
            .last()
            .is_some_and(|value| value.eq_ignore_ascii_case("chunked"));
    if !transfer_encodings.is_empty() && !request_chunked {
        return Err(TransportError::InvalidInput(
            "HTTP/1 request Transfer-Encoding must end in chunked".into(),
        ));
    }

    let mut sent = Vec::with_capacity(headers.len() + 2);
    if !has_header(headers, "host") {
        sent.push(("Host".into(), authority));
    }
    if body.is_some()
        && !has_header(headers, "content-length")
        && !has_header(headers, "transfer-encoding")
    {
        sent.push((
            "Content-Length".into(),
            body.map_or(0, <[u8]>::len).to_string(),
        ));
    }
    sent.extend(
        headers
            .iter()
            .filter(|(name, _)| !name.starts_with(':'))
            .cloned(),
    );

    let mut wire = Vec::with_capacity(
        method.as_str().len()
            + target.len()
            + sent
                .iter()
                .map(|(name, value)| name.len() + value.len() + 4)
                .sum::<usize>()
            + 16,
    );
    wire.extend_from_slice(method.as_str().as_bytes());
    wire.push(b' ');
    wire.extend_from_slice(target.as_bytes());
    wire.extend_from_slice(b" HTTP/1.1\r\n");
    for (name, value) in &sent {
        validate_header(name, value)?;
        wire.extend_from_slice(name.as_bytes());
        wire.extend_from_slice(b": ");
        wire.extend_from_slice(value.as_bytes());
        wire.extend_from_slice(b"\r\n");
    }
    wire.extend_from_slice(b"\r\n");

    connected.stream.write_all(&wire).await?;
    sent.retain(|(name, _)| !name.eq_ignore_ascii_case("proxy-authorization"));
    if let Some(observer) = &request.observer {
        observer.request_sent(&sent);
    }
    if request_chunked {
        if let Some(body) = body.filter(|body| !body.is_empty()) {
            connected
                .stream
                .write_all(format!("{:x}\r\n", body.len()).as_bytes())
                .await?;
            connected.stream.write_all(body).await?;
            connected.stream.write_all(b"\r\n").await?;
        }
        connected.stream.write_all(b"0\r\n\r\n").await?;
    } else if let Some(body) = body {
        connected.stream.write_all(body).await?;
    }
    connected.stream.flush().await?;
    Ok(sent)
}

pub(crate) async fn read_final_head(
    connected: &mut ConnectedStream,
    request_method: &str,
) -> Result<ResponseHead, TransportError> {
    let mut buffered = BytesMut::with_capacity(8192);
    loop {
        let head_len = loop {
            if let Some(position) = find_head_end(&buffered) {
                break position + 4;
            }
            if buffered.len() >= MAX_HEAD_BYTES {
                return Err(TransportError::Http1(
                    "response headers exceed 64 KiB".into(),
                ));
            }
            let read = connected.stream.read_buf(&mut buffered).await?;
            if read == 0 {
                return if buffered.is_empty() {
                    Err(TransportError::EmptyResponse)
                } else {
                    Err(TransportError::Http1(
                        "connection closed in response headers".into(),
                    ))
                };
            }
        };

        let mut raw_headers = [httparse::EMPTY_HEADER; 128];
        let mut parsed = httparse::Response::new(&mut raw_headers);
        match parsed.parse(&buffered[..head_len]) {
            Ok(httparse::Status::Complete(_)) => {}
            Ok(httparse::Status::Partial) => {
                return Err(TransportError::Http1("incomplete response headers".into()));
            }
            Err(error) => {
                return Err(TransportError::Http1(format!(
                    "malformed response headers: {error}"
                )));
            }
        }
        let status = parsed
            .code
            .ok_or_else(|| TransportError::Http1("response has no status".into()))?;
        let version = match parsed.version {
            Some(0) => http::Version::HTTP_10,
            Some(1) => http::Version::HTTP_11,
            _ => return Err(TransportError::Http1("unsupported HTTP version".into())),
        };
        let headers = parsed
            .headers
            .iter()
            .map(|header| {
                (
                    header.name.to_ascii_lowercase(),
                    String::from_utf8_lossy(header.value).into_owned(),
                )
            })
            .collect::<Vec<_>>();
        buffered.advance(head_len);

        // 101 is a final upgrade response. Other informational responses are
        // consumed here and never confused with the final response metadata.
        if (100..200).contains(&status) && status != 101 {
            continue;
        }

        let no_body = request_method.eq_ignore_ascii_case("HEAD")
            || status == 101
            || status == 204
            || status == 205
            || status == 304;
        let connection_close =
            header_tokens(&headers, "connection").any(|token| token.eq_ignore_ascii_case("close"));
        let keep_alive = header_tokens(&headers, "connection")
            .any(|token| token.eq_ignore_ascii_case("keep-alive"));
        let transfer_encodings = header_tokens(&headers, "transfer-encoding").collect::<Vec<_>>();
        let chunked = transfer_encodings
            .last()
            .is_some_and(|token| token.eq_ignore_ascii_case("chunked"));
        if transfer_encodings
            .iter()
            .any(|token| token.eq_ignore_ascii_case("chunked"))
            && !chunked
        {
            return Err(TransportError::Http1(
                "chunked is not the final Transfer-Encoding".into(),
            ));
        }
        let framing = if no_body {
            BodyFraming::Empty
        } else if chunked {
            BodyFraming::Chunked
        } else if let Some(length) = content_length(&headers)? {
            BodyFraming::Fixed(length)
        } else {
            BodyFraming::CloseDelimited
        };
        let reusable = status != 101
            && !connection_close
            && !matches!(framing, BodyFraming::CloseDelimited)
            && (version == http::Version::HTTP_11 || keep_alive);
        return Ok(ResponseHead {
            status,
            headers,
            version,
            framing,
            buffered,
            reusable,
        });
    }
}

pub(crate) struct H1Body {
    connection: Option<PooledH1>,
    pool: ConnectionPool,
    key: OriginKey,
    framing: BodyFraming,
    buffered: BytesMut,
    reusable: bool,
    chunk_remaining: usize,
    chunk_needs_crlf: bool,
    reading_trailers: bool,
    finished: bool,
    pool_on_complete: bool,
}

impl H1Body {
    pub(crate) fn new(
        connection: PooledH1,
        pool: ConnectionPool,
        key: OriginKey,
        head: ResponseHead,
    ) -> Self {
        Self {
            connection: Some(connection),
            pool,
            key,
            framing: head.framing,
            buffered: head.buffered,
            reusable: head.reusable,
            chunk_remaining: 0,
            chunk_needs_crlf: false,
            reading_trailers: false,
            finished: false,
            pool_on_complete: true,
        }
    }

    pub(crate) async fn chunk(&mut self) -> Result<Option<Bytes>, TransportError> {
        if self.finished {
            return Ok(None);
        }
        match self.framing {
            BodyFraming::Empty => self.complete().await,
            BodyFraming::Fixed(remaining) => self.fixed_chunk(remaining).await,
            BodyFraming::Chunked => self.chunked_chunk().await,
            BodyFraming::CloseDelimited => self.close_delimited_chunk().await,
        }
    }

    pub(crate) async fn drain_for_retry(mut self) -> Result<Option<PooledH1>, TransportError> {
        self.pool_on_complete = false;
        while self.chunk().await?.is_some() {}
        Ok(self.connection.take())
    }

    async fn fixed_chunk(&mut self, remaining: u64) -> Result<Option<Bytes>, TransportError> {
        if remaining == 0 {
            return self.complete().await;
        }
        if self.buffered.is_empty() {
            let read = self.read_more().await?;
            if read == 0 {
                self.reusable = false;
                return Err(TransportError::Http1(
                    "connection closed before Content-Length bytes arrived".into(),
                ));
            }
        }
        let count = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(self.buffered.len())
            .min(READ_CHUNK);
        let bytes = self.buffered.split_to(count).freeze();
        self.framing = BodyFraming::Fixed(remaining - count as u64);
        Ok(Some(bytes))
    }

    async fn close_delimited_chunk(&mut self) -> Result<Option<Bytes>, TransportError> {
        if self.buffered.is_empty() && self.read_more().await? == 0 {
            self.finished = true;
            self.connection.take();
            return Ok(None);
        }
        let count = self.buffered.len().min(READ_CHUNK);
        Ok(Some(self.buffered.split_to(count).freeze()))
    }

    async fn chunked_chunk(&mut self) -> Result<Option<Bytes>, TransportError> {
        loop {
            if self.reading_trailers {
                if self.buffered.starts_with(b"\r\n") {
                    self.buffered.advance(2);
                    return self.complete().await;
                }
                if let Some(end) = find_head_end(&self.buffered) {
                    self.buffered.advance(end + 4);
                    return self.complete().await;
                }
                if self.buffered.len() >= MAX_HEAD_BYTES {
                    self.reusable = false;
                    return Err(TransportError::Http1("chunk trailers exceed 64 KiB".into()));
                }
                if self.read_more().await? == 0 {
                    self.reusable = false;
                    return Err(TransportError::Http1(
                        "connection closed in chunk trailers".into(),
                    ));
                }
                continue;
            }

            if self.chunk_needs_crlf {
                while self.buffered.len() < 2 {
                    if self.read_more().await? == 0 {
                        self.reusable = false;
                        return Err(TransportError::Http1(
                            "connection closed after chunk data".into(),
                        ));
                    }
                }
                if &self.buffered[..2] != b"\r\n" {
                    self.reusable = false;
                    return Err(TransportError::Http1(
                        "chunk data lacks trailing CRLF".into(),
                    ));
                }
                self.buffered.advance(2);
                self.chunk_needs_crlf = false;
            }

            if self.chunk_remaining == 0 {
                let line_end = loop {
                    if let Some(position) =
                        self.buffered.windows(2).position(|part| part == b"\r\n")
                    {
                        break position;
                    }
                    if self.buffered.len() >= MAX_HEAD_BYTES {
                        self.reusable = false;
                        return Err(TransportError::Http1("chunk size line is too large".into()));
                    }
                    if self.read_more().await? == 0 {
                        self.reusable = false;
                        return Err(TransportError::Http1(
                            "connection closed in chunk size".into(),
                        ));
                    }
                };
                let line = std::str::from_utf8(&self.buffered[..line_end])
                    .map_err(|_| TransportError::Http1("chunk size is not ASCII".into()))?;
                let size = line.split(';').next().unwrap_or("").trim();
                self.chunk_remaining = usize::from_str_radix(size, 16)
                    .map_err(|_| TransportError::Http1("invalid chunk size".into()))?;
                self.buffered.advance(line_end + 2);
                if self.chunk_remaining == 0 {
                    self.reading_trailers = true;
                    continue;
                }
            }

            if self.buffered.is_empty() && self.read_more().await? == 0 {
                self.reusable = false;
                return Err(TransportError::Http1(
                    "connection closed in chunk data".into(),
                ));
            }
            let count = self
                .chunk_remaining
                .min(self.buffered.len())
                .min(READ_CHUNK);
            let bytes = self.buffered.split_to(count).freeze();
            self.chunk_remaining -= count;
            if self.chunk_remaining == 0 {
                self.chunk_needs_crlf = true;
            }
            return Ok(Some(bytes));
        }
    }

    async fn read_more(&mut self) -> Result<usize, TransportError> {
        let connection = self.connection.as_mut().ok_or(TransportError::Cancelled)?;
        Ok(connection
            .connected
            .stream
            .read_buf(&mut self.buffered)
            .await?)
    }

    async fn complete(&mut self) -> Result<Option<Bytes>, TransportError> {
        self.finished = true;
        if self.reusable {
            if self.pool_on_complete
                && let Some(connection) = self.connection.take()
            {
                self.pool.put_h1(self.key.clone(), connection).await;
            }
        } else {
            self.connection.take();
        }
        Ok(None)
    }
}

fn validate_header(name: &str, value: &str) -> Result<(), TransportError> {
    http::header::HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| TransportError::InvalidInput(format!("invalid header name `{name}`")))?;
    http::header::HeaderValue::from_str(value)
        .map_err(|_| TransportError::InvalidInput(format!("invalid value for header `{name}`")))?;
    Ok(())
}

fn has_header(headers: &[(String, String)], wanted: &str) -> bool {
    headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(wanted))
}

fn find_head_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|part| part == b"\r\n\r\n")
}

fn header_tokens<'a>(
    headers: &'a [(String, String)],
    wanted: &'a str,
) -> impl Iterator<Item = &'a str> {
    headers
        .iter()
        .filter(move |(name, _)| name.eq_ignore_ascii_case(wanted))
        .flat_map(|(_, value)| value.split(','))
        .map(str::trim)
}

fn content_length(headers: &[(String, String)]) -> Result<Option<u64>, TransportError> {
    let values = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .flat_map(|(_, value)| value.split(','))
        .map(str::trim)
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| TransportError::Http1("invalid Content-Length".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(first) = values.first().copied() {
        if values.iter().any(|value| *value != first) {
            return Err(TransportError::Http1(
                "conflicting Content-Length fields".into(),
            ));
        }
        Ok(Some(first))
    } else {
        Ok(None)
    }
}
