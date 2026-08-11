use std::collections::HashSet;

use cssparser::{Parser, ParserInput, Token, UnicodeRange};
use moli_crypto::sha256_hex;
use moli_css_parse::{
    DeclarationParseOptions, parse_declaration_list, parse_font_face_rule_view_with_stylo,
    parse_font_faces, parse_stylesheet_rule_snapshots_with_stylo,
};
use moli_layout::{WebFontFace, WebFontRegistration, WebFontStyle, WebFontUnicodeRange};
use url::Url;

use crate::protocol_types::OptionalResourceFetchMask;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StylesheetLoadBlockingResourceKind {
    Image,
    Font,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StylesheetLoadBlockingResource {
    request_url: Url,
    kind: StylesheetLoadBlockingResourceKind,
    web_font: Option<StylesheetWebFont>,
}

impl StylesheetLoadBlockingResource {
    fn new(request_url: Url, kind: StylesheetLoadBlockingResourceKind) -> Self {
        Self {
            request_url,
            kind,
            web_font: None,
        }
    }

    pub(crate) fn image(request_url: Url) -> Self {
        Self::new(request_url, StylesheetLoadBlockingResourceKind::Image)
    }

    fn font(request_url: Url, web_font: StylesheetWebFont) -> Self {
        Self {
            request_url,
            kind: StylesheetLoadBlockingResourceKind::Font,
            web_font: Some(web_font),
        }
    }

    pub(crate) fn request_url(&self) -> &Url {
        &self.request_url
    }

    pub(crate) fn into_parts(self) -> (Url, Option<StylesheetWebFont>) {
        (self.request_url, self.web_font)
    }

    pub(crate) fn web_font(&self) -> Option<&StylesheetWebFont> {
        self.web_font.as_ref()
    }

    pub(crate) fn bind_web_font_request(mut self, request_id: u64) -> Self {
        self.web_font = self
            .web_font
            .take()
            .map(|font| font.bind_request(request_id));
        self
    }

    pub(crate) fn kind(&self) -> StylesheetLoadBlockingResourceKind {
        self.kind
    }
}

/// Parsed `@font-face` metadata that stays attached to the exact resource
/// request until its owner-validated completion.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StylesheetWebFont {
    slot: String,
    face: WebFontFace,
    request_id: Option<u64>,
}

impl StylesheetWebFont {
    pub(crate) fn bind_request(mut self, request_id: u64) -> Self {
        self.request_id = Some(request_id);
        self
    }

    pub(crate) fn request_id(&self) -> Option<u64> {
        self.request_id
    }

    pub(crate) fn registration(&self, bytes: Vec<u8>) -> WebFontRegistration {
        WebFontRegistration::new(self.slot.clone(), self.face.clone(), bytes)
    }

    pub(crate) fn slot(&self) -> &str {
        &self.slot
    }

