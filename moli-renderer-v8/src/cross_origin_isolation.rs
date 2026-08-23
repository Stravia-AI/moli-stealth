use url::Url;

/// Enforced Cross-Origin-Opener-Policy value used by the top-level Page owner.
///
/// The variants and swap matrix mirror Chromium's
/// `network::mojom::CrossOriginOpenerPolicyValue`. Report-only policy and
/// reporting endpoints do not alter the real browsing-context group and are
/// intentionally kept out of this owner value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CrossOriginOpenerPolicyValue {
    #[default]
    UnsafeNone,
    SameOriginAllowPopups,
    SameOrigin,
    SameOriginPlusCoep,
    NoopenerAllowPopups,
}

/// Current or prospective top-level Document state used by COOP navigation
/// admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TopLevelDocumentCrossOriginOpenerPolicy {
    value: CrossOriginOpenerPolicyValue,
    serialized_origin: String,
    is_initial_empty_document: bool,
}

impl TopLevelDocumentCrossOriginOpenerPolicy {
    pub(crate) fn new(
        value: CrossOriginOpenerPolicyValue,
        serialized_origin: String,
        is_initial_empty_document: bool,
    ) -> Self {
        Self {
            value,
            serialized_origin,
            is_initial_empty_document,
        }
    }

    pub(crate) fn from_response(final_url: &Url, headers: &[(String, String)]) -> Self {
        Self::new(
            cross_origin_opener_policy_value_from_headers(final_url, headers),
            moli_url::origin_ascii_serialization(final_url),
            false,
        )
    }

