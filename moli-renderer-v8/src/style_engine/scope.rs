use dom::ElementState as StyloElementState;
use moli_selector::StyloStyleSourceScope as StyleSourceScope;

use crate::{document_runtime::DomHandle, dom::native::DomHost};

use super::StyleMutationEffect;

pub(super) fn source_scope_for_mutations(
    host: &DomHost,
    effects: &[StyleMutationEffect],
) -> Option<StyleSourceScope> {
    mutation_effects_have_source_scope(effects)
        .then(|| style_source_scope_for_mutation_effects(host, effects))
}

pub(super) fn mutation_effects_have_source_scope(effects: &[StyleMutationEffect]) -> bool {
    !effects.is_empty()
        && effects
            .iter()
            .any(|effect| !matches!(effect, StyleMutationEffect::DisconnectedSubtrees { .. }))
}

pub(super) fn source_scope_for_element_state_change(
    host: &DomHost,
    element: DomHandle,
    state: StyloElementState,
) -> Option<StyleSourceScope> {
    if state.is_empty() {
        return None;
    }
    Some(StyleSourceScope::for_handle(host, element))
}

pub(super) fn source_scope_for_custom_state_change(
    host: &DomHost,
    element: DomHandle,
    state_names: &[String],
) -> Option<StyleSourceScope> {
    if state_names.iter().all(String::is_empty) {
        return None;
    }
    Some(StyleSourceScope::for_handle(host, element))
}

pub(super) fn style_source_scope_for_mutation_effects(
    host: &DomHost,
    effects: &[StyleMutationEffect],
) -> StyleSourceScope {
    let mut handles = Vec::new();
    for effect in effects {
        match effect {
            StyleMutationEffect::Attribute { element, .. } => {
                handles.push(*element);
            }
            StyleMutationEffect::ConnectedSubtrees { roots }
            | StyleMutationEffect::DisconnectedSubtrees { roots } => {
                handles.extend(roots.iter().copied());
            }
            StyleMutationEffect::CharacterData { node } => {
                handles.push(*node);
            }
            StyleMutationEffect::SlotAssignment { slot, .. } => {
                handles.push(*slot);
            }
            StyleMutationEffect::ChildList {
                parent,
                added_nodes,
                removed_nodes,
                removed_element_snapshots: _,
                previous_sibling,
                next_sibling,
            } => {
                handles.push(*parent);
                for handle in added_nodes.iter().chain(removed_nodes) {
                    handles.push(*handle);
                }
                if let Some(previous_sibling) = previous_sibling {
                    handles.push(*previous_sibling);
                }
                if let Some(next_sibling) = next_sibling {
                    handles.push(*next_sibling);
                }
            }
        }
    }
    StyleSourceScope::for_handles(host, handles)
}
