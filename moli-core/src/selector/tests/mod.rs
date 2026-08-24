mod attribute;
mod combinator;
mod host_adapter;
mod pseudo;
mod query;
mod syntax;

use url::Url;

use crate::{
    dom::{
        NodeId,
        native::{DomHost, NativeDom},
    },
    parser::HtmlParser,
};

use super::{QueryEngine, error::SelectorErrorKind};

pub(crate) fn parse_document(html: &str) -> NativeDom {
    HtmlParser::SCRIPTING_ENABLED.parse(
        Url::parse("http://example.test/").expect("valid test url"),
        html.to_owned(),
    )
}

pub(crate) fn find_by_id(document: &NativeDom, id: &str) -> NodeId {
    document
        .nodes()
        .iter()
        .find_map(|node| {
            node.kind()
                .as_element()
                .and_then(|element| (element.attribute("id") == Some(id)).then_some(node.id()))
        })
        .expect("expected element by id")
}

pub(crate) fn host_from_html(html: &str) -> DomHost {
    DomHost::from_dom(parse_document(html))
}

pub(crate) fn host_find_by_id(host: &DomHost, id: &str) -> NodeId {
    host.element_handle_by_id(id)
        .expect("expected host element by id")
}

// ---------------------------------------------------------------------------
// Helper: run a batch of (selector, expected_ids) assertions on one document.
// ---------------------------------------------------------------------------
pub(crate) fn assert_query_ids(
    engine: &QueryEngine,
    document: &NativeDom,
    cases: &[(&str, Vec<&str>)],
) {
    for (selector, expected) in cases {
        let results = engine
            .query_selector_all(document, selector)
            .unwrap_or_else(|e| {
                panic!("selector {selector:?} should succeed but got error: {e:?}")
            });
        let got: Vec<&str> = results
            .iter()
            .map(|nid| {
                document
                    .node(*nid)
                    .and_then(|n| n.kind().as_element())
                    .and_then(|el| el.attribute("id"))
                    .unwrap_or("")
            })
            .collect();
        assert_eq!(got, *expected, "selector {selector:?}");
    }
}