    pub(crate) const fn value(&self) -> CrossOriginOpenerPolicyValue {
        self.value
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CrossOriginEmbedderPolicy {
    #[default]
    None,
    RequireCorp,
    Credentialless,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum DocumentIsolationPolicy {
    #[default]
    None,
    IsolateAndRequireCorp,
    IsolateAndCredentialless,
}

pub(crate) fn response_headers_enable_cross_origin_isolation(
    final_url: &Url,
    headers: &[(String, String)],
) -> bool {
    if !moli_url::is_potentially_trustworthy_url(final_url) {
        return false;
    }
    if document_isolation_policy_from_headers(headers).enables_cross_origin_isolation() {
        return true;
    }
    let coop = response_header_policy_value(headers, "cross-origin-opener-policy");
    matches!(coop.as_deref(), Some("same-origin"))
        && cross_origin_embedder_policy_from_headers(headers).enables_cross_origin_isolation()
}

pub(crate) fn cross_origin_opener_policy_value_from_headers(
    final_url: &Url,
    headers: &[(String, String)],
) -> CrossOriginOpenerPolicyValue {
    if !moli_url::is_potentially_trustworthy_url(final_url) {
        return CrossOriginOpenerPolicyValue::UnsafeNone;
    }
    let mut value =
        match response_header_policy_value(headers, "cross-origin-opener-policy").as_deref() {
            Some("same-origin-allow-popups") => CrossOriginOpenerPolicyValue::SameOriginAllowPopups,
            Some("same-origin") => CrossOriginOpenerPolicyValue::SameOrigin,
            Some("noopener-allow-popups") => CrossOriginOpenerPolicyValue::NoopenerAllowPopups,
            _ => CrossOriginOpenerPolicyValue::UnsafeNone,
        };
    if value == CrossOriginOpenerPolicyValue::SameOrigin
        && cross_origin_embedder_policy_from_headers(headers).enables_cross_origin_isolation()
    {
        value = CrossOriginOpenerPolicyValue::SameOriginPlusCoep;
    }
    value
}

/// Chromium's enforced COOP browsing-instance swap matrix.
///
/// Opaque serialized origins never compare same-origin here. A future
/// group-safe opaque-origin nonce can make that identity explicit without
/// weakening this fail-closed behavior.
pub(crate) fn should_swap_browsing_context_group_for_cross_origin_opener_policy(
    current: &TopLevelDocumentCrossOriginOpenerPolicy,
    destination: &TopLevelDocumentCrossOriginOpenerPolicy,
) -> bool {
    use CrossOriginOpenerPolicyValue as Coop;

    let same_origin = current.serialized_origin != "null"
        && current.serialized_origin == destination.serialized_origin;
    match current.value {
        Coop::UnsafeNone => !matches!(destination.value, Coop::UnsafeNone),
        Coop::SameOriginAllowPopups => match destination.value {
            Coop::UnsafeNone => !current.is_initial_empty_document,
            Coop::SameOriginAllowPopups => !same_origin,
            Coop::SameOrigin | Coop::SameOriginPlusCoep | Coop::NoopenerAllowPopups => true,
        },
        Coop::NoopenerAllowPopups => match destination.value {
            Coop::UnsafeNone => false,
            Coop::NoopenerAllowPopups => current.is_initial_empty_document || !same_origin,
            Coop::SameOriginAllowPopups | Coop::SameOrigin | Coop::SameOriginPlusCoep => true,
        },
        Coop::SameOrigin | Coop::SameOriginPlusCoep => {
            current.value != destination.value || !same_origin
        }
    }
}

pub(crate) fn cross_origin_embedder_policy_from_headers(
    headers: &[(String, String)],
) -> CrossOriginEmbedderPolicy {
    match response_header_policy_value(headers, "cross-origin-embedder-policy").as_deref() {
        Some("require-corp") => CrossOriginEmbedderPolicy::RequireCorp,
        Some("credentialless") => CrossOriginEmbedderPolicy::Credentialless,
        _ => CrossOriginEmbedderPolicy::None,
    }
}

pub(crate) fn document_isolation_policy_from_headers(
    headers: &[(String, String)],
) -> DocumentIsolationPolicy {
    match response_header_policy_value(headers, "document-isolation-policy").as_deref() {
        Some("isolate-and-require-corp") => DocumentIsolationPolicy::IsolateAndRequireCorp,
        Some("isolate-and-credentialless") => DocumentIsolationPolicy::IsolateAndCredentialless,
        _ => DocumentIsolationPolicy::None,
    }
}

impl CrossOriginEmbedderPolicy {
    pub(crate) fn enables_cross_origin_isolation(self) -> bool {
        matches!(self, Self::RequireCorp | Self::Credentialless)
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RequireCorp => "require-corp",
            Self::Credentialless => "credentialless",
        }
    }
}

impl DocumentIsolationPolicy {
    pub(crate) fn enables_cross_origin_isolation(self) -> bool {
        matches!(
            self,
            Self::IsolateAndRequireCorp | Self::IsolateAndCredentialless
        )
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::IsolateAndRequireCorp => "isolate-and-require-corp",
            Self::IsolateAndCredentialless => "isolate-and-credentialless",
        }
    }
}

fn response_header_policy_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .rev()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coop_coep_headers_enable_cross_origin_isolation_for_trustworthy_urls() {
        let url = Url::parse("https://example.test/").expect("valid url");
        let headers = vec![
            (
                "Cross-Origin-Embedder-Policy".to_owned(),
                "require-corp".to_owned(),
            ),
            (
                "Cross-Origin-Opener-Policy".to_owned(),
                "same-origin".to_owned(),
            ),
        ];
        assert!(response_headers_enable_cross_origin_isolation(
            &url, &headers
        ));
    }

    #[test]
    fn cross_origin_isolation_requires_both_headers() {
        let url = Url::parse("https://example.test/").expect("valid url");
        let headers = vec![(
            "Cross-Origin-Opener-Policy".to_owned(),
            "same-origin".to_owned(),
        )];
        assert!(!response_headers_enable_cross_origin_isolation(
            &url, &headers
        ));
    }

    #[test]
    fn document_isolation_policy_enables_cross_origin_isolation_for_trustworthy_urls() {
        let url = Url::parse("https://example.test/").expect("valid url");
        let headers = vec![(
            "Document-Isolation-Policy".to_owned(),
            "isolate-and-require-corp".to_owned(),
        )];
        assert!(response_headers_enable_cross_origin_isolation(
            &url, &headers
        ));
    }

