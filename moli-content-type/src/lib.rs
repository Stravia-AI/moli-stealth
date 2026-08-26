//! MIME parsing for two distinct browser contexts.
//!
//! Choose the entry point from the source of the value:
//!
//! - [`parse_mime_type`] applies the WHATWG MIME parsing and serialization
//!   rules. Use it for standards-facing MIME operations such as Blob/File type
//!   handling, MIME classification, and interpreting an already selected MIME
//!   value.
//! - [`parse_response_content_type`] matches Chromium's network-layer parsing
//!   of a raw HTTP response `Content-Type` field. Use it when transport
//!   metadata must retain Chromium behavior for values such as `charset` and
//!   multipart `boundary`.
//!
//! These parsers intentionally have different validity and recovery rules. A
//! value accepted by one is not necessarily accepted by the other, so callers
//! must select the parser from the value's origin rather than treating the two
//! result types as interchangeable.

mod response;
mod whatwg;

pub use response::{ResponseContentType, parse_response_content_type};
pub use whatwg::{
    MimeType, mime_charset, mime_essence, mime_parameter, normalize_web_api_mime_type,
    parse_mime_type,
};

#[cfg(test)]
mod tests;
