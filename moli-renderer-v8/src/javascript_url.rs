use percent_encoding::percent_decode_str;
use url::Url;

pub(crate) fn decoded_source(url: &Url) -> String {
    let source = url
        .as_str()
        .strip_prefix("javascript:")
        .unwrap_or_else(|| url.path());
    percent_decode_str(source).decode_utf8_lossy().into_owned()
}

pub(crate) fn csp_source(url: &Url) -> String {
    format!("javascript:{}", decoded_source(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_and_csp_source_share_percent_decoding() {
        let url = Url::parse("javascript:globalThis%2Evalue%20%3D%201").expect("javascript URL");

        assert_eq!(decoded_source(&url), "globalThis.value = 1");
        assert_eq!(csp_source(&url), "javascript:globalThis.value = 1");
    }
}
