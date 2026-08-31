use super::insertion_plan::TreeInsertionPlan;
use crate::{
    custom_elements,
    document_runtime::{DocumentRuntime, DomHandle},
    dom::native::DomHost,
    native_bridge::JsContextHost,
};

#[derive(Clone, Debug, Default)]
pub(in crate::document_runtime) struct TreeAdoptionPlan {
    transition: Option<TreeDocumentTransition>,
    custom_elements: custom_elements::CustomElementAdoptionPlan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TreeDocumentTransition {
    previous_document: DomHandle,
    new_document: DomHandle,
}

impl TreeDocumentTransition {
    fn documents(self) -> (DomHandle, DomHandle) {
        (self.previous_document, self.new_document)
    }

    fn crosses_documents(self) -> bool {
        self.previous_document != self.new_document
    }

    fn cross_document(self) -> Option<(DomHandle, DomHandle)> {
        self.crosses_documents().then_some(self.documents())
    }

    fn for_root(dom_host: &DomHost, root: DomHandle, new_document: DomHandle) -> Option<Self> {
        Some(Self {
            previous_document: dom_host.owner_document_handle(root)?,
            new_document,
        })
    }
}

impl TreeAdoptionPlan {
    pub(in crate::document_runtime) fn before_standalone_adoption(
        dom_host: &DomHost,
        host_ptr: *mut JsContextHost,
        root: DomHandle,
        new_document: DomHandle,
    ) -> Self {
        Self::before_adoption(
            dom_host,
            host_ptr,
            std::slice::from_ref(&root),
            new_document,
        )
    }

    fn before_adoption(
        dom_host: &DomHost,
        host_ptr: *mut JsContextHost,
        roots: &[DomHandle],
        new_document: DomHandle,
    ) -> Self {
        // The roots of one DOM insertion share a node document, including
        // children hoisted from a DocumentFragment. One lookup classifies the
        // whole batch.
        let transition = roots
            .first()
            .and_then(|root| TreeDocumentTransition::for_root(dom_host, *root, new_document));
        let custom_elements = transition.map_or_else(Default::default, |transition| {
            custom_elements::adoption_plan_for_roots_before_adoption(
                host_ptr,
                roots,
                new_document,
                transition.crosses_documents(),
            )
        });
        Self {
            transition,
            custom_elements,
        }
    }

    pub(in crate::document_runtime) fn documents(&self) -> Option<(DomHandle, DomHandle)> {
        self.transition.map(TreeDocumentTransition::documents)
    }

    pub(super) fn crosses_documents(&self) -> bool {
        self.transition
            .is_some_and(TreeDocumentTransition::crosses_documents)
    }

    fn cross_document(&self) -> Option<(DomHandle, DomHandle)> {
        self.transition?.cross_document()
    }

    pub(in crate::document_runtime) fn custom_elements(
        &self,
    ) -> &custom_elements::CustomElementAdoptionPlan {
        &self.custom_elements
    }
}

impl DocumentRuntime {
    pub(super) fn tree_adoption_plan_before_insert(
        &self,
        host_ptr: *mut JsContextHost,
        roots: &[DomHandle],
        parent: DomHandle,
    ) -> TreeAdoptionPlan {
        let Some(new_document) = self.dom_host.owner_document_handle(parent) else {
            return TreeAdoptionPlan::default();
        };
        TreeAdoptionPlan::before_adoption(&self.dom_host, host_ptr, roots, new_document)
    }

    pub(super) fn sync_shadow_root_adopted_style_sheets_after_insertion_adoption(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        host_ptr: *mut JsContextHost,
        insertion_plan: &TreeInsertionPlan<'_>,
    ) {
        let Some((_, new_document)) = insertion_plan.adoption.cross_document() else {
            return;
        };
        if unsafe { &*host_ptr }
            .child_browsing_context_host_for_document_handle(new_document)
            .is_none()
        {
            return;
        }
        let runtime = unsafe { &mut *host_ptr };
        for &root in insertion_plan.insertion_roots {
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
    fn fragment_insertion_roots_share_one_document_transition() {
        let mut dom_host = DomHost::from_dom(NativeDom::new(
            Url::parse("https://example.test/").expect("test URL parses"),
        ));
        let target_document = dom_host.document_handle();
        let other_document = dom_host.create_detached_html_document();
        let foreign_fragment = dom_host.create_document_fragment_for_document(other_document);
        let first_root = dom_host.create_parser_element_without_attributes_for_document(
            other_document,
            "div".to_owned(),
            "http://www.w3.org/1999/xhtml".to_owned(),
            None,
        );
        let second_root = dom_host.create_parser_element_without_attributes_for_document(
            other_document,
            "span".to_owned(),
            "http://www.w3.org/1999/xhtml".to_owned(),
            None,
        );
        assert!(dom_host.append_child(foreign_fragment, first_root));
        assert!(dom_host.append_child(foreign_fragment, second_root));
        assert_eq!(
            dom_host.owner_document_handle(first_root),
            Some(other_document)
        );
        assert_eq!(
            dom_host.owner_document_handle(second_root),
            Some(other_document)
        );

        // A DocumentFragment adopts each appended child into its own node
        // document, so either root classifies the whole insertion batch.
        assert_eq!(
            TreeDocumentTransition::for_root(&dom_host, first_root, target_document),
            Some(TreeDocumentTransition {
                previous_document: other_document,
                new_document: target_document,
            })
        );
        assert_eq!(
            TreeDocumentTransition::for_root(&dom_host, first_root, other_document),
            Some(TreeDocumentTransition {
                previous_document: other_document,
                new_document: other_document,
            })
        );
    }
}
