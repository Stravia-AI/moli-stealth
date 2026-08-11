use crate::{
    document_runtime::{DocumentRuntime, DomHandle},
    dom::native::Node,
};

impl DocumentRuntime {
    pub(in crate::document_runtime) fn is_ancestor_of(
        &self,
        ancestor: DomHandle,
        node: DomHandle,
    ) -> bool {
        let mut current = Some(node);
        while let Some(handle) = current {
            if handle == ancestor {
                return true;
            }
            current = self.dom_host.node(handle).and_then(Node::parent_node);
        }
        false
    }

    #[cfg(test)]
    pub(in crate::document_runtime) fn precedes_in_document_order(
        &self,
        left: DomHandle,
        right: DomHandle,
    ) -> bool {
        if left == right {
            return false;
        }

        let right_chain = self.ancestor_chain(right);
        self.precedes_in_document_order_against_ancestor_chain(left, &right_chain)
    }

    #[cfg(test)]
    pub(in crate::document_runtime) fn precedes_in_document_order_against_ancestor_chain(
        &self,
        left: DomHandle,
        right_chain: &[DomHandle],
    ) -> bool {
        let left_chain = self.ancestor_chain(left);
        let mut diverge = 0usize;
        while diverge < left_chain.len()
            && diverge < right_chain.len()
            && left_chain[diverge] == right_chain[diverge]
        {
            diverge += 1;
        }

        if diverge == left_chain.len() || diverge == right_chain.len() {
            return left_chain.len() < right_chain.len();
        }

        if diverge == 0 {
            return false;
        }

        let Some(parent) = left_chain.get(diverge - 1).copied() else {
            return false;
        };
        let left_child = left_chain[diverge];
        let right_child = right_chain[diverge];
        for child in self.dom_host.child_handles(parent) {
            if child == left_child {
                return true;
            }
            if child == right_child {
                return false;
            }
        }
        false
    }

    #[cfg(test)]
    pub(in crate::document_runtime) fn ancestor_chain(&self, node: DomHandle) -> Vec<DomHandle> {
        let mut chain = Vec::new();
        let mut current = Some(node);
        while let Some(handle) = current {
            chain.push(handle);
            current = self.dom_host.node(handle).and_then(Node::parent_node);
        }
        chain.reverse();
        chain
    }
}
