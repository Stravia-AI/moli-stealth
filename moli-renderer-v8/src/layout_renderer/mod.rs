mod display_lock;
mod inline_svg;
mod remembered_size;
mod source_view;
mod style_resolver;

pub(crate) use display_lock::AutoDisplayLockState;
pub(crate) use remembered_size::IntrinsicSizeObserverState;
pub(crate) use source_view::native_element_bypasses_display_lock_display_type_check;

use std::{collections::HashMap, time::Duration};

use moli_layout::{
    DocumentLayoutServices, EmbeddedFrameRenderer, LayoutPassRequest, LayoutPassResult,
    LayoutViewport, build_layout_pass_with_embedded_frames,
};

use crate::{document_runtime::DomHandle, native_bridge::JsContextHost};

pub(crate) fn current_native_stylesheet_resources(
    runtime: &JsContextHost,
    root: DomHandle,
) -> Option<crate::style_engine::StylesheetResourceSnapshot> {
    let mut reads = crate::native_bridge::element::StyleObservation::new(runtime);
    reads.read(root).stylesheet_resource_snapshot()
}

pub(crate) fn build_native_layout_pass(
    runtime: &JsContextHost,
    root: DomHandle,
    services: &mut DocumentLayoutServices,
    embedded_document_services: &mut HashMap<DomHandle, DocumentLayoutServices>,
    auto_display_locks: &mut AutoDisplayLockState,
    intrinsic_size_observer: &mut IntrinsicSizeObserverState,
    request: LayoutPassRequest,
) -> Result<LayoutPassResult<DomHandle>, moli_layout::LayoutError> {
    let mut working_auto_display_locks = auto_display_locks.clone();
    working_auto_display_locks.retain_connected(runtime);
    let mut working_intrinsic_size_observer = intrinsic_size_observer.clone();
    working_intrinsic_size_observer.retain_connected(runtime);
    let mut delivered_display_lock_observations = std::collections::HashSet::new();
    let mut total_elapsed = Duration::ZERO;

    loop {
        // Every document and embedded frame in this epoch consumes one
        // immutable lock snapshot. Post-layout observations update the
        // working state only for the next epoch, so sibling frames cannot
        // observe a partially advanced lifecycle.
        let display_lock_snapshot = working_auto_display_locks.clone();
        let (result, display_lock_changed) = {
            let mut epoch = NativeLayoutEpoch {
                runtime,
                embedded_document_services,
                display_lock_snapshot: &display_lock_snapshot,
                auto_display_locks: &mut working_auto_display_locks,
                intrinsic_size_observer: &mut working_intrinsic_size_observer,
                delivered_display_lock_observations: &mut delivered_display_lock_observations,
                document_stack: Vec::new(),
                display_lock_changed: false,
            };
            let result = build_native_layout_pass_recursive(&mut epoch, root, services, request);
            (result, epoch.display_lock_changed)
        };
        let mut pass = result?;
        total_elapsed = total_elapsed.saturating_add(pass.metrics.elapsed);
        if display_lock_changed {
            continue;
        }

        pass.metrics.elapsed = total_elapsed;
        *auto_display_locks = working_auto_display_locks;
        *intrinsic_size_observer = working_intrinsic_size_observer;
        return Ok(pass);
    }
}

/// Mutable state shared by every document participating in one atomic layout
/// epoch. The immutable lock snapshot is the sole input to box construction;
/// observations accumulate separately and are visible only to the next epoch.
struct NativeLayoutEpoch<'a> {
    runtime: &'a JsContextHost,
    embedded_document_services: &'a mut HashMap<DomHandle, DocumentLayoutServices>,
    display_lock_snapshot: &'a AutoDisplayLockState,
    auto_display_locks: &'a mut AutoDisplayLockState,
    intrinsic_size_observer: &'a mut IntrinsicSizeObserverState,
    delivered_display_lock_observations: &'a mut std::collections::HashSet<DomHandle>,
    document_stack: Vec<DomHandle>,
    display_lock_changed: bool,
}

