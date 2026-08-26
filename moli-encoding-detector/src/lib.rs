//! Heuristic detection of legacy encodings used by web documents.

use compact_enc_det::{DetectHints, Encoding as CedEncoding, TextCorpusType, detect_encoding};
use encoding_rs::Encoding;

/// Detects a legacy HTML encoding using Chromium's CED configuration.
///
/// UTF encodings are deliberately excluded: browser-side heuristics exist for
/// legacy content, not as a substitute for labelling modern UTF-8 documents.
pub fn detect_legacy_html_encoding(
    bytes: &[u8],
    url_hint: Option<&str>,
) -> Option<&'static Encoding> {
    if bytes.iter().all(u8::is_ascii) {
        return None;
    }

    let detection = detect_encoding(
        bytes,
        DetectHints {
            url_hint: url_hint.unwrap_or_default(),
            corpus_type: TextCorpusType::WEB_CORPUS,
            // Blink asks CED to consider 7-bit encodings and then disables
            // ISO-2022-JP for HTML at the TextResourceDecoder boundary.
            ignore_7bit_mail_encodings: false,
            ..DetectHints::default()
        },
    );
    if matches!(
        detection.encoding,
        CedEncoding::UNKNOWN_ENCODING
            | CedEncoding::ASCII_7BIT
            | CedEncoding::UTF8
            | CedEncoding::UTF16BE
            | CedEncoding::UTF16LE
            | CedEncoding::UTF32BE
            | CedEncoding::UTF32LE
            | CedEncoding::JAPANESE_JIS
            | CedEncoding::KDDI_ISO_2022_JP
            | CedEncoding::SOFTBANK_ISO_2022_JP
    ) {
        return None;
    }

    Encoding::for_label(detection.mime_name.as_bytes()).filter(|encoding| {
        *encoding != encoding_rs::UTF_8
            && *encoding != encoding_rs::UTF_16BE
            && *encoding != encoding_rs::UTF_16LE
            && *encoding != encoding_rs::ISO_2022_JP
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_unlabelled_gbk_legacy_web_content() {
        let text = "吴姓－姓氏渊源。第一源流，源于姜姓，出自炎帝大臣吴权之后裔。".repeat(8);
        let bytes = encoding_rs::GBK.encode(&text).0;

        assert_eq!(
            detect_legacy_html_encoding(&bytes, None),
            Some(encoding_rs::GBK)
        );
    }

    #[test]
    fn does_not_auto_detect_utf8_or_ascii() {
        assert_eq!(
            detect_legacy_html_encoding("吴姓－姓氏渊源".as_bytes(), None),
            None
        );
        assert_eq!(detect_legacy_html_encoding(b"plain ASCII", None), None);
    }

    #[test]
    fn url_hint_disambiguates_euc_jp_like_blink() {
        let bytes = b"<TITLE>\xA5\xD1\xA5\xEF\xA1\xBC\xA5\xC1\xA5\xE3\xA1\xBC\xA5\xC8\xA1\xC3\xC5\xEA\xBB\xF1\xBE\xF0\xCA\xF3\xA4\xCE\xA5\xD5\xA5\xA3\xA5\xB9\xA5\xB3</TITLE>";

        assert_eq!(
            detect_legacy_html_encoding(bytes, None),
            Some(encoding_rs::GBK)
        );
        assert_eq!(
            detect_legacy_html_encoding(bytes, Some("http://example.co.jp/")),
            Some(encoding_rs::EUC_JP)
        );
    }
}
