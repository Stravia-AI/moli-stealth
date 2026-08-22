use moli_layout::{
    LayoutDisplay, LayoutError, LayoutPseudo, LayoutStyleResolver, LayoutViewport,
    ResolvedLayoutStyle,
};

use crate::{
    document_runtime::DomHandle,
    native_bridge::{JsContextHost, element::StyleObservation},
    style_engine::{StyleViewport, StyloAnonymousBoxKind},
};

pub(super) struct NativeLayoutStyleResolver<'a> {
    runtime: &'a JsContextHost,
    reads: StyleObservation<'a>,
    scripting_enabled: bool,
}

impl<'a> NativeLayoutStyleResolver<'a> {
    pub(super) fn new(
        runtime: &'a JsContextHost,
        root: DomHandle,
        document: DomHandle,
        viewport: LayoutViewport,
    ) -> Self {
        let mut reads = layout_style_read_scope(runtime, document, viewport);
        let _ = reads.read(root).computed_values();
        Self {
            runtime,
            reads,
            scripting_enabled: runtime.document_scripting_enabled(document),
        }
    }
}

fn layout_style_read_scope<'a>(
    runtime: &'a JsContextHost,
    document: DomHandle,
    viewport: LayoutViewport,
) -> StyleObservation<'a> {
    let viewport = layout_style_viewport(runtime, viewport);
    if document == runtime.document_handle() && viewport == runtime.style_viewport() {
        // Keep the normal main-document observation environment so layout and
        // rendered-text collection read the same persistent style world. Only
        // an override viewport or embedded Document needs an explicit context.
        StyleObservation::new(runtime)
    } else {
        StyleObservation::new_for_document_viewport(runtime, document, viewport)
    }
}

fn layout_style_viewport(runtime: &JsContextHost, viewport: LayoutViewport) -> StyleViewport {
    let screen = runtime.style_viewport();
    StyleViewport::new(
        Some(f64::from(viewport.css_width)),
        Some(f64::from(viewport.css_height)),
    )
    .with_screen_size(screen.screen_width, screen.screen_height)
}

impl LayoutStyleResolver<DomHandle> for NativeLayoutStyleResolver<'_> {
    fn primary_style(
        &mut self,
        node: DomHandle,
    ) -> Result<Option<ResolvedLayoutStyle>, LayoutError> {
        let read = self.reads.read(node);
        let Some(computed) = read.computed_values() else {
            return Ok(None);
        };
        let mut resolved = ResolvedLayoutStyle::from_stylo(computed);
        if self
            .runtime
            .dom_host()
            .get_attribute(node, "hidden")
            .is_some()
        {
            resolved.force_display_none();
        }
        if self.scripting_enabled
            && self
                .runtime
                .dom_host()
                .is_html_element_named(node, "noscript")
        {
            // Match Blink's HTMLNoScriptElement::LayoutObjectIsNeeded(): the
            // computed `display` remains observable, including author
            // overrides, but no layout object is generated while this exact
            // Document can execute scripts.
            resolved.force_display_none();
        }
        Ok(Some(resolved))
    }

    fn pseudo_style(
        &mut self,
        node: DomHandle,
        pseudo: LayoutPseudo,
    ) -> Result<Option<ResolvedLayoutStyle>, LayoutError> {
        let pseudo_name = pseudo_style_name(pseudo);
        let read = self.reads.read(node);
        let computed = read.pseudo_computed_values(pseudo_name);
        Ok(computed.map(ResolvedLayoutStyle::from_stylo))
    }

    fn anonymous_style(
        &mut self,
        owner: DomHandle,
        parent: &ResolvedLayoutStyle,
        display: LayoutDisplay,
    ) -> Result<ResolvedLayoutStyle, LayoutError> {
        let parent_computed = parent.stylo_computed_values().ok_or_else(|| {
            LayoutError::style_resolution(
                format!("{owner:?}"),
                "native anonymous box parent has no retained Stylo ComputedValues",
            )
        })?;
        let anonymous_kind = anonymous_box_kind(display);
        let read = self.reads.read(owner);
        let computed = read
            .anonymous_computed_values(parent_computed.as_ref(), anonymous_kind)
            .ok_or_else(|| {
                LayoutError::style_resolution(
                    format!("{owner:?}"),
                    format!("Stylo could not resolve {anonymous_kind:?} anonymous style"),
                )
            })?;
        let mut resolved = ResolvedLayoutStyle::from_stylo(computed);
        resolved.force_layout_display(display);
        Ok(resolved)
    }
}

const fn anonymous_box_kind(display: LayoutDisplay) -> StyloAnonymousBoxKind {
    match display {
        LayoutDisplay::Table | LayoutDisplay::InlineTable => StyloAnonymousBoxKind::Table,
        LayoutDisplay::TableRow => StyloAnonymousBoxKind::TableRow,
        LayoutDisplay::TableCell => StyloAnonymousBoxKind::TableCell,
        LayoutDisplay::None
        | LayoutDisplay::Contents
        | LayoutDisplay::Block
        | LayoutDisplay::FlowRoot
        | LayoutDisplay::Inline
        | LayoutDisplay::InlineBlock
        | LayoutDisplay::Flex
        | LayoutDisplay::InlineFlex
        | LayoutDisplay::Grid
        | LayoutDisplay::InlineGrid
        | LayoutDisplay::BlockListItem
        | LayoutDisplay::InlineListItem
        | LayoutDisplay::TableCaption
        | LayoutDisplay::TableRowGroup
        | LayoutDisplay::TableHeaderGroup
        | LayoutDisplay::TableFooterGroup
        | LayoutDisplay::TableColumnGroup
        | LayoutDisplay::TableColumn => StyloAnonymousBoxKind::Generic,
    }
}

const fn pseudo_style_name(pseudo: LayoutPseudo) -> &'static str {
    match pseudo {
        LayoutPseudo::Marker => "marker",
        LayoutPseudo::Before => "before",
        LayoutPseudo::After => "after",
    }
}

#[cfg(test)]
mod tests {
    use super::{anonymous_box_kind, pseudo_style_name};
    use crate::style_engine::StyloAnonymousBoxKind;
    use moli_layout::{LayoutDisplay, LayoutPseudo};

    #[test]
    fn all_layout_pseudos_map_to_stylo_names() {
        assert_eq!(pseudo_style_name(LayoutPseudo::Marker), "marker");
        assert_eq!(pseudo_style_name(LayoutPseudo::Before), "before");
        assert_eq!(pseudo_style_name(LayoutPseudo::After), "after");
    }

    #[test]
    fn anonymous_table_roles_use_the_matching_servo_precomputed_pseudo() {
        assert_eq!(
            anonymous_box_kind(LayoutDisplay::Table),
            StyloAnonymousBoxKind::Table
        );
        assert_eq!(
            anonymous_box_kind(LayoutDisplay::TableRow),
            StyloAnonymousBoxKind::TableRow
        );
        assert_eq!(
            anonymous_box_kind(LayoutDisplay::TableCell),
            StyloAnonymousBoxKind::TableCell
        );
        assert_eq!(
            anonymous_box_kind(LayoutDisplay::TableRowGroup),
            StyloAnonymousBoxKind::Generic
        );
        assert_eq!(
            anonymous_box_kind(LayoutDisplay::Block),
            StyloAnonymousBoxKind::Generic
        );
    }
}
