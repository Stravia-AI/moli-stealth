use super::insertion_plan::TreeInsertionPlan;
use crate::{
    custom_elements,
    document_runtime::{DocumentRuntime, DomHandle},
    dom::native::{DomHost, Node},
    native_bridge::JsContextHost,
};

#[derive(Clone, Debug, Default)]
pub(in crate::document_runtime) struct TreeAdoptionPlan {
    root: Option<DomHandle>,
    previous_owner_document: Option<DomHandle>,
    new_document: Option<DomHandle>,
    roots_with_owner_document_change: Vec<DomHandle>,
    custom_elements: custom_elements::CustomElementAdoptionPlan,
}

impl TreeAdoptionPlan {
    pub(in crate::document_runtime) fn before_adoption(
        dom_host: &DomHost,
        host_ptr: *mut JsContextHost,
        roots: &[DomHandle],
        new_document: DomHandle,
        collect_custom_elements: bool,
    ) -> Self {
        let root = roots.first().copied();
        let previous_owner_document =
            root.and_then(|root| dom_host.node(root).and_then(Node::owner_document));
        let roots_with_owner_document_change = roots
            .iter()
            .copied()
            .filter(|root| {
                dom_host
                    .node(*root)
                    .and_then(Node::owner_document)
                    .is_some_and(|previous| previous != new_document)
            })
            .collect();
        let custom_elements = if collect_custom_elements {
            custom_elements::adoption_plan_for_roots_before_adoption(host_ptr, roots, new_document)
        } else {
            custom_elements::CustomElementAdoptionPlan::default()
        };
        Self {
            root,
            previous_owner_document,
            new_document: Some(new_document),
            roots_with_owner_document_change,
            custom_elements,
        }
    }

    pub(in crate::document_runtime) fn root_with_previous_owner_document(
        &self,
    ) -> Option<(DomHandle, Option<DomHandle>)> {
        self.root.map(|root| (root, self.previous_owner_document))
    }

    pub(super) fn has_targets(&self) -> bool {
        self.custom_elements.has_targets()
    }

    pub(super) fn has_registry_retargets_without_adoption(&self) -> bool {
        self.custom_elements
            .has_registry_retargets_without_adoption()
    }

    pub(in crate::document_runtime) fn custom_elements(
        &self,
    ) -> &custom_elements::CustomElementAdoptionPlan {
        &self.custom_elements
    }

    pub(in crate::document_runtime) fn roots_with_owner_document_change(&self) -> &[DomHandle] {
        &self.roots_with_owner_document_change
    }

    pub(in crate::document_runtime) fn new_document(&self) -> Option<DomHandle> {
        self.new_document
    }
}

impl DocumentRuntime {
    pub(super) fn tree_adoption_plan_before_insert(
        &self,
        host_ptr: *mut JsContextHost,
        roots: &[DomHandle],
        parent: DomHandle,
    ) -> TreeAdoptionPlan {
        let Some(new_document) = self.document_for_insertion_parent(parent) else {
            return TreeAdoptionPlan::default();
        };
        TreeAdoptionPlan::before_adoption(&self.dom_host, host_ptr, roots, new_document, true)
    }

    fn document_for_insertion_parent(&self, parent: DomHandle) -> Option<DomHandle> {
        if self.dom_host.node(parent).is_some_and(Node::is_document) {
            return Some(parent);
        }
        self.dom_host.owner_document_handle(parent)
    }

    pub(super) fn sync_shadow_root_adopted_style_sheets_after_insertion_adoption(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        host_ptr: *mut JsContextHost,
        insertion_plan: &TreeInsertionPlan<'_>,
    ) {
        let Some(new_document) = insertion_plan.adoption.new_document() else {
            return;
        };
        if unsafe { &*host_ptr }
            .child_browsing_context_host_for_document_handle(new_document)
            .is_none()
        {
            return;
        }
        let runtime = unsafe { &mut *host_ptr };
        for &root in insertion_plan.adoption.roots_with_owner_document_change() {
            for shadow_root in runtime.shadow_roots_in_subtree(root) {
                crate::native_bridge::element::clear_shadow_root_adopted_style_sheets(
                    scope,
                    runtime,
                    shadow_root,
                );
            }
            runtime.note_style_subtree_context_change(root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::native::NativeDom;
    use url::Url;

    #[test]
    fn adoption_plan_records_only_roots_that_change_owner_document() {
        let mut dom_host = DomHost::from_dom(NativeDom::new(
            Url::parse("https://example.test/").expect("test URL parses"),
        ));
        let target_document = dom_host.document_handle();
        let other_document = dom_host.create_detached_html_document();
        let same_document_root = dom_host.create_parser_element_without_attributes_for_document(
            target_document,
            "div".to_owned(),
            "http://www.w3.org/1999/xhtml".to_owned(),
            None,
        );
        let other_document_root = dom_host.create_parser_element_without_attributes_for_document(
            other_document,
            "span".to_owned(),
            "http://www.w3.org/1999/xhtml".to_owned(),
            None,
        );

        let plan = TreeAdoptionPlan::before_adoption(
            &dom_host,
            std::ptr::null_mut(),
            &[same_document_root, other_document_root],
            target_document,
            false,
        );

        assert_eq!(
            plan.root_with_previous_owner_document(),
            Some((same_document_root, Some(target_document)))
        );
        assert_eq!(
            plan.roots_with_owner_document_change(),
            &[other_document_root]
        );
    }
}
