//! MIME parsing for two distinct browser contexts.
//!
//! Choose the entry point from the browser algorithm being implemented:
//!
//! - [`parse_mime_type`] applies the WHATWG MIME parsing and serialization
//!   rules. Use it when a web-platform algorithm says to parse a MIME type,
//!   including Blob/File type handling, MIME classification and sniffing,
//!   Fetch response MIME handling, and media capability checks.
//! - [`parse_response_content_type`] matches Chromium's network-layer parsing
//!   of an HTTP response `Content-Type` field. Use it only when reproducing
//!   Chromium transport-metadata behavior, such as selecting a response
//!   `charset` or multipart `boundary`.
//!
//! These parsers intentionally have different validity and recovery rules. A
//! response header can legitimately reach either parser depending on the
//! operation: MIME classification uses [`parse_mime_type`], while transport
//! charset selection uses [`parse_response_content_type`]. The result types
//! are therefore not interchangeable.

mod response;
mod whatwg;

pub use response::{ResponseContentType, parse_response_content_type};
pub use whatwg::{
    MimeType, mime_charset, mime_essence, mime_parameter, normalize_web_api_mime_type,
    parse_mime_type,
};

#[cfg(test)]
mod tests;
