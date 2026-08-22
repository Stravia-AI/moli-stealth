use std::{collections::BTreeMap, sync::Arc};

use crate::{css_resource_urls::StylesheetLoadBlockingResource, document_runtime::DomHandle};

use super::{active_stylesheets::ActiveStylesheetCollection, shadow_scopes::ShadowScopeStyles};

#[cfg(test)]
thread_local! {
    static STYLESHEET_RESOURCE_MANIFEST_BUILD_COUNT: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
pub(crate) fn reset_stylesheet_resource_manifest_build_count_for_test() {
    STYLESHEET_RESOURCE_MANIFEST_BUILD_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn stylesheet_resource_manifest_build_count_for_test() -> usize {
    STYLESHEET_RESOURCE_MANIFEST_BUILD_COUNT.with(std::cell::Cell::get)
}

/// Identity of one typed stylesheet resource set published by a Document.
///
/// The revision advances only when the font/import manifest changes, not for
/// unrelated style-world flushes. Keeping the Document in the type prevents a
/// layout-owned sidecar from treating equal revisions from two iframe worlds
/// as interchangeable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct StylesheetResourceGeneration {
    document: DomHandle,
    revision: u64,
}

impl StylesheetResourceGeneration {
    pub(super) const fn new(document: DomHandle, revision: u64) -> Self {
        Self { document, revision }
    }
}

/// Owned, typed resource projection from one current retained style world.
#[derive(Clone, Debug)]
pub(crate) struct StylesheetResourceSnapshot {
    generation: StylesheetResourceGeneration,
    web_fonts: Arc<[StylesheetLoadBlockingResource]>,
    imports: Arc<[url::Url]>,
}

impl StylesheetResourceSnapshot {
    pub(super) fn new(
        generation: StylesheetResourceGeneration,
        web_fonts: Arc<[StylesheetLoadBlockingResource]>,
        imports: Arc<[url::Url]>,
    ) -> Self {
        Self {
            generation,
            web_fonts,
            imports,
        }
    }

    pub(crate) const fn generation(&self) -> StylesheetResourceGeneration {
        self.generation
    }

    pub(crate) fn web_fonts(&self) -> &[StylesheetLoadBlockingResource] {
        self.web_fonts.as_ref()
    }

    pub(crate) fn imports(&self) -> &[url::Url] {
        self.imports.as_ref()
    }
}

/// Resource metadata retained beside the exact parsed stylesheet collections.
/// It contains no layout, paint, or network state.
#[derive(PartialEq)]
pub(super) struct StylesheetResourceManifest {
    web_fonts: Arc<[StylesheetLoadBlockingResource]>,
    imports: Arc<[url::Url]>,
}

impl StylesheetResourceManifest {
    pub(super) fn from_active_stylesheets(
        document_stylesheets: &ActiveStylesheetCollection,
        shadow_scopes: &[ShadowScopeStyles],
    ) -> Self {
        #[cfg(test)]
        STYLESHEET_RESOURCE_MANIFEST_BUILD_COUNT.with(|count| count.set(count.get() + 1));
        let mut web_fonts = BTreeMap::new();
        let mut imports = BTreeMap::new();
        collect_resources(document_stylesheets, &mut web_fonts, &mut imports);
        for scope in shadow_scopes {
            collect_resources(scope.active_stylesheets(), &mut web_fonts, &mut imports);
        }
        Self {
            web_fonts: web_fonts.into_values().collect::<Vec<_>>().into(),
            imports: imports.into_values().collect::<Vec<_>>().into(),
        }
    }

    pub(super) fn snapshot(
        &self,
        generation: StylesheetResourceGeneration,
    ) -> StylesheetResourceSnapshot {
        StylesheetResourceSnapshot::new(
            generation,
            Arc::clone(&self.web_fonts),
            Arc::clone(&self.imports),
        )
    }
}

fn collect_resources(
    stylesheets: &ActiveStylesheetCollection,
    web_fonts: &mut BTreeMap<String, StylesheetLoadBlockingResource>,
    imports: &mut BTreeMap<String, url::Url>,
) {
    for stylesheet in stylesheets.entries() {
        for resource in stylesheet.web_font_resources() {
            let Some(font) = resource.web_font() else {
                continue;
            };
            web_fonts
                .entry(font.slot().to_owned())
                .or_insert_with(|| resource.clone());
        }
        for import in stylesheet.import_urls() {
            let mut identity = import.clone();
            identity.set_fragment(None);
            imports
                .entry(identity.as_str().to_owned())
                .or_insert_with(|| import.clone());
        }
    }
}
