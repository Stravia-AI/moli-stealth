use crate::{
    context_bootstrap::evaluate_match_media_query_list_with_viewport,
    document_runtime::DomHandle,
    dom::native::{DomHost, Node},
    protocol_types::EmulatedMediaOverrides,
    style_engine::StyleViewport,
};

pub(crate) fn link_rel_qualifies_as_stylesheet(rel: Option<&str>, title: Option<&str>) -> bool {
    let Some(rel) = rel else {
        return false;
    };
    let includes_token = |token: &str| {
        rel.split_ascii_whitespace()
            .any(|candidate| candidate.eq_ignore_ascii_case(token))
    };
    includes_token("stylesheet")
        && (!includes_token("alternate") || title.is_some_and(|title| !title.is_empty()))
}

pub(super) fn stylesheet_owner_is_stylesheet_source_enabled(
    host: &DomHost,
    handle: DomHandle,
    media_text: &str,
    emulated_media: &EmulatedMediaOverrides,
    viewport: StyleViewport,
) -> bool {
    host.node(handle).is_some_and(|node| {
        node.as_element()
            .is_some_and(|element| element.is_inline_style_element())
            && style_element_is_stylesheet_source_enabled(
                host,
                handle,
                media_text,
                emulated_media,
                viewport,
            )
    })
}

pub(super) fn stylesheet_source_base_url(host: &DomHost, handle: DomHandle) -> url::Url {
    host.owner_document_handle(handle)
        .and_then(|document_handle| {
            host.node(document_handle)
                .and_then(Node::as_document)
                .map(|document| document.base_url().clone())
        })
        .or_else(|| host.document_base_url())
        .or_else(|| host.document_url().cloned())
        .unwrap_or_else(|| url::Url::parse("about:blank").expect("static about:blank parses"))
}

fn style_element_is_stylesheet_source_enabled(
    host: &DomHost,
    handle: DomHandle,
    media_text: &str,
    emulated_media: &EmulatedMediaOverrides,
    viewport: StyleViewport,
) -> bool {
    let Some(node) = host.node(handle) else {
        return false;
    };
    node.as_element()
        .is_some_and(|element| element.is_inline_style_element())
        && moli_web_mime::is_stylesheet_type_attribute(
            host.get_attribute(handle, "type").as_deref(),
        )
        && stylesheet_source_media_matches(media_text, emulated_media, viewport)
}

pub(super) fn linked_stylesheet_media_matches_for_stylesheet_source(
    media_text: &str,
    emulated_media: &EmulatedMediaOverrides,
    viewport: StyleViewport,
) -> bool {
    stylesheet_source_media_matches(media_text, emulated_media, viewport)
}

pub(super) fn stylesheet_source_media_matches(
    media_text: &str,
    emulated_media: &EmulatedMediaOverrides,
    viewport: StyleViewport,
) -> bool {
    let media = media_text.trim();
    media.is_empty()
        || evaluate_match_media_query_list_with_viewport(media, Some(emulated_media), viewport)
}

#[cfg(test)]
mod tests {
    use super::link_rel_qualifies_as_stylesheet;

    #[test]
    fn alternate_stylesheet_links_require_a_present_non_empty_title() {
        assert!(link_rel_qualifies_as_stylesheet(Some("stylesheet"), None));
        assert!(link_rel_qualifies_as_stylesheet(
            Some("alternate stylesheet"),
            Some("contrast")
        ));
        assert!(link_rel_qualifies_as_stylesheet(
            Some("STYLESHEET alternate"),
            Some(" ")
        ));
        assert!(!link_rel_qualifies_as_stylesheet(
            Some("alternate stylesheet"),
            None
        ));
        assert!(!link_rel_qualifies_as_stylesheet(
            Some("alternate stylesheet"),
            Some("")
        ));
        assert!(!link_rel_qualifies_as_stylesheet(Some("alternate"), None));
    }
}