    pub(crate) fn face(&self) -> &WebFontFace {
        &self.face
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CompletedStylesheetWebFont {
    request: StylesheetWebFont,
    bytes: Option<Vec<u8>>,
}

impl CompletedStylesheetWebFont {
    pub(crate) fn response(request: StylesheetWebFont, bytes: Vec<u8>) -> Self {
        Self {
            request,
            bytes: Some(bytes),
        }
    }

    pub(crate) fn failure(request: StylesheetWebFont) -> Self {
        Self {
            request,
            bytes: None,
        }
    }

    pub(crate) fn into_parts(self) -> (StylesheetWebFont, Option<Vec<u8>>) {
        (self.request, self.bytes)
    }
}

pub(crate) fn stylesheet_load_blocking_resources(
    css_text: &str,
    base_url: &Url,
    optional_resource_fetch_mask: OptionalResourceFetchMask,
) -> Vec<StylesheetLoadBlockingResource> {
    let mut resources = Vec::new();
    if optional_resource_fetch_mask.contains(OptionalResourceFetchMask::IMAGE) {
        resources.extend(
            stylesheet_image_resource_urls(css_text, base_url)
                .into_iter()
                .map(StylesheetLoadBlockingResource::image),
        );
    }
    if optional_resource_fetch_mask.contains(OptionalResourceFetchMask::FONT) {
        resources.extend(stylesheet_web_font_resources(css_text, base_url));
    }
    resources
}

fn stylesheet_web_font_resources(
    css_text: &str,
    base_url: &Url,
) -> Vec<StylesheetLoadBlockingResource> {
    let rules = parse_stylesheet_rule_snapshots_with_stylo(css_text);
    let mut font_face_rules = Vec::new();
    collect_font_face_rule_css(&rules, &mut font_face_rules);
    font_face_rules
        .into_iter()
        .filter_map(|css_text| {
            let descriptor = parsed_web_font_face(&css_text)?;
            let request_url = preferred_font_source_url(descriptor.source(), base_url)?;
            // The slot describes a declaration, not a layout generation. Two
            // identical declarations may safely share decoded font data, while
            // a CSSOM replacement gets a different slot even when it occupies
            // the same rule index. This also avoids collisions between separate
            // inline/adopted sheets that use the same document base URL.
            let mut slot_material =
                Vec::with_capacity(base_url.as_str().len() + css_text.len() + 1);
            slot_material.extend_from_slice(base_url.as_str().as_bytes());
            slot_material.push(0);
            slot_material.extend_from_slice(css_text.as_bytes());
            let slot = format!("stylesheet-font:{}", sha256_hex(&slot_material));
            Some(StylesheetLoadBlockingResource::font(
                request_url,
                StylesheetWebFont {
                    slot,
                    face: descriptor.into_face(),
                    request_id: None,
                },
            ))
        })
        .collect()
}

fn collect_font_face_rule_css(rules: &[moli_css_parse::CssRuleSnapshot], output: &mut Vec<String>) {
    for rule in rules {
        if rule.rule_type == style::stylesheets::CssRuleType::FontFace {
            output.push(rule.css_text.clone());
        }
        collect_font_face_rule_css(&rule.child_rules, output);
    }
}

#[derive(Debug)]
struct ParsedWebFontFace {
    family: String,
    source: String,
    weight: Option<f32>,
    stretch: Option<f32>,
    style: Option<WebFontStyle>,
    unicode_ranges: Vec<WebFontUnicodeRange>,
}

impl ParsedWebFontFace {
    fn source(&self) -> &str {
        &self.source
    }

    fn into_face(self) -> WebFontFace {
        let mut face = WebFontFace::new(self.family);
        if let Some(weight) = self.weight {
            face = face.with_weight(weight);
        }
        if let Some(stretch) = self.stretch {
            face = face.with_stretch(stretch);
        }
        if let Some(style) = self.style {
            face = face.with_style(style);
        }
        face.with_unicode_ranges(self.unicode_ranges)
    }
}

fn parsed_web_font_face(css_text: &str) -> Option<ParsedWebFontFace> {
    let parsed = parse_font_faces(css_text).into_iter().next()?;
    let view = parse_font_face_rule_view_with_stylo(css_text)?;
    let declarations = parse_declaration_list(
        &view.style_text,
        DeclarationParseOptions {
            canonicalize_property_name: true,
            unescape_value_semicolons: false,
            preserve_empty_values: false,
        },
    );
    let descriptor = |name: &str| {
        declarations
            .iter()
            .rev()
            .find(|declaration| declaration.name == name)
            .map(|declaration| declaration.value.as_str())
    };
    Some(ParsedWebFontFace {
        family: parsed.family,
        source: parsed.source,
        weight: descriptor("font-weight").and_then(parse_font_weight_lower_bound),
        stretch: descriptor("font-stretch").and_then(parse_font_stretch_lower_bound),
        style: descriptor("font-style").and_then(parse_font_style_lower_bound),
        unicode_ranges: descriptor("unicode-range")
            .and_then(parse_font_unicode_ranges)
            .unwrap_or_default(),
    })
}

fn parse_font_unicode_ranges(value: &str) -> Option<Vec<WebFontUnicodeRange>> {
    let mut input = ParserInput::new(value);
    let mut input = Parser::new(&mut input);
    input
        .parse_comma_separated(|input| {
            let range = UnicodeRange::parse(input)?;
            Ok::<_, cssparser::ParseError<'_, ()>>(WebFontUnicodeRange::new(range.start, range.end))
        })
        .ok()
}

fn parse_font_weight_lower_bound(value: &str) -> Option<f32> {
    match value
        .split_ascii_whitespace()
        .next()?
        .to_ascii_lowercase()
        .as_str()
    {
        "normal" => Some(400.0),
        "bold" => Some(700.0),
        value => value
            .parse::<f32>()
            .ok()
            .filter(|value| (1.0..=1000.0).contains(value)),
    }
}

fn parse_font_stretch_lower_bound(value: &str) -> Option<f32> {
    let value = value.split_ascii_whitespace().next()?.to_ascii_lowercase();
    let percentage = match value.as_str() {
        "ultra-condensed" => 50.0,
        "extra-condensed" => 62.5,
        "condensed" => 75.0,
        "semi-condensed" => 87.5,
        "normal" => 100.0,
        "semi-expanded" => 112.5,
        "expanded" => 125.0,
        "extra-expanded" => 150.0,
        "ultra-expanded" => 200.0,
        value => value.strip_suffix('%')?.parse::<f32>().ok()?,
    };
    (percentage > 0.0 && percentage.is_finite()).then_some(percentage)
}

fn parse_font_style_lower_bound(value: &str) -> Option<WebFontStyle> {
    let mut values = value.split_ascii_whitespace();
    match values.next()?.to_ascii_lowercase().as_str() {
        "normal" => Some(WebFontStyle::Normal),
        "italic" => Some(WebFontStyle::Italic),
        "oblique" => Some(WebFontStyle::Oblique(
            values.next().and_then(parse_css_angle_degrees),
        )),
        _ => None,
    }
}

fn parse_css_angle_degrees(value: &str) -> Option<f32> {
    let value = value.to_ascii_lowercase();
    let parse = |suffix: &str| value.strip_suffix(suffix)?.parse::<f32>().ok();
    parse("deg")
        .or_else(|| parse("grad").map(|value| value * 0.9))
        .or_else(|| parse("rad").map(f32::to_degrees))
        .or_else(|| parse("turn").map(|value| value * 360.0))
        .filter(|value| value.is_finite())
}

fn preferred_font_source_url(source: &str, base_url: &Url) -> Option<Url> {
    let mut input = ParserInput::new(source);
    let mut input = Parser::new(&mut input);
    let mut candidate_url: Option<String> = None;
    let mut candidate_format_is_supported = true;
    while let Ok(token) = input.next_including_whitespace_and_comments().cloned() {
        match token {
            Token::Comma => {
                if candidate_format_is_supported && let Some(url) = candidate_url.take() {
                    return resolve_font_source_url(&url, base_url);
                }
                candidate_url = None;
                candidate_format_is_supported = true;
            }
            Token::UnquotedUrl(raw_url) => candidate_url = Some(raw_url.to_string()),
            Token::Function(name) if name.eq_ignore_ascii_case("url") => {
                let _ = input.parse_nested_block(|input| {
                    candidate_url = css_url_function_value(input);
                    Ok::<(), cssparser::ParseError<'_, ()>>(())
                });
            }
            Token::Function(name) if name.eq_ignore_ascii_case("format") => {
                let _ = input.parse_nested_block(|input| {
                    candidate_format_is_supported = font_format_function_is_supported(input);
                    Ok::<(), cssparser::ParseError<'_, ()>>(())
                });
            }
            Token::Function(_) | Token::ParenthesisBlock | Token::SquareBracketBlock => {
                let _ = input.parse_nested_block(|input| {
                    skip_css_block(input);
                    Ok::<(), cssparser::ParseError<'_, ()>>(())
                });
            }
            _ => {}
        }
    }
    candidate_url
        .filter(|_| candidate_format_is_supported)
        .and_then(|url| resolve_font_source_url(&url, base_url))
}

fn css_url_function_value<'i, 't>(input: &mut Parser<'i, 't>) -> Option<String> {
    while let Ok(token) = input.next_including_whitespace_and_comments().cloned() {
        match token {
            Token::WhiteSpace(_) | Token::Comment(_) => {}
            Token::QuotedString(value) | Token::UnquotedUrl(value) => {
                return Some(value.to_string());
            }
            _ => return None,
        }
    }
    None
}

fn font_format_function_is_supported<'i, 't>(input: &mut Parser<'i, 't>) -> bool {
    let mut saw_format = false;
    while let Ok(token) = input.next_including_whitespace_and_comments().cloned() {
        let value = match token {
            Token::WhiteSpace(_) | Token::Comment(_) | Token::Comma => continue,
            Token::Ident(value) | Token::QuotedString(value) => value.to_ascii_lowercase(),
            _ => return false,
        };
        saw_format = true;
        if !matches!(
            value.as_ref(),
            "woff" | "woff2" | "truetype" | "opentype" | "ttf" | "otf" | "collection"
        ) {
            return false;
        }
    }
    saw_format
}

fn resolve_font_source_url(raw_url: &str, base_url: &Url) -> Option<Url> {
    let raw_url = raw_url.trim();
    let url = base_url
        .join(raw_url)
        .or_else(|_| Url::parse(raw_url))
        .ok()?;
    matches!(url.scheme(), "http" | "https" | "data" | "blob").then_some(url)
}

pub(crate) fn stylesheet_image_resource_urls(css_text: &str, base_url: &Url) -> Vec<Url> {
    let mut input = ParserInput::new(css_text);
    let mut parser = Parser::new(&mut input);
    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    collect_css_resource_urls(&mut parser, base_url, &mut seen, &mut urls);
    urls
}

fn collect_css_resource_urls<'i, 't>(
    input: &mut Parser<'i, 't>,
    base_url: &Url,
    seen: &mut HashSet<String>,
    urls: &mut Vec<Url>,
) {
    while let Ok(token) = input.next_including_whitespace_and_comments().cloned() {
        match token {
            Token::AtKeyword(name) if name.eq_ignore_ascii_case("font-face") => {
                skip_css_at_rule_body(input);
            }
            Token::AtKeyword(name)
                if name.eq_ignore_ascii_case("import")
                    || name.eq_ignore_ascii_case("namespace") =>
            {
                skip_css_at_rule_body(input);
            }
            Token::UnquotedUrl(raw_url) => {
                push_resolved_css_resource_url(&raw_url, base_url, seen, urls);
            }
            Token::Function(name) if name.eq_ignore_ascii_case("url") => {
                let _ = input.parse_nested_block(|input| {
                    collect_css_url_function_argument(input, base_url, seen, urls);
                    Ok::<(), cssparser::ParseError<'_, ()>>(())
                });
            }
            Token::Function(_)
            | Token::ParenthesisBlock
            | Token::SquareBracketBlock
            | Token::CurlyBracketBlock => {
                let _ = input.parse_nested_block(|input| {
                    collect_css_resource_urls(input, base_url, seen, urls);
                    Ok::<(), cssparser::ParseError<'_, ()>>(())
                });
            }
            _ => {}
        }
    }
}

fn collect_css_url_function_argument<'i, 't>(
    input: &mut Parser<'i, 't>,
    base_url: &Url,
    seen: &mut HashSet<String>,
    urls: &mut Vec<Url>,
) {
    while let Ok(token) = input.next_including_whitespace_and_comments().cloned() {
        match token {
            Token::WhiteSpace(_) | Token::Comment(_) => {}
            Token::QuotedString(raw_url) | Token::UnquotedUrl(raw_url) => {
                push_resolved_css_resource_url(&raw_url, base_url, seen, urls);
                return;
            }
            _ => return,
        }
    }
}

fn skip_css_at_rule_body<'i, 't>(input: &mut Parser<'i, 't>) {
    while let Ok(token) = input.next_including_whitespace_and_comments().cloned() {
        match token {
            Token::Semicolon => return,
            Token::CurlyBracketBlock => {
                let _ = input.parse_nested_block(|input| {
                    skip_css_block(input);
                    Ok::<(), cssparser::ParseError<'_, ()>>(())
                });
                return;
            }
            Token::Function(_) | Token::ParenthesisBlock | Token::SquareBracketBlock => {
                let _ = input.parse_nested_block(|input| {
                    skip_css_block(input);
                    Ok::<(), cssparser::ParseError<'_, ()>>(())
                });
            }
            _ => {}
        }
    }
}

fn skip_css_block<'i, 't>(input: &mut Parser<'i, 't>) {
    while let Ok(token) = input.next_including_whitespace_and_comments().cloned() {
        match token {
            Token::Function(_)
            | Token::ParenthesisBlock
            | Token::SquareBracketBlock
            | Token::CurlyBracketBlock => {
                let _ = input.parse_nested_block(|input| {
                    skip_css_block(input);
                    Ok::<(), cssparser::ParseError<'_, ()>>(())
                });
            }
            _ => {}
        }
    }
}

fn push_resolved_css_resource_url(
    raw_url: &str,
    base_url: &Url,
    seen: &mut HashSet<String>,
    urls: &mut Vec<Url>,
) {
    let raw_url = raw_url.trim();
    if raw_url.is_empty() {
        return;
    }
    let Ok(url) = base_url.join(raw_url).or_else(|_| Url::parse(raw_url)) else {
        return;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return;
    }
    if seen.insert(url.as_str().to_owned()) {
        urls.push(url);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_url() -> Url {
        Url::parse("https://example.test/assets/app.css").expect("base URL should parse")
    }

    #[test]
    fn extracts_stylesheet_url_function_resources() {
        let urls = stylesheet_image_resource_urls(
            r#"
                body { background-image: url("hero.png"); }
                .icon { mask: url(icons/icon.svg); }
            "#,
            &base_url(),
        );
        assert_eq!(
            urls.into_iter()
                .map(|url| url.to_string())
                .collect::<Vec<_>>(),
            vec![
                "https://example.test/assets/hero.png",
                "https://example.test/assets/icons/icon.svg",
            ]
        );
    }

    #[test]
    fn ignores_duplicate_and_non_network_urls() {
        let urls = stylesheet_image_resource_urls(
            r#"
                .a { background: url(shared.png); }
                .b { background: url("shared.png"); }
                .c { background: url("data:image/png;base64,aa"); }
                .d { background: url("about:blank"); }
            "#,
            &base_url(),
        );
        assert_eq!(
            urls.into_iter()
                .map(|url| url.to_string())
                .collect::<Vec<_>>(),
            vec!["https://example.test/assets/shared.png"]
        );
    }

    #[test]
    fn keeps_font_face_urls_out_of_stylesheet_image_resources() {
        let image_urls = stylesheet_image_resource_urls(
            r#"
                @font-face {
                    font-family: Demo;
                    src: local("Demo"), url(fonts/demo.woff2) format("woff2");
                }
                @media screen {
                    @font-face {
                        font-family: NestedDemo;
                        src: url(fonts/nested.woff2);
                    }
                }
                @media screen {
                    body { background-image: url(hero.png); }
                }
            "#,
            &base_url(),
        );
        assert_eq!(
            image_urls
                .into_iter()
                .map(|url| url.to_string())
                .collect::<Vec<_>>(),
            vec!["https://example.test/assets/hero.png"]
        );
    }

    #[test]
    fn keeps_non_image_at_rule_urls_out_of_stylesheet_image_resources() {
        let image_urls = stylesheet_image_resource_urls(
            r#"
                @import url(imported.css);
                @namespace svg url("http://www.w3.org/2000/svg");
                @media screen {
                    body { background-image: url(hero.png); }
                }
            "#,
            &base_url(),
        );
        assert_eq!(
            image_urls
                .into_iter()
                .map(|url| url.to_string())
                .collect::<Vec<_>>(),
            vec!["https://example.test/assets/hero.png"]
        );
    }

    #[test]
    fn selects_one_supported_source_per_font_face_and_keeps_descriptors() {
        let resources = stylesheet_load_blocking_resources(
            r#"
                @font-face {
                    font-family: Demo;
                    src: local("Demo"),
                         url(fonts/demo.svg) format("svg"),
                         url(fonts/demo.woff2) format("woff2"),
                         url("/fonts/demo.woff") format("woff");
                    font-weight: 625 800;
                    font-stretch: semi-condensed expanded;
                    font-style: oblique 30deg 45deg;
                    unicode-range: U+0000-00FF, U+4E??;
                }
                @font-face {
                    font-family: DemoBold;
                    src: url(fonts/demo.woff2);
                }
                @media screen {
                    @font-face {
                        font-family: NestedDemo;
                        src: url(fonts/nested.woff2);
                    }
                }
                @font-face {
                    font-family: DataFace;
                    src: url("data:font/woff2;base64,AA==") format("woff2");
                }
                body { background-image: url(hero.png); }
            "#,
            &base_url(),
            OptionalResourceFetchMask::FONT,
        );
        assert_eq!(
            resources
                .iter()
                .map(|resource| resource.request_url().to_string())
                .collect::<Vec<_>>(),
            vec![
                "https://example.test/assets/fonts/demo.woff2",
                "https://example.test/assets/fonts/demo.woff2",
                "https://example.test/assets/fonts/nested.woff2",
                "data:font/woff2;base64,AA==",
            ]
        );
        let web_font = resources[0]
            .web_font
            .as_ref()
            .expect("font resource should retain its descriptors");
        assert!(web_font.slot().starts_with("stylesheet-font:"));
        assert_eq!(web_font.request_id(), None);
        assert_eq!(web_font.face().family_name(), "Demo");
        assert_eq!(web_font.face().weight(), Some(625.0));
        assert_eq!(web_font.face().stretch(), Some(87.5));
        assert_eq!(
            web_font.face().style(),
            Some(WebFontStyle::Oblique(Some(30.0)))
        );
        assert_eq!(
            web_font.face().unicode_ranges(),
            [
                WebFontUnicodeRange::new(0x0000, 0x00ff),
                WebFontUnicodeRange::new(0x4e00, 0x4eff),
            ]
        );
    }

    #[test]
    fn invalid_unicode_range_falls_back_to_the_full_font_face_range() {
        let resources = stylesheet_load_blocking_resources(
            r#"
                @font-face {
                    font-family: Demo;
                    src: url(fonts/demo.woff2);
                    unicode-range: U+110000-120000;
                }
            "#,
            &base_url(),
            OptionalResourceFetchMask::FONT,
        );
        assert_eq!(resources.len(), 1);
        assert!(
            resources[0]
                .web_font()
                .expect("font metadata")
                .face()
                .unicode_ranges()
                .is_empty()
        );
    }

    #[test]
    fn load_blocking_resource_selection_obeys_image_and_font_bits_independently() {
        let css = r#"
            @font-face {
                font-family: Demo;
                src: url(fonts/demo.woff2);
            }
            body { background-image: url(images/hero.png); }
        "#;
        let selected = |mask| {
            stylesheet_load_blocking_resources(css, &base_url(), mask)
                .into_iter()
                .map(|resource| (resource.kind(), resource.request_url().to_string()))
                .collect::<Vec<_>>()
        };

        assert!(selected(OptionalResourceFetchMask::NONE).is_empty());
        assert_eq!(
            selected(OptionalResourceFetchMask::IMAGE),
            vec![(
                StylesheetLoadBlockingResourceKind::Image,
                "https://example.test/assets/images/hero.png".to_owned(),
            )]
        );
        assert_eq!(
            selected(OptionalResourceFetchMask::FONT),
            vec![(
                StylesheetLoadBlockingResourceKind::Font,
                "https://example.test/assets/fonts/demo.woff2".to_owned(),
            )]
        );
        assert_eq!(
            selected(OptionalResourceFetchMask::IMAGE | OptionalResourceFetchMask::FONT),
            vec![
                (
                    StylesheetLoadBlockingResourceKind::Image,
                    "https://example.test/assets/images/hero.png".to_owned(),
                ),
                (
                    StylesheetLoadBlockingResourceKind::Font,
                    "https://example.test/assets/fonts/demo.woff2".to_owned(),
                ),
            ]
        );
        for unrelated in [
            OptionalResourceFetchMask::AUDIO,
            OptionalResourceFetchMask::VIDEO,
            OptionalResourceFetchMask::MEDIA,
            OptionalResourceFetchMask::TEXT_TRACK,
        ] {
            assert!(
                selected(unrelated).is_empty(),
                "{unrelated:?} must not enable CSS image or font requests"
            );
        }
    }
}
