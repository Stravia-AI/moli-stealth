//! Shared libcurl multi scheduler for Moli network requests.

mod dns_adapter;
mod runtime;

pub use dns_adapter::CurlDnsResolution;
pub use runtime::{
    CurlMultiCompletion, CurlMultiJob, CurlMultiRuntime, CurlMultiRuntimeConfig, CurlOriginKey,
    CurlSubmitError, CurlTransferId,
};
