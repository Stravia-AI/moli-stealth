use super::NativeDom;
use super::node::{NativeNodeId, Node};

/// A bounded serialization stopped before appending bytes past its limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HtmlSerializationLimitExceeded {
    pub max_bytes: usize,
}

impl std::fmt::Display for HtmlSerializationLimitExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "serialized HTML exceeds the {}-byte output limit",
            self.max_bytes
        )
    }
}

impl std::error::Error for HtmlSerializationLimitExceeded {}

pub(super) trait HtmlSerializationSink {
    fn push_str(&mut self, value: &str);
    fn push(&mut self, value: char);
    fn limit_exceeded(&self) -> bool;
}

impl HtmlSerializationSink for String {
    fn push_str(&mut self, value: &str) {
        String::push_str(self, value);
    }

    fn push(&mut self, value: char) {
        String::push(self, value);
    }

    fn limit_exceeded(&self) -> bool {
        false
    }
}

pub(super) fn escape_html_text<S>(value: &str, out: &mut S)
where
    S: HtmlSerializationSink + ?Sized,
{
    for ch in value.chars() {
        if out.limit_exceeded() {
            return;
        }
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\u{00A0}' => out.push_str("&nbsp;"),
            _ => out.push(ch),
        }
    }
}

pub(super) fn escape_html_attribute<S>(value: &str, out: &mut S)
where
    S: HtmlSerializationSink + ?Sized,
{
    for ch in value.chars() {
        if out.limit_exceeded() {
            return;
        }
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

pub(super) fn serialize_cdata_section<S>(
    value: &str,
    out: &mut S,
    raw_text_parent: bool,
    html_document: bool,
) where
    S: HtmlSerializationSink + ?Sized,
{
    if html_document {
        if raw_text_parent {
            out.push_str(value);
        } else {
            escape_html_text(value, out);
        }
    } else {
        out.push_str("<![CDATA[");
        out.push_str(value);
        out.push_str("]]>");
    }
}

pub(super) fn is_void_html_element(namespace: &str, local_name: &str) -> bool {
    namespace == "http://www.w3.org/1999/xhtml"
        && matches!(
            local_name,
            "area"
                | "base"
                | "basefont"
                | "bgsound"
                | "br"
                | "col"
                | "embed"
                | "frame"
                | "hr"
                | "img"
                | "input"
                | "keygen"
                | "link"
                | "meta"
                | "param"
                | "source"
                | "track"
                | "wbr"
        )
}

impl NativeDom {
    pub fn serialize_document(&self) -> String {
        let mut html = String::new();
        for child in self.child_ids(self.document_node_id) {
            if let Some(child) = self.node(child) {
                child.serialize_into(self, &mut html, false);
            }
        }
        html
    }

    pub fn is_html_element_named(&self, node_id: NativeNodeId, local_name: &str) -> bool {
        self.node(node_id)
            .is_some_and(|node| node.is_html_element_named(local_name))
    }

    pub fn option_value(&self, node_id: NativeNodeId) -> Option<String> {
        let element = self.node(node_id).and_then(Node::as_element)?;
        if !element.is_html_option() {
            return None;
        }
        Some(element.option_value(self, node_id))
    }

    pub fn outer_html(&self, node_id: NativeNodeId) -> Option<String> {
        let node = self.node(node_id)?;
        let mut html = String::new();
        node.serialize_into(self, &mut html, false);
        Some(html)
    }

    /// Serializes one subtree without ever growing the output beyond `max_bytes`.
    ///
    /// This is intended for bounded derived consumers such as a fresh inline
    /// SVG image parse. Web-exposed `outerHTML` continues to use the unbounded
    /// serializer because truncation would violate its string contract.
    pub fn outer_html_with_limit(
        &self,
        node_id: NativeNodeId,
        max_bytes: usize,
    ) -> Result<Option<String>, HtmlSerializationLimitExceeded> {
        let Some(node) = self.node(node_id) else {
            return Ok(None);
        };
        node.serialize_with_limit(self, false, max_bytes).map(Some)
    }

    pub fn inner_html(&self, node_id: NativeNodeId) -> Option<String> {
        let node = self.node(node_id)?;
        if node
            .as_element()
            .is_some_and(|element| is_void_html_element(element.namespace(), element.local_name()))
        {
            return Some(String::new());
        }
        let mut html = String::new();
        let raw_text_child = node.as_element().is_some_and(|element| {
            element.namespace() == "http://www.w3.org/1999/xhtml"
                && matches!(element.local_name(), "script" | "style" | "noscript")
        });
        if let Some(template_contents) = node
            .as_element()
            .and_then(|element| element.template_contents())
        {
            if let Some(fragment) = self.node(template_contents) {
                for child in fragment.child_ids(self) {
                    if let Some(child) = self.node(child) {
                        child.serialize_into(self, &mut html, raw_text_child);
                    }
                }
            }
        } else {
            for child in node.child_ids(self) {
                if let Some(child) = self.node(child) {
                    child.serialize_into(self, &mut html, raw_text_child);
                }
            }
        }
        Some(html)
    }

    pub fn script_handles(&self) -> Vec<NativeNodeId> {
        self.nodes
            .iter()
            .filter_map(|node| node.is_script_element().then_some(node.id()))
            .collect()
    }

    pub fn script_node_ids(&self) -> Vec<NativeNodeId> {
        self.script_handles()
    }

    pub fn document_order_script_handles(&self) -> Vec<NativeNodeId> {
        let mut script_handles = Vec::new();
        let mut stack = vec![self.document_node_id];
        while let Some(node_id) = stack.pop() {
            let Some(node) = self.node(node_id) else {
                continue;
            };
            if node.is_script_element() {
                script_handles.push(node_id);
            }
            stack.extend(self.child_ids_reversed(node_id));
        }
        script_handles
    }

    pub fn document_order_script_node_ids(&self) -> Vec<NativeNodeId> {
        self.document_order_script_handles()
    }

    pub fn script_src(&self, node_id: NativeNodeId) -> Option<&str> {
        self.node(node_id)?.as_element()?.script_source_attribute()
    }

    pub fn script_text(&self, node_id: NativeNodeId) -> Option<String> {
        let script_node = self.node(node_id)?;
        let element = script_node.as_element()?;
        if !element.is_script_element() {
            return None;
        }

        let mut script_text = String::new();
        for child_id in script_node.child_ids(self) {
            let Some(child) = self.node(child_id) else {
                continue;
            };

            if let Some(text) = child.as_text() {
                script_text.push_str(text.data());
            }
        }

        (!script_text.is_empty()).then_some(script_text)
    }

    pub fn push_parse_error(&mut self, error: String) {
        self.parse_errors.push(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::DomHost;

    fn test_url() -> url::Url {
        url::Url::parse("https://serialization.test/").expect("test URL")
    }

    #[test]
    fn html_serializers_share_the_complete_void_element_set() {
        let mut dom = NativeDom::new_html(test_url());
        let container = dom.create_element("div");
        let mut expected = String::new();
        let mut void_elements = Vec::new();
        for local_name in [
            "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "img",
            "input", "keygen", "link", "meta", "param", "source", "track", "wbr",
        ] {
            let element = dom.create_element(local_name);
            let ignored_child = dom.create_element("span");
            assert!(dom.append_child(element, ignored_child));
            assert_eq!(dom.outer_html(element), Some(format!("<{local_name}>")));
            assert_eq!(dom.inner_html(element).as_deref(), Some(""));
            assert!(dom.append_child(container, element));
            expected.push_str(&format!("<{local_name}>"));
            void_elements.push(element);
        }
        assert_eq!(
            dom.inner_html(container).as_deref(),
            Some(expected.as_str())
        );

        let foreign_param = dom
            .create_element_ns(Some("http://www.w3.org/2000/svg"), "param")
            .expect("SVG element");
        assert_eq!(
            dom.outer_html(foreign_param).as_deref(),
            Some("<param></param>")
        );

        let mut host = DomHost::from_dom(dom);
        let param = host.create_element("param");
        assert!(host.append_child(container, param));
        assert_eq!(
            host.get_html(container, false, &[]).as_deref(),
            Some(format!("{expected}<param>").as_str())
        );
        for element in void_elements {
            assert_eq!(host.get_html(element, false, &[]).as_deref(), Some(""));
        }
    }

    #[test]
    fn bounded_outer_html_stops_before_exceeding_the_output_limit() {
        let mut dom = NativeDom::new_html(test_url());
        let element = dom.create_element("div");
        let expected = "<div></div>";

        assert_eq!(
            dom.outer_html_with_limit(element, expected.len()),
            Ok(Some(expected.to_owned()))
        );
        assert_eq!(
            dom.outer_html_with_limit(element, expected.len() - 1),
            Err(HtmlSerializationLimitExceeded {
                max_bytes: expected.len() - 1,
            })
        );
    }

    #[test]
    fn html_serializers_apply_attribute_serialized_name_rules() {
        let mut dom = NativeDom::new_html(test_url());
        let container = dom.create_element("section");
        let element = dom
            .create_element_ns(Some("urn:element"), "div")
            .expect("namespaced element");
        assert!(dom.set_attribute_ns(
            element,
            Some("http://www.w3.org/XML/1998/namespace"),
            Some("alternate"),
            "lang",
            "en-us",
        ));
        assert!(dom.set_attribute_ns(
            element,
            Some("http://www.w3.org/2000/xmlns/"),
            None,
            "binding",
            "urn:binding",
        ));
        assert!(dom.set_attribute_ns(
            element,
            Some("http://www.w3.org/2000/xmlns/"),
            None,
            "xmlns",
            "urn:default",
        ));
        assert!(dom.set_attribute_ns(
            element,
            Some("http://www.w3.org/1999/xlink"),
            Some("alternate"),
            "href",
            "target",
        ));
        assert!(dom.set_attribute_ns(element, Some("urn:custom"), Some("p"), "attr", "value",));
        assert!(dom.append_child(container, element));

        let expected = concat!(
            "<div xml:lang=\"en-us\" xmlns:binding=\"urn:binding\" ",
            "xmlns=\"urn:default\" xlink:href=\"target\" p:attr=\"value\"></div>"
        );
        assert_eq!(dom.inner_html(container).as_deref(), Some(expected));

        let host = DomHost::from_dom(dom);
        assert_eq!(
            host.get_html(container, false, &[]).as_deref(),
            Some(expected)
        );
    }

    #[test]
    fn html_serializers_escape_adopted_cdata_as_text() {
        const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
        const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

        let mut xml_dom = NativeDom::new_xml(test_url());
        let xml_svg = xml_dom
            .create_element_ns(Some(SVG_NAMESPACE), "svg")
            .expect("XML SVG element");
        assert!(xml_dom.set_attribute_ns(
            xml_svg,
            Some(XMLNS_NAMESPACE),
            None,
            "xmlns",
            SVG_NAMESPACE,
        ));
        let xml_cdata = xml_dom.create_cdata_section("<img>&");
        assert!(xml_dom.append_child(xml_svg, xml_cdata));
        assert_eq!(
            xml_dom.outer_html(xml_svg).as_deref(),
            Some(r#"<svg xmlns="http://www.w3.org/2000/svg"><![CDATA[<img>&]]></svg>"#)
        );

        let mut html_dom = NativeDom::new_html(test_url());
        let html_svg = html_dom
            .create_element_ns(Some(SVG_NAMESPACE), "svg")
            .expect("HTML-document SVG element");
        assert!(html_dom.set_attribute_ns(
            html_svg,
            Some(XMLNS_NAMESPACE),
            None,
            "xmlns",
            SVG_NAMESPACE,
        ));
        let adopted_cdata = html_dom.create_cdata_section("<img>&");
        assert!(html_dom.append_child(html_svg, adopted_cdata));
        assert_eq!(
            html_dom.outer_html(html_svg).as_deref(),
            Some(r#"<svg xmlns="http://www.w3.org/2000/svg">&lt;img&gt;&amp;</svg>"#)
        );

        let host = DomHost::from_dom(html_dom);
        assert_eq!(
            host.get_html(html_svg, false, &[]).as_deref(),
            Some("&lt;img&gt;&amp;")
        );
    }
}