    #[test]
    fn document_isolation_policy_cross_origin_isolation_requires_trustworthy_url() {
        let url = Url::parse("http://example.test/").expect("valid url");
        let headers = vec![(
            "Document-Isolation-Policy".to_owned(),
            "isolate-and-credentialless".to_owned(),
        )];
        assert!(!response_headers_enable_cross_origin_isolation(
            &url, &headers
        ));
    }

    #[test]
    fn parses_cross_origin_embedder_policy_header_values() {
        assert_eq!(
            cross_origin_embedder_policy_from_headers(&[(
                "Cross-Origin-Embedder-Policy".to_owned(),
                "require-corp; report-to=\"endpoint\"".to_owned()
            )]),
            CrossOriginEmbedderPolicy::RequireCorp
        );
        assert_eq!(
            cross_origin_embedder_policy_from_headers(&[(
                "Cross-Origin-Embedder-Policy".to_owned(),
                "credentialless".to_owned()
            )]),
            CrossOriginEmbedderPolicy::Credentialless
        );
        assert_eq!(
            cross_origin_embedder_policy_from_headers(&[(
                "Cross-Origin-Embedder-Policy".to_owned(),
                "invalid".to_owned()
            )]),
            CrossOriginEmbedderPolicy::None
        );
    }

    #[test]
    fn parses_enforced_cross_origin_opener_policy_and_augments_same_origin_with_coep() {
        let trustworthy = Url::parse("https://example.test/").expect("trustworthy URL");
        assert_eq!(
            cross_origin_opener_policy_value_from_headers(
                &trustworthy,
                &[(
                    "Cross-Origin-Opener-Policy".to_owned(),
                    "same-origin-allow-popups; report-to=endpoint".to_owned(),
                )],
            ),
            CrossOriginOpenerPolicyValue::SameOriginAllowPopups
        );
        assert_eq!(
            cross_origin_opener_policy_value_from_headers(
                &trustworthy,
                &[
                    (
                        "Cross-Origin-Opener-Policy".to_owned(),
                        "same-origin".to_owned(),
                    ),
                    (
                        "Cross-Origin-Embedder-Policy".to_owned(),
                        "require-corp".to_owned(),
                    ),
                ],
            ),
            CrossOriginOpenerPolicyValue::SameOriginPlusCoep
        );
        let untrustworthy = Url::parse("http://example.test/").expect("untrustworthy URL");
        assert_eq!(
            cross_origin_opener_policy_value_from_headers(
                &untrustworthy,
                &[(
                    "Cross-Origin-Opener-Policy".to_owned(),
                    "same-origin".to_owned(),
                )],
            ),
            CrossOriginOpenerPolicyValue::UnsafeNone
        );
    }

