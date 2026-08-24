use url::Url;

use crate::{parser::HtmlParser, renderer::ReflectorRegistry};

use super::native::{NativeDom, NodeType};

fn parse_fixture(html: &str) -> NativeDom {
    HtmlParser::SCRIPTING_ENABLED.parse(
        Url::parse("https://fixture.moli.local/native-dom.html")
            .expect("fixture url must be valid"),
        html.to_owned(),
    )
}

#[test]
fn native_dom_preserves_tree_links_and_owner_document() {
    let native_dom = parse_fixture(
        "<!doctype html><html><body><div id='alpha'><span>one</span><span>two</span></div></body></html>",
    );

    let document_node = native_dom
        .node(native_dom.document_node_id())
        .expect("document node must exist");
    assert_eq!(document_node.node_type(), NodeType::Document);
    assert!(document_node.as_document().is_some());

    let html_node_id = native_dom
        .document_element_node_id()
        .expect("document element should exist");
    let body_node_id = native_dom.body_node_id().expect("body should exist");

    let html_node = native_dom
        .node(html_node_id)
        .expect("html node should exist");
    let body_node = native_dom
        .node(body_node_id)
        .expect("body node should exist");
    assert_eq!(html_node.parent_node(), Some(native_dom.document_node_id()));
    assert_eq!(
        body_node.owner_document(),
        Some(native_dom.document_node_id())
    );
    assert!(body_node.flags().in_document_tree());

    let body_children = native_dom.child_ids(body_node_id).collect::<Vec<_>>();
    assert_eq!(body_children.len(), 1);
    let div_node = native_dom
        .node(body_children[0])
        .expect("div node should exist");
    assert_eq!(
        div_node
            .as_element()
            .and_then(|element| element.attribute("id")),
        Some("alpha")
    );

    let div_children = native_dom.child_ids(body_children[0]).collect::<Vec<_>>();
    assert_eq!(div_children.len(), 2);
    let first_span = native_dom
        .node(div_children[0])
        .expect("first span should exist");
    let second_span = native_dom
        .node(div_children[1])
        .expect("second span should exist");
    assert_eq!(first_span.next_sibling(), Some(div_children[1]));
    assert_eq!(second_span.prev_sibling(), Some(div_children[0]));
}

#[test]
fn native_dom_preserves_whitespace_text_siblings_inside_elements() {
    let native_dom = parse_fixture(
        "<!doctype html><html><body><div id='parent'>\n  <p id='c1'>1</p>\n  <p id='c2'>2</p>\n  <p id='c3'>3</p>\n</div></body></html>",
    );

    let parent = native_dom
        .nodes()
        .iter()
        .find_map(|node| {
            node.as_element().and_then(|element| {
                (element.attribute("id") == Some("parent")).then_some(node.id())
            })
        })
        .expect("parent element should exist");
    let first_child = native_dom
        .node(parent)
        .and_then(|node| node.first_child())
        .expect("parent should have first child");
    assert_eq!(
        native_dom.node(first_child).map(|node| node.node_name()),
        Some("#text".to_owned())
    );

    let first_element = native_dom
        .node(first_child)
        .and_then(|node| node.next_sibling())
        .expect("whitespace node should be followed by first element");
    assert_eq!(
        native_dom
            .node(first_element)
            .and_then(|node| node.as_element())
            .and_then(|element| element.attribute("id")),
        Some("c1")
    );
    let previous = native_dom
        .node(first_element)
        .and_then(|node| node.prev_sibling())
        .expect("first element should have previous text sibling");
    assert_eq!(previous, first_child);
}

