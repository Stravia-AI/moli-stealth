use std::{cell::Ref, ops::Deref};

use moli_selector::StyloStyleSourceScope as StyleSourceScope;

use crate::{document_runtime::DomHandle, dom::native::DomHost};

use super::{
    StyleSourceId, StyleViewport,
    document_world::DocumentStyleWorld,
    request::RetainedSourceDependencyRequestPlan,
    source::adopted::AdoptedStyleSheetSources,
    source::linked::LinkedStylesheetSources,
    source_lifecycle::{StyleSourceDocumentContext, StyleSourceLifecycleReport},
    source_owner_text::OwnerStyleSheetSources,
    source_record::{MatchingStyleDependencySource, RetainedStylesheetSourceRecord},
    stylesheet::moli_user_agent_source_dependency_summary,
};

pub(super) struct DocumentStyleSourceStores<'a> {
    document: DomHandle,
    linked_stylesheet_sources: StyleSourceStoreBorrow<'a, LinkedStylesheetSources>,
    owner_style_sheet_sources: StyleSourceStoreBorrow<'a, OwnerStyleSheetSources>,
    adopted_style_sheet_sources: StyleSourceStoreBorrow<'a, AdoptedStyleSheetSources>,
}

enum StyleSourceStoreBorrow<'a, T> {
    #[cfg(test)]
    Borrowed(&'a T),
    Ref(Ref<'a, T>),
}

impl<T> Deref for StyleSourceStoreBorrow<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self {
            #[cfg(test)]
            Self::Borrowed(value) => value,
            Self::Ref(value) => value,
        }
    }
}

impl DocumentStyleWorld {
    pub(super) fn borrow_source_stores(&self) -> DocumentStyleSourceStores<'_> {
        DocumentStyleSourceStores {
            document: self.document,
            linked_stylesheet_sources: StyleSourceStoreBorrow::Ref(
                self.linked_stylesheet_sources.borrow(),
            ),
            owner_style_sheet_sources: StyleSourceStoreBorrow::Ref(
                self.owner_style_sheet_sources.borrow(),
            ),
            adopted_style_sheet_sources: StyleSourceStoreBorrow::Ref(
                self.adopted_style_sheet_sources.borrow(),
            ),
        }
    }
}

impl<'a> DocumentStyleSourceStores<'a> {
    #[cfg(test)]
    pub(super) fn borrowed_for_test(
        document: DomHandle,
        linked_stylesheet_sources: &'a LinkedStylesheetSources,
        owner_style_sheet_sources: &'a OwnerStyleSheetSources,
        adopted_style_sheet_sources: &'a AdoptedStyleSheetSources,
    ) -> Self {
        Self {
            document,
            linked_stylesheet_sources: StyleSourceStoreBorrow::Borrowed(linked_stylesheet_sources),
            owner_style_sheet_sources: StyleSourceStoreBorrow::Borrowed(owner_style_sheet_sources),
            adopted_style_sheet_sources: StyleSourceStoreBorrow::Borrowed(
                adopted_style_sheet_sources,
            ),
        }
    }

    pub(super) fn document(&self) -> DomHandle {
        self.document
    }

    pub(super) fn adopted_sources(&self) -> &AdoptedStyleSheetSources {
        &self.adopted_style_sheet_sources
    }

    pub(super) fn has_document_retained_sources(&self, host: &DomHost) -> bool {
        self.linked_stylesheet_sources.has_retained_sources(host)
            || self
                .owner_style_sheet_sources
                .has_document_retained_sources_for_document(host, self.document)
            || self
                .adopted_sources()
                .has_document_retained_sources(host, self.document)
    }

    pub(super) fn has_dependency_match_for_request_plan(
        &self,
        host: &DomHost,
        request_plan: &RetainedSourceDependencyRequestPlan,
    ) -> bool {
        request_plan
            .matches_dependency_summary(moli_user_agent_source_dependency_summary().as_ref())
            || self
                .linked_stylesheet_sources
                .has_dependency_match_for_store(host, request_plan)
            || self
                .owner_style_sheet_sources
                .has_dependency_match_for_document(host, self.document, request_plan)
            || self.adopted_sources().has_dependency_match_for_document(
                host,
                self.document,
                request_plan,
            )
    }

    pub(super) fn source_lifecycle_report(
        &self,
        host: &DomHost,
        document_context: StyleSourceDocumentContext<'_>,
    ) -> StyleSourceLifecycleReport {
        let mut entries = self
            .linked_stylesheet_sources
            .tracked_lifecycle_entries(host)
            .collect::<Vec<_>>();
        entries.extend(
            self.owner_style_sheet_sources
                .tracked_lifecycle_entries_for_document(host, self.document),
        );
        entries.extend(
            self.adopted_sources()
                .tracked_lifecycle_entries_for_document(host, self.document),
        );
        StyleSourceLifecycleReport::from_tracked_entries(host, document_context, entries)
    }

    pub(super) fn source_lifecycle_report_for_source_ids(
        &self,
        host: &DomHost,
        document_context: StyleSourceDocumentContext<'_>,
        source_ids: impl IntoIterator<Item = StyleSourceId>,
    ) -> StyleSourceLifecycleReport {
        let source_ids = source_ids.into_iter().collect::<Vec<_>>();
        let mut entries = self
            .linked_stylesheet_sources
            .tracked_lifecycle_entries_for_source_ids(host, source_ids.iter().cloned())
            .collect::<Vec<_>>();
        entries.extend(
            self.owner_style_sheet_sources
                .tracked_lifecycle_entries_for_source_ids(host, source_ids.iter().cloned()),
        );
        entries.extend(
            self.adopted_sources()
                .tracked_lifecycle_entries_for_source_ids(host, source_ids.iter().cloned()),
        );
        StyleSourceLifecycleReport::from_tracked_entries(host, document_context, entries)
    }

    pub(super) fn matching_dependency_sources(
        &self,
        host: &DomHost,
        source_scope: &StyleSourceScope,
        emulated_media: &crate::protocol_types::EmulatedMediaOverrides,
        viewport: StyleViewport,
    ) -> Vec<MatchingStyleDependencySource> {
        let mut sources = MatchingStyleDependencySource::user_agent(
            host,
            self.document,
            moli_user_agent_source_dependency_summary(),
            source_scope,
        )
        .into_iter()
        .collect::<Vec<_>>();
        sources.extend(
            self.linked_stylesheet_sources
                .matching_dependency_sources_for_store(
                    host,
                    source_scope,
                    emulated_media,
                    viewport,
                ),
        );
        sources.extend(
            self.owner_style_sheet_sources
                .matching_dependency_sources_for_document(
                    host,
                    self.document,
                    source_scope,
                    emulated_media,
                    viewport,
                ),
        );
        sources.extend(
            self.adopted_sources()
                .matching_dependency_sources_for_document(
                    host,
                    self.document,
                    source_scope,
                    emulated_media,
                    viewport,
                ),
        );
        sources
    }

    pub(super) fn retained_source_records_for_lifecycle(
        &self,
        host: &DomHost,
        source_lifecycle: &StyleSourceLifecycleReport,
    ) -> Vec<RetainedStylesheetSourceRecord<'_>> {
        let mut records = self
            .linked_stylesheet_sources
            .retained_source_records_for_lifecycle(host, source_lifecycle);
        records.extend(
            self.owner_style_sheet_sources
                .retained_source_records_for_lifecycle(host, source_lifecycle),
        );
        records.extend(
            self.adopted_sources()
                .retained_source_records_for_lifecycle(source_lifecycle),
        );
        records
    }
}
