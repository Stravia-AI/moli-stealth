//! Shared libcurl multi scheduler for Moli network requests.

#[cfg(windows)]
mod windows_init {
    unsafe extern "C" {
        fn curlx_verify_windows_init();
    }

    // curl 8.19 initializes Schannel before its general Win32 initialization.
    // Without an application compatibility manifest, that leaves its
    // RtlVerifyVersionInfo pointer unset and incorrectly disables ALPN. Run
    // the pinned libcurl helper before curl-rust's `.CRT$XCU` constructor.
    extern "C" fn initialize_windows_version_detection() {
        // SAFETY: this initializes one process-global function pointer before
        // any threads or libcurl handles exist.
        unsafe {
            curlx_verify_windows_init();
        }
    }

    #[used]
    #[unsafe(link_section = ".CRT$XCT")]
    static WINDOWS_VERSION_INIT: extern "C" fn() = initialize_windows_version_detection;
}

mod dns_adapter;
mod runtime;

pub use dns_adapter::CurlDnsResolution;
pub use runtime::{
    CurlMultiCompletion, CurlMultiJob, CurlMultiRuntime, CurlMultiRuntimeConfig, CurlOriginKey,
    CurlSubmitError, CurlTransferId,
};
