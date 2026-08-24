// SPDX-License-Identifier: MIT OR Apache-2.0
//
// SVG presentation-attribute synthesis is narrowly ported from
// DioxusLabs/blitz packages/blitz-dom/src/stylo.rs. Keeping it in the Stylo
// adapter lets the normal cascade, inheritance, and relative-length resolver
// own the result instead of teaching layout about authored attribute strings.

use selectors::sink::Push;
use style::{
    applicable_declarations::ApplicableDeclarationBlock,
    context::QuirksMode,
    properties::{
        Importance, PropertyDeclaration, PropertyDeclarationBlock, PropertyId,
        SourcePropertyDeclaration, parse_one_declaration_into,
    },
    rule_tree::{CascadeLevel, CascadeOrigin},
    servo_arc::Arc,
    shared_lock::SharedRwLock,
    stylesheets::{CssRuleType, Origin, UrlExtraData, layer_rule::LayerOrder},
    values::specified::{LengthPercentage, NoCalcLength, NoCalcPercentage},
};
use style_traits::ParsingMode;

use crate::dom::{
    NodeId,
    native::{DomHost, Element},
};

const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";

// Mirrors Blink's CSSPropertyIdForSVGAttributeName allowlist. These attributes
// participate in the author cascade as presentation hints: author rules and an
// inline style override them, while inheritance observes their parsed CSS
// value. Geometry attributes such as the root SVG width/height are handled
// separately below because they have element-specific SVG parsing rules.
const SVG_STYLE_PRESENTATION_ATTRIBUTES: &[&str] = &[
    "alignment-baseline",
    "baseline-shift",
    "buffered-rendering",
    "clip",
    "clip-path",
    "clip-rule",
    "color",
    "color-interpolation",
    "color-interpolation-filters",
    "color-rendering",
    "cursor",
    "direction",
    "display",
    "dominant-baseline",
    "fill",
    "fill-opacity",
    "fill-rule",
    "filter",
    "flood-color",
    "flood-opacity",
    "font-family",
    "font-size",
    "font-stretch",
    "font-style",
    "font-variant",
    "font-weight",
    "image-rendering",
    "letter-spacing",
    "lighting-color",
    "marker-end",
    "marker-mid",
    "marker-start",
    "mask",
    "mask-type",
    "opacity",
    "overflow",
    "paint-order",
    "pointer-events",
    "shape-rendering",
    "stop-color",
    "stop-opacity",
    "stroke",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-opacity",
    "stroke-width",
    "text-anchor",
    "text-decoration",
    "text-rendering",
    "transform-origin",
    "unicode-bidi",
    "vector-effect",
    "visibility",
    "word-spacing",
    "writing-mode",
];

/// Whether changing an attribute can change an SVG element's computed style
/// without any selector dependency on that attribute.
pub fn is_svg_presentation_attribute_name(name: &str) -> bool {
    matches!(name, "width" | "height") || SVG_STYLE_PRESENTATION_ATTRIBUTES.contains(&name)
}

pub(super) fn synthesize_svg_presentational_hints<V>(
    host: &DomHost,
    handle: NodeId,
    element: &Element,
    quirks_mode: QuirksMode,
    shared_lock: &SharedRwLock,
    hints: &mut V,
) where
    V: Push<ApplicableDeclarationBlock>,
{
    if element.namespace() != SVG_NAMESPACE {
        return;
    }

    let mut block = PropertyDeclarationBlock::new();
    if element.local_name() == "svg" {
        append_root_svg_size_declarations(element, &mut block);
    }
    append_svg_style_presentation_declarations(host, handle, element, quirks_mode, &mut block);

    if !block.is_empty() {
        hints.push(ApplicableDeclarationBlock::from_declarations(
            Arc::new(shared_lock.wrap(block)),
            CascadeLevel::new(CascadeOrigin::PresHints),
            LayerOrder::root(),
        ));
    }
}

