//! Shared parsing for MIME values and HTTP response `Content-Type` fields.
//!
//! Web-facing MIME operations use the WHATWG parser, while transport response
//! metadata uses Chromium's deliberately more tolerant network semantics. The
//! two entry points are named separately so callers cannot select the wrong
//! behavior through a generic leniency flag.

mod response;
mod whatwg;

pub use response::{ResponseContentType, parse_response_content_type};
pub use whatwg::{
    MimeType, mime_charset, mime_essence, mime_parameter, normalize_web_api_mime_type,
    parse_mime_type,
};

#[cfg(test)]
mod tests;