#[test]
fn native_dom_keeps_template_contents_out_of_document_tree() {
    let native_dom = parse_fixture(
        "<!doctype html><html><body><template id='tpl'><span>ghost</span></template></body></html>",
    );

    let template_node_id = native_dom
        .nodes()
        .iter()
        .find_map(|node| {
            node.kind()
                .as_element()
                .and_then(|element| (element.attribute("id") == Some("tpl")).then_some(node.id()))
        })
        .expect("template node should exist");
    let template_element = native_dom
        .node(template_node_id)
        .and_then(|node| node.as_element())
        .expect("template node should be an element");
    let fragment_id = template_element
        .template_contents()
        .expect("template contents fragment should exist");
    let fragment_node = native_dom
        .node(fragment_id)
        .expect("fragment node should exist");

    assert_eq!(fragment_node.node_type(), NodeType::DocumentFragment);
    assert!(!fragment_node.flags().in_document_tree());
    assert_eq!(fragment_node.parent_node(), None);
    let fragment_owner_document = fragment_node
        .owner_document()
        .expect("template contents should have an owner document");
    assert_eq!(
        fragment_owner_document,
        native_dom
            .node(
                native_dom
                    .nth_child(fragment_id, 0)
                    .expect("template child")
            )
            .and_then(|node| node.owner_document())
            .expect("template child should have an owner document")
    );
    assert_ne!(fragment_owner_document, native_dom.document_node_id());
    assert_eq!(native_dom.child_ids(fragment_id).count(), 1);
}

#[test]
fn native_dom_treats_noscript_contents_as_raw_text_when_scripting_is_enabled() {
    let native_dom = parse_fixture(
        "<!doctype html><html><body><noscript id='ns'><h1>Hello</h1><p>World</p></noscript></body></html>",
    );

    let noscript = native_dom
        .nodes()
        .iter()
        .find_map(|node| {
            node.kind()
                .as_element()
                .and_then(|element| (element.attribute("id") == Some("ns")).then_some(node.id()))
        })
        .expect("noscript element should exist");
    let children = native_dom.child_ids(noscript).collect::<Vec<_>>();
    assert_eq!(
        children.len(),
        1,
        "noscript should have a single raw text child"
    );
    let text = native_dom
        .node(children[0])
        .and_then(|node| node.as_text())
        .expect("noscript child should be text");
    assert_eq!(text.data(), "<h1>Hello</h1><p>World</p>");
    assert_eq!(
        native_dom.outer_html(noscript).as_deref(),
        Some("<noscript id=\"ns\"><h1>Hello</h1><p>World</p></noscript>")
    );
}

#[test]
fn native_dom_node_ids_can_be_interned_by_reflector_registry() {
    let native_dom = parse_fixture("<!doctype html><html><body><p>ok</p></body></html>");
    let body_node_id = native_dom.body_node_id().expect("body should exist");

    let mut registry = ReflectorRegistry::default();
    let first = registry.intern(body_node_id);
    let second = registry.intern(body_node_id);

    assert_eq!(first.id(), second.id());
    assert_eq!(registry.len(), 1);
}

#[test]
fn native_dom_exposes_node_traversal_and_equality_helpers() {
    let native_dom = parse_fixture(
        "<!doctype html><html><body><div id='root'><span>one</span><!--gap--><span>one</span></div></body></html>",
    );
    let body = native_dom.body_node_id().expect("body should exist");
    let root = native_dom
        .find_child(body, |node_id| {
            native_dom.node(node_id).is_some_and(|node| {
                node.as_element()
                    .and_then(|element| element.attribute("id"))
                    == Some("root")
            })
        })
        .expect("root should exist");
    let children = native_dom.child_ids(root).collect::<Vec<_>>();
    let alpha = children[0];
    let gap = children[1];
    let beta = children[2];

    let root_node = native_dom.node(root).expect("root node should exist");
    let alpha_node = native_dom.node(alpha).expect("alpha node should exist");
    let gap_node = native_dom.node(gap).expect("comment node should exist");
    let beta_node = native_dom.node(beta).expect("beta node should exist");

    assert!(root_node.has_child_nodes());
    assert_eq!(root_node.child_element_count(&native_dom), 2);
    assert_eq!(root_node.first_element_child(&native_dom), Some(alpha));
    assert_eq!(root_node.last_element_child(&native_dom), Some(beta));
    assert_eq!(alpha_node.next_element_sibling(&native_dom), Some(beta));
    assert_eq!(beta_node.previous_element_sibling(&native_dom), Some(alpha));
    assert_eq!(gap_node.node_value(), Some("gap"));
    assert!(alpha_node.is_equal_node(&native_dom, beta_node));
    assert!(!root_node.is_equal_node(&native_dom, alpha_node));
}
