use std::rc::Rc;

use style::context::QuirksMode;

use crate::document_runtime::DomHandle;

use super::{
    CssCustomPropertyRegistrationRecord, StyleWorldEnvironment, StyloStyleEnvironment,
    StyloStylesheetSource,
};

/// Complete source projection used to construct a fresh Document style world.
///
/// Normal observations use `IncrementalStyleWorldUpdate`; this full snapshot
/// is reserved for the first observation, true world replacement, isolated
/// provenance queries, and the full-rebuild test oracle.
#[derive(Clone, Debug)]
pub(crate) struct FullStyleWorldSnapshot {
    pub(crate) document_stylesheet_sources: Vec<StyloStylesheetSource>,
    pub(crate) shadow_stylesheet_sources: Vec<(DomHandle, Vec<StyloStylesheetSource>)>,
    pub(crate) script_custom_property_registrations: Vec<CssCustomPropertyRegistrationRecord>,
    pub(crate) environment: StyloStyleEnvironment,
    pub(crate) quirks_mode: QuirksMode,
}

impl FullStyleWorldSnapshot {
    /// Makes a snapshot safe to consume under a different Stylo
    /// `SharedRwLock`. Text sources already have no lock-bound rule tree;
    /// live sources are converted to independently parsed text projections.
    pub(crate) fn independent_style_engine_projection(&self) -> Self {
        let project_sources = |sources: &[StyloStylesheetSource]| {
            sources
                .iter()
                .map(StyloStylesheetSource::independent_text_projection)
                .collect()
        };
        Self {
            document_stylesheet_sources: project_sources(&self.document_stylesheet_sources),
            shadow_stylesheet_sources: self
                .shadow_stylesheet_sources
                .iter()
                .map(|(root, sources)| (*root, project_sources(sources)))
                .collect(),
            script_custom_property_registrations: self.script_custom_property_registrations.clone(),
            environment: self.environment,
            quirks_mode: self.quirks_mode,
        }
    }
}

impl Default for FullStyleWorldSnapshot {
    fn default() -> Self {
        Self {
            document_stylesheet_sources: Vec::new(),
            shadow_stylesheet_sources: Vec::new(),
            script_custom_property_registrations: Vec::new(),
            environment: StyloStyleEnvironment::default(),
            quirks_mode: QuirksMode::NoQuirks,
        }
    }
}

/// Work that must be materialized before observing a retained Document style
/// world.
///
/// A full update is reserved for the first observation and true world
/// replacement. Incremental updates name the exact stylesheet scopes whose
/// ordered active collections must be refreshed. Device-only updates carry no
/// stylesheet vectors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StyleWorldUpdatePlan {
    Full,
    Incremental(IncrementalStyleWorldUpdatePlan),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct IncrementalStyleWorldUpdatePlan {
    document_stylesheets: bool,
    shadow_stylesheet_roots: Vec<DomHandle>,
    connected_shadow_roots: Option<Vec<DomHandle>>,
    custom_property_registrations: bool,
}

impl IncrementalStyleWorldUpdatePlan {
    pub(super) fn new(
        document_stylesheets: bool,
        shadow_stylesheet_roots: Vec<DomHandle>,
        connected_shadow_roots: Option<Vec<DomHandle>>,
        custom_property_registrations: bool,
    ) -> Self {
        Self {
            document_stylesheets,
            shadow_stylesheet_roots,
            connected_shadow_roots,
            custom_property_registrations,
        }
    }

    pub(crate) fn refreshes_document_stylesheets(&self) -> bool {
        self.document_stylesheets
    }

    pub(crate) fn shadow_stylesheet_roots(&self) -> &[DomHandle] {
        &self.shadow_stylesheet_roots
    }

    pub(crate) fn connected_shadow_roots(&self) -> Option<&[DomHandle]> {
        self.connected_shadow_roots.as_deref()
    }

    pub(crate) fn refreshes_custom_property_registrations(&self) -> bool {
        self.custom_property_registrations
    }
}

#[derive(Clone, Debug)]
pub(crate) struct IncrementalStyleWorldUpdate {
    pub(super) document_stylesheet_sources: Option<Vec<StyloStylesheetSource>>,
    pub(super) shadow_stylesheet_sources: Vec<(DomHandle, Vec<StyloStylesheetSource>)>,
    pub(super) connected_shadow_roots: Option<Vec<DomHandle>>,
    pub(super) script_custom_property_registrations:
        Option<Vec<CssCustomPropertyRegistrationRecord>>,
}

impl IncrementalStyleWorldUpdate {
    pub(crate) fn new(
        document_stylesheet_sources: Option<Vec<StyloStylesheetSource>>,
        shadow_stylesheet_sources: Vec<(DomHandle, Vec<StyloStylesheetSource>)>,
        connected_shadow_roots: Option<Vec<DomHandle>>,
        script_custom_property_registrations: Option<Vec<CssCustomPropertyRegistrationRecord>>,
    ) -> Self {
        Self {
            document_stylesheet_sources,
            shadow_stylesheet_sources,
            connected_shadow_roots,
            script_custom_property_registrations,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum StyleWorldUpdate {
    Full(Rc<FullStyleWorldSnapshot>),
    Incremental(IncrementalStyleWorldUpdate),
}

/// An observation-local update batch. It contains only data requested by the
/// retained world's dirty plan and never outlives the synchronous observation.
#[derive(Clone, Debug)]
pub(crate) struct PreparedStyleWorldUpdate {
    environment: StyleWorldEnvironment,
    update: StyleWorldUpdate,
}

impl PreparedStyleWorldUpdate {
    pub(crate) fn full(
        environment: StyleWorldEnvironment,
        inputs: Rc<FullStyleWorldSnapshot>,
    ) -> Self {
        Self {
            environment,
            update: StyleWorldUpdate::Full(inputs),
        }
    }

    pub(crate) fn incremental(
        environment: StyleWorldEnvironment,
        update: IncrementalStyleWorldUpdate,
    ) -> Self {
        Self {
            environment,
            update: StyleWorldUpdate::Incremental(update),
        }
    }

    pub(super) fn environment(&self) -> &StyleWorldEnvironment {
        &self.environment
    }

    pub(super) fn update(&self) -> &StyleWorldUpdate {
        &self.update
    }
}
