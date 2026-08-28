//! Document-root font state shared between Stylo cascades and Device changes.

use moli_selector::StyloDomStyleAdapter;
use style::{
    computed_value_flags::ComputedValueFlags, device::Device, dom::TElement,
    properties::ComputedValues, servo_arc::Arc as ServoArc,
};

use crate::{document_runtime::DomHandle, dom::native::DomHost};

/// Seed a replacement Device from the root style retained by the current
/// document style world.
///
/// Stylo updates root font-relative state while finishing a root restyle. A
/// Device replacement does not restyle the root by itself, so it must inherit
/// the last published root bases before any newly resolved descendant can use
/// `rem`, `rlh`, or root font-metric units.
pub(super) fn initialize_from_retained_document_root(
    device: &Device,
    host: &DomHost,
    dom_adapter: &StyloDomStyleAdapter,
    document: DomHandle,
) {
    let Some(root) = host.document_element_handle_for_document(document) else {
        return;
    };
    dom_adapter.with_bound_host(host, |binding| {
        let Some(element) = binding.element(host, root) else {
            return;
        };
        let style = element
            .borrow_data()
            .and_then(|data| data.styles.get_primary().cloned());
        let Some(style) = style else {
            return;
        };
        set_root_font_relative_state(device, &style);
    });
}

/// Publish a freshly resolved document-root style to the current Device.
///
/// Moli resolves an exact ancestor chain through Stylo's single-node
/// `resolve_style` API. That API does not run Stylo's `finish_restyle`, so the
/// root-to-descendant state transition is made explicitly at the same boundary
/// before resolving the next node in the chain.
pub(super) fn publish_resolved_root_style(device: &Device, style: &ServoArc<ComputedValues>) {
    if !style
        .flags
        .contains(ComputedValueFlags::IS_ROOT_ELEMENT_STYLE)
    {
        return;
    }

    set_root_font_relative_state(device, style);
    if device.used_root_font_metrics() {
        device.update_root_font_metrics();
    }
}

fn set_root_font_relative_state(device: &Device, style: &ServoArc<ComputedValues>) {
    device.set_root_style(style);

    let font = style.get_font();
    let font_size = font.clone_font_size().computed_size();
    device.set_root_font_size(style.effective_zoom.unzoom(font_size.px()));

    let line_height = device.calc_line_height(font, style.writing_mode, None).0;
    device.set_root_line_height(style.effective_zoom.unzoom(line_height.px()));
}