    #[test]
    fn coop_group_swap_matrix_matches_chromium_for_committed_and_initial_empty_documents() {
        use CrossOriginOpenerPolicyValue as Coop;

        struct Case {
            from: Coop,
            to: Coop,
            same_origin: bool,
            cross_origin: bool,
            initial_empty_cross_origin: bool,
        }
        let cases = [
            Case {
                from: Coop::UnsafeNone,
                to: Coop::UnsafeNone,
                same_origin: false,
                cross_origin: false,
                initial_empty_cross_origin: false,
            },
            Case {
                from: Coop::UnsafeNone,
                to: Coop::SameOriginAllowPopups,
                same_origin: true,
                cross_origin: true,
                initial_empty_cross_origin: true,
            },
            Case {
                from: Coop::UnsafeNone,
                to: Coop::SameOrigin,
                same_origin: true,
                cross_origin: true,
                initial_empty_cross_origin: true,
            },
            Case {
                from: Coop::UnsafeNone,
                to: Coop::SameOriginPlusCoep,
                same_origin: true,
                cross_origin: true,
                initial_empty_cross_origin: true,
            },
            Case {
                from: Coop::SameOriginAllowPopups,
                to: Coop::UnsafeNone,
                same_origin: true,
                cross_origin: true,
                initial_empty_cross_origin: false,
            },
            Case {
                from: Coop::SameOriginAllowPopups,
                to: Coop::SameOriginAllowPopups,
                same_origin: false,
                cross_origin: true,
                initial_empty_cross_origin: true,
            },
            Case {
                from: Coop::SameOriginAllowPopups,
                to: Coop::SameOrigin,
                same_origin: true,
                cross_origin: true,
                initial_empty_cross_origin: true,
            },
            Case {
                from: Coop::SameOrigin,
                to: Coop::UnsafeNone,
                same_origin: true,
                cross_origin: true,
                initial_empty_cross_origin: true,
            },
            Case {
                from: Coop::SameOrigin,
                to: Coop::SameOrigin,
                same_origin: false,
                cross_origin: true,
                initial_empty_cross_origin: true,
            },
            Case {
                from: Coop::SameOrigin,
                to: Coop::SameOriginPlusCoep,
                same_origin: true,
                cross_origin: true,
                initial_empty_cross_origin: true,
            },
            Case {
                from: Coop::SameOriginPlusCoep,
                to: Coop::SameOriginPlusCoep,
                same_origin: false,
                cross_origin: true,
                initial_empty_cross_origin: true,
            },
            Case {
                from: Coop::NoopenerAllowPopups,
                to: Coop::UnsafeNone,
                same_origin: false,
                cross_origin: false,
                initial_empty_cross_origin: false,
            },
            Case {
                from: Coop::NoopenerAllowPopups,
                to: Coop::NoopenerAllowPopups,
                same_origin: false,
                cross_origin: true,
                initial_empty_cross_origin: true,
            },
        ];
        for case in cases {
            let current_same = TopLevelDocumentCrossOriginOpenerPolicy::new(
                case.from,
                "https://a.test".to_owned(),
                false,
            );
            let destination_same = TopLevelDocumentCrossOriginOpenerPolicy::new(
                case.to,
                "https://a.test".to_owned(),
                false,
            );
            assert_eq!(
                should_swap_browsing_context_group_for_cross_origin_opener_policy(
                    &current_same,
                    &destination_same,
                ),
                case.same_origin,
                "same-origin case {:?} -> {:?}",
                case.from,
                case.to,
            );

            let destination_cross = TopLevelDocumentCrossOriginOpenerPolicy::new(
                case.to,
                "https://b.test".to_owned(),
                false,
            );
            assert_eq!(
                should_swap_browsing_context_group_for_cross_origin_opener_policy(
                    &current_same,
                    &destination_cross,
                ),
                case.cross_origin,
                "cross-origin case {:?} -> {:?}",
                case.from,
                case.to,
            );

            let current_initial = TopLevelDocumentCrossOriginOpenerPolicy::new(
                case.from,
                "https://a.test".to_owned(),
                true,
            );
            assert_eq!(
                should_swap_browsing_context_group_for_cross_origin_opener_policy(
                    &current_initial,
                    &destination_cross,
                ),
                case.initial_empty_cross_origin,
                "initial-empty case {:?} -> {:?}",
                case.from,
                case.to,
            );
        }
    }

    #[test]
    fn parses_document_isolation_policy_header_values() {
        assert_eq!(
            document_isolation_policy_from_headers(&[(
                "Document-Isolation-Policy".to_owned(),
                "isolate-and-require-corp; report-to=\"endpoint\"".to_owned()
            )]),
            DocumentIsolationPolicy::IsolateAndRequireCorp
        );
        assert_eq!(
            document_isolation_policy_from_headers(&[(
                "Document-Isolation-Policy".to_owned(),
                "isolate-and-credentialless".to_owned()
            )]),
            DocumentIsolationPolicy::IsolateAndCredentialless
        );
        assert_eq!(
            document_isolation_policy_from_headers(&[(
                "Document-Isolation-Policy".to_owned(),
                "invalid".to_owned()
            )]),
            DocumentIsolationPolicy::None
        );
    }
}