fn build_native_layout_pass_recursive(
    epoch: &mut NativeLayoutEpoch<'_>,
    root: DomHandle,
    services: &mut DocumentLayoutServices,
    request: LayoutPassRequest,
) -> Result<LayoutPassResult<DomHandle>, moli_layout::LayoutError> {
    let runtime = epoch.runtime;
    let document = runtime
        .dom_host()
        .owner_document_handle(root)
        .unwrap_or_else(|| runtime.document_handle());
    style_resolver::prepare_layout_style_inputs(runtime, root, document, request.viewport);
    style_resolver::reconcile_remembered_size_policies(
        runtime,
        document,
        request.viewport,
        epoch.intrinsic_size_observer,
    );
    epoch.document_stack.push(document);
    let source = source_view::NativeLayoutSourceView::with_paint_resources(
        runtime,
        root,
        request.requests_paint(),
    );
    let mut styles = style_resolver::NativeLayoutStyleResolver::new(
        runtime,
        root,
        document,
        request.viewport,
        epoch.display_lock_snapshot,
        epoch.intrinsic_size_observer,
    );
    let result = {
        let mut frames = NativeEmbeddedFrameRenderer {
            epoch,
            reason: request.reason,
            capture_paint: request.requests_paint(),
            include_backgrounds: request.includes_backgrounds(),
        };
        build_layout_pass_with_embedded_frames(&source, &mut styles, services, request, &mut frames)
    };
    let policies = styles.into_pass_policies();
    if let Ok(pass) = &result {
        epoch
            .intrinsic_size_observer
            .observe_layout(&pass.tree, &policies.remembered_sizes);
        epoch.display_lock_changed |= epoch.auto_display_locks.observe_layout(
            runtime,
            root,
            document,
            &pass.tree,
            &policies.display_locks,
            epoch.delivered_display_lock_observations,
        );
    }
    epoch.document_stack.pop();
    result
}

struct NativeEmbeddedFrameRenderer<'epoch, 'state> {
    epoch: &'epoch mut NativeLayoutEpoch<'state>,
    reason: moli_layout::LayoutFlushReason,
    capture_paint: bool,
    include_backgrounds: bool,
}

impl EmbeddedFrameRenderer<DomHandle> for NativeEmbeddedFrameRenderer<'_, '_> {
    fn render_embedded_frame(
        &mut self,
        frame: DomHandle,
        viewport: LayoutViewport,
    ) -> Result<Option<moli_layout::EmbeddedFrameSnapshot<DomHandle>>, moli_layout::LayoutError>
    {
        const MAX_EMBEDDED_DOCUMENT_DEPTH: usize = 32;

        let Some(document) = self
            .epoch
            .runtime
            .child_browsing_context_document_handle(frame)
        else {
            return Ok(None);
        };
        if self.epoch.document_stack.len() >= MAX_EMBEDDED_DOCUMENT_DEPTH
            || self.epoch.document_stack.contains(&document)
        {
            return Ok(None);
        }
        let Some(root) = self
            .epoch
            .runtime
            .dom_host()
            .dom()
            .document_element_handle_for_document(document)
        else {
            return Ok(None);
        };
        let mut services = self
            .epoch
            .embedded_document_services
            .remove(&document)
            .unwrap_or_default();
        let request = if self.capture_paint {
            let mut capture = moli_layout::PaintCaptureRequest::viewport();
            capture.include_backgrounds = self.include_backgrounds;
            capture.base_background_color = moli_layout::PaintColor::TRANSPARENT;
            LayoutPassRequest::with_capture(viewport, self.reason, capture)
        } else {
            LayoutPassRequest::new(viewport, self.reason)
        };
        let result = build_native_layout_pass_recursive(self.epoch, root, &mut services, request);
        self.epoch
            .embedded_document_services
            .insert(document, services);
        let output = result?;
        let (tree, paint, css_image_references) = output.into_embedded_parts();
        Ok(Some(moli_layout::EmbeddedFrameSnapshot::new(
            tree,
            paint,
            css_image_references,
        )))
    }
}

#[cfg(test)]
pub(crate) fn build_normalized_native_box_tree_for_test(
    runtime: &JsContextHost,
    root: DomHandle,
) -> Result<moli_layout::NormalizedBoxTree, moli_layout::LayoutError> {
    let document = runtime
        .dom_host()
        .owner_document_handle(root)
        .unwrap_or_else(|| runtime.document_handle());
    let viewport = runtime.layout_viewport_for_document(document);
    let source = source_view::NativeLayoutSourceView::new(runtime, root);
    let remembered_sizes = IntrinsicSizeObserverState::default();
    let display_locks = AutoDisplayLockState::default();
    let mut styles = style_resolver::NativeLayoutStyleResolver::new(
        runtime,
        root,
        document,
        viewport,
        &display_locks,
        &remembered_sizes,
    );
    moli_layout::build_layout_world(&source, &mut styles).map(|world| world.normalized_tree())
}