fn append_root_svg_size_declarations(element: &Element, block: &mut PropertyDeclarationBlock) {
    for (attribute, is_width) in [("width", true), ("height", false)] {
        let Some(value) = element.attribute(attribute) else {
            continue;
        };
        let Some(size) = parse_svg_size_attribute(value) else {
            continue;
        };
        use style::values::generics::{NonNegative, length::Size};
        let size = Size::LengthPercentage(NonNegative(size));
        let declaration = if is_width {
            PropertyDeclaration::Width(size)
        } else {
            PropertyDeclaration::Height(size)
        };
        block.push(declaration, Importance::Normal);
    }
}

fn append_svg_style_presentation_declarations(
    host: &DomHost,
    handle: NodeId,
    element: &Element,
    quirks_mode: QuirksMode,
    block: &mut PropertyDeclarationBlock,
) {
    let Some(base_url) = host
        .owner_document_handle(handle)
        .and_then(|document| host.document_base_url_for_handle(document))
    else {
        return;
    };
    let url_data = UrlExtraData::from(base_url);

    for attribute in element.attributes() {
        if !attribute.namespace().is_empty()
            || !SVG_STYLE_PRESENTATION_ATTRIBUTES.contains(&attribute.local_name())
        {
            continue;
        }
        let Ok(property) = PropertyId::parse_enabled_for_all_content(attribute.local_name()) else {
            continue;
        };
        let mut declarations = SourcePropertyDeclaration::default();
        if parse_one_declaration_into(
            &mut declarations,
            property,
            attribute.value(),
            Origin::Author,
            &url_data,
            None,
            ParsingMode::ALLOW_UNITLESS_LENGTH | ParsingMode::ALLOW_ALL_NUMERIC_VALUES,
            quirks_mode,
            CssRuleType::Style,
        )
        .is_ok()
        {
            block.extend(declarations.drain(), Importance::Normal);
        }
    }
}

/// Parses the SVG 2 root `width`/`height` presentation attributes.
///
/// These are CSS `<length-percentage>` values, unlike legacy HTML dimension
/// attributes. Unitless numbers are SVG user units and therefore CSS pixels;
/// relative units such as `em`, `rem`, and viewport units remain specified
/// lengths here so Stylo resolves them in the element's real style context.
fn parse_svg_size_attribute(value: &str) -> Option<LengthPercentage> {
    let value = value.trim();
    if let Some(number) = value.strip_suffix('%') {
        let value = number.trim().parse::<f32>().ok()?;
        return (value.is_finite() && value >= 0.0)
            .then(|| LengthPercentage::Percentage(NoCalcPercentage::new(value / 100.0)));
    }

    // A CSS dimension has no whitespace between its number and unit. Taking
    // only a trailing alphabetic run leaves scientific notation such as
    // `1e3` intact because it ends in a digit.
    let number_len = value
        .trim_end_matches(|character: char| character.is_ascii_alphabetic())
        .len();
    let (number, unit) = value.split_at(number_len);
    let value = number
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)?;
    let length = if unit.is_empty() {
        NoCalcLength::from_px(value)
    } else {
        NoCalcLength::parse_dimension_with_flags(ParsingMode::DEFAULT, false, value, unit).ok()?
    };
    Some(LengthPercentage::Length(length))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_size_attribute_preserves_relative_units_for_stylo() {
        assert!(parse_svg_size_attribute("1em").is_some());
        assert!(parse_svg_size_attribute("1.5rem").is_some());
        assert!(parse_svg_size_attribute("24").is_some());
        assert!(parse_svg_size_attribute("50%").is_some());
        assert!(parse_svg_size_attribute("auto").is_none());
        assert!(parse_svg_size_attribute("-1em").is_none());
    }

    #[test]
    fn svg_paint_attributes_are_classified_as_presentational() {
        for name in [
            "fill",
            "fill-opacity",
            "stroke",
            "stroke-width",
            "paint-order",
            "shape-rendering",
            "width",
            "height",
        ] {
            assert!(
                is_svg_presentation_attribute_name(name),
                "{name} must invalidate presentation hints when mutated"
            );
        }
        assert!(!is_svg_presentation_attribute_name("viewBox"));
        assert!(!is_svg_presentation_attribute_name("d"));
    }
}
