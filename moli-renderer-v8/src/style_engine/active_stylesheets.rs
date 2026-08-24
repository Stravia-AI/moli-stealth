use std::sync::Arc;
use style::{
    font_face::FontFaceRule,
    servo_arc::Arc as ServoArc,
    shared_lock::{Locked, SharedRwLockReadGuard},
    stylesheets::DocumentStyleSheet,
    stylist::Stylist,
};

use crate::css_resource_urls::StylesheetLoadBlockingResource;

use super::source::store::{LiveStylesheetCascadeUpdate, StyloStylesheetSource};

#[cfg(test)]
thread_local! {
    static EXACT_RULE_CHANGE_NOTIFICATION_COUNT: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
    static FULL_CASCADE_UPDATE_FALLBACK_COUNT: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
pub(crate) fn reset_live_stylesheet_update_counts_for_test() {
    EXACT_RULE_CHANGE_NOTIFICATION_COUNT.with(|count| count.set(0));
    FULL_CASCADE_UPDATE_FALLBACK_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn exact_rule_change_notification_count_for_test() -> usize {
    EXACT_RULE_CHANGE_NOTIFICATION_COUNT.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn full_cascade_update_fallback_count_for_test() -> usize {
    FULL_CASCADE_UPDATE_FALLBACK_COUNT.with(std::cell::Cell::get)
}

pub(super) fn note_exact_rule_change_notifications(count: usize) {
    #[cfg(test)]
    EXACT_RULE_CHANGE_NOTIFICATION_COUNT.with(|current| {
        current.set(current.get().saturating_add(count));
    });
    #[cfg(not(test))]
    let _ = count;
}

pub(super) fn note_full_cascade_update_fallback() {
    #[cfg(test)]
    FULL_CASCADE_UPDATE_FALLBACK_COUNT.with(|count| {
        count.set(count.get().saturating_add(1));
    });
}

/// One stylesheet installation retained by a document or shadow-tree scope.
///
/// `source` carries the stable installation/revision identity used to diff a
/// later active-sheet list. `stylesheet` is the parsed Stylo object installed
/// in the owning `Stylist`/`AuthorStyles`; retaining it makes clean observations
/// and unrelated scope updates parse-free.
#[derive(Clone)]
pub(super) struct ActiveStylesheet {
    source: StyloStylesheetSource,
    stylesheet: DocumentStyleSheet,
    web_font_resources: Arc<[ActiveWebFontResource]>,
    import_urls: Arc<[url::Url]>,
}

/// One parsed `@font-face` resource tied to its native Stylo rule.
///
/// The strong rule reference cannot outlive its owning `ActiveStylesheet`, but
/// it prevents a removed rule's allocation address from being reused before
/// the next style observation reconciles this resource snapshot.
#[derive(Clone)]
pub(super) struct ActiveWebFontResource {
    /// Stable native-rule identity for device-aware resource projection.
    ///
    /// Projection compares this allocation's address with Stylo's current
    /// effective `@font-face` rules. Keeping a strong reference here pins that
    /// allocation until this `ActiveStylesheet` snapshot is reconciled, so a
    /// removed rule cannot release and recycle its address into a false match.
    /// The rule contents are not read through this field.
    rule: ServoArc<Locked<FontFaceRule>>,
    resource: StylesheetLoadBlockingResource,
}

impl ActiveWebFontResource {
    pub(super) fn new(
        rule: ServoArc<Locked<FontFaceRule>>,
        resource: StylesheetLoadBlockingResource,
    ) -> Self {
        Self { rule, resource }
    }

    pub(super) fn rule_address(&self) -> usize {
        self.rule.raw_ptr().as_ptr() as usize
    }

    pub(super) fn resource(&self) -> &StylesheetLoadBlockingResource {
        &self.resource
    }
}

impl ActiveStylesheet {
    pub(super) fn new(
        source: StyloStylesheetSource,
        stylesheet: DocumentStyleSheet,
        web_font_resources: Arc<[ActiveWebFontResource]>,
        import_urls: Arc<[url::Url]>,
    ) -> Self {
        Self {
            source,
            stylesheet,
            web_font_resources,
            import_urls,
        }
    }

    pub(super) fn source(&self) -> &StyloStylesheetSource {
        &self.source
    }

    pub(super) fn stylesheet(&self) -> &DocumentStyleSheet {
        &self.stylesheet
    }

    pub(super) fn web_font_resources(&self) -> &[ActiveWebFontResource] {
        self.web_font_resources.as_ref()
    }

    pub(super) fn import_urls(&self) -> &[url::Url] {
        self.import_urls.as_ref()
    }
}

#[derive(Default)]
pub(super) struct ActiveStylesheetCollection {
    entries: Vec<ActiveStylesheet>,
}

pub(super) struct ActiveStylesheetReconciliation {
    previous_stylesheets: Vec<DocumentStyleSheet>,
    stylesheet_set_changed: bool,
    stylesheet_removed: bool,
    in_place_updates: Vec<ActiveStylesheetInPlaceUpdate>,
}

pub(super) struct ActiveStylesheetInPlaceUpdate {
    stylesheet: DocumentStyleSheet,
    cascade_update: LiveStylesheetCascadeUpdate,
}

impl ActiveStylesheetReconciliation {
    pub(super) fn previous_stylesheets(&self) -> &[DocumentStyleSheet] {
        &self.previous_stylesheets
    }

    pub(super) fn stylesheet_set_changed(&self) -> bool {
        self.stylesheet_set_changed
    }

    pub(super) fn stylesheet_removed(&self) -> bool {
        self.stylesheet_removed
    }

    pub(super) fn in_place_updates(&self) -> &[ActiveStylesheetInPlaceUpdate] {
        &self.in_place_updates
    }
}

impl ActiveStylesheetInPlaceUpdate {
    pub(super) fn stylesheet(&self) -> &DocumentStyleSheet {
        &self.stylesheet
    }

    pub(super) fn cascade_update(&self) -> &LiveStylesheetCascadeUpdate {
        &self.cascade_update
    }
}

impl ActiveStylesheetCollection {
    pub(super) fn new(entries: Vec<ActiveStylesheet>) -> Self {
        Self { entries }
    }

    pub(super) fn entries(&self) -> &[ActiveStylesheet] {
        &self.entries
    }

    /// Returns the Stylo-facing cascade list.
    ///
    /// The adoptedStyleSheets observable array may contain the same sheet more
    /// than once, but Stylo's stylesheet set requires unique sheet objects.
    /// Earlier occurrences are cascade-inert because the last occurrence of
    /// an identical sheet has the highest position, so retain only that last
    /// occurrence while preserving its relative order with other sheets.
    pub(super) fn cascade_stylesheets(&self) -> Vec<DocumentStyleSheet> {
        cascade_stylesheets(&self.entries)
    }

    pub(super) fn matches_sources(&self, sources: &[StyloStylesheetSource]) -> bool {
        self.entries.len() == sources.len()
            && self
                .entries
                .iter()
                .zip(sources)
                .all(|(entry, source)| entry.source() == source)
    }

    /// Reconciles an ordered active list while retaining parsed stylesheet
    /// objects for unchanged installations, including reorder-only changes.
    pub(super) fn reconcile(
        &mut self,
        sources: &[StyloStylesheetSource],
        mut install: impl FnMut(&StyloStylesheetSource) -> ActiveStylesheet,
    ) -> Option<ActiveStylesheetReconciliation> {
        if self.matches_sources(sources) {
            return None;
        }

        let mut previous = std::mem::take(&mut self.entries);
        let previous_stylesheets = cascade_stylesheets(&previous);
        let mut in_place_updates = Vec::new();
        self.entries = sources
            .iter()
            .map(|source| {
                if let Some(index) = previous.iter().position(|entry| entry.source() == source) {
                    return previous.remove(index);
                }
                let Some(index) = previous
                    .iter()
                    .position(|entry| has_same_installation(entry.source(), source))
                else {
                    return install(source);
                };
                let previous_entry = previous.remove(index);
                if previous_entry.source().has_same_stylesheet_revision(source) {
                    return ActiveStylesheet::new(
                        source.clone(),
                        previous_entry.stylesheet().clone(),
                        Arc::clone(&previous_entry.web_font_resources),
                        Arc::clone(&previous_entry.import_urls),
                    );
                }
                let next_entry = install(source);
                if previous_entry.stylesheet() == next_entry.stylesheet() {
                    in_place_updates.push(ActiveStylesheetInPlaceUpdate {
                        stylesheet: next_entry.stylesheet().clone(),
                        cascade_update: source.live_cascade_update_since(previous_entry.source()),
                    });
                }
                next_entry
            })
            .collect();
        let next_stylesheets = self.cascade_stylesheets();
        let stylesheet_set_changed = previous_stylesheets != next_stylesheets;
        let stylesheet_removed = previous_stylesheets.iter().any(|stylesheet| {
            !next_stylesheets
                .iter()
                .any(|next_stylesheet| next_stylesheet == stylesheet)
        });
        Some(ActiveStylesheetReconciliation {
            previous_stylesheets,
            stylesheet_set_changed,
            stylesheet_removed,
            in_place_updates,
        })
    }
}

fn cascade_stylesheets(entries: &[ActiveStylesheet]) -> Vec<DocumentStyleSheet> {
    entries
        .iter()
        .enumerate()
        .filter(|(index, entry)| {
            !entries[index + 1..]
                .iter()
                .any(|later| later.stylesheet() == entry.stylesheet())
        })
        .map(|(_, entry)| entry.stylesheet().clone())
        .collect()
}

fn has_same_installation(previous: &StyloStylesheetSource, next: &StyloStylesheetSource) -> bool {
    match (previous.adopted_client_id(), next.adopted_client_id()) {
        (Some(previous), Some(next)) => previous == next,
        (Some(_), None) | (None, Some(_)) => false,
        (None, None) => previous.has_same_installation_identity(next),
    }
}

pub(super) enum StylesheetSetUpdate {
    Remove(DocumentStyleSheet),
    InsertBefore {
        stylesheet: DocumentStyleSheet,
        before: DocumentStyleSheet,
    },
    Append(DocumentStyleSheet),
}

pub(super) fn stylesheet_set_updates(
    previous: &[DocumentStyleSheet],
    next: &[DocumentStyleSheet],
) -> Vec<StylesheetSetUpdate> {
    let mut current = previous.to_vec();
    let mut updates = Vec::new();

    for index in (0..current.len()).rev() {
        if !next.contains(&current[index]) {
            updates.push(StylesheetSetUpdate::Remove(current.remove(index)));
        }
    }
    for (index, stylesheet) in next.iter().enumerate() {
        if current.get(index) == Some(stylesheet) {
            continue;
        }
        if let Some(current_index) = current.iter().position(|current| current == stylesheet) {
            let stylesheet = current.remove(current_index);
            updates.push(StylesheetSetUpdate::Remove(stylesheet));
        }
        if let Some(before) = current.get(index).cloned() {
            updates.push(StylesheetSetUpdate::InsertBefore {
                stylesheet: stylesheet.clone(),
                before,
            });
            current.insert(index, stylesheet.clone());
        } else {
            updates.push(StylesheetSetUpdate::Append(stylesheet.clone()));
            current.push(stylesheet.clone());
        }
    }
    while current.len() > next.len() {
        updates.push(StylesheetSetUpdate::Remove(
            current
                .pop()
                .expect("current stylesheet list cannot be empty here"),
        ));
    }
    updates
}

pub(super) fn update_document_stylesheet_set(
    stylist: &mut Stylist,
    previous: &[DocumentStyleSheet],
    next: &[DocumentStyleSheet],
    guard: &SharedRwLockReadGuard<'_>,
) {
    for update in stylesheet_set_updates(previous, next) {
        match update {
            StylesheetSetUpdate::Remove(stylesheet) => {
                stylist.remove_stylesheet(stylesheet, guard);
            }
            StylesheetSetUpdate::InsertBefore { stylesheet, before } => {
                stylist.insert_stylesheet_before(stylesheet, before, guard)
            }
            StylesheetSetUpdate::Append(stylesheet) => {
                stylist.append_stylesheet(stylesheet, guard);
            }
        }
    }
}

/// Publishes exact live-CSSOM changes into the persistent Document Stylist.
/// Returns `true` when at least one generation was not representable by the
/// journal and the caller must conservatively dirty the author origin.
pub(super) fn notify_document_stylesheet_rule_changes(
    stylist: &mut Stylist,
    reconciliation: &ActiveStylesheetReconciliation,
    guard: &SharedRwLockReadGuard<'_>,
) -> bool {
    if reconciliation
        .in_place_updates()
        .iter()
        .any(|update| matches!(update.cascade_update(), LiveStylesheetCascadeUpdate::Full))
    {
        note_full_cascade_update_fallback();
        return true;
    }
    for update in reconciliation.in_place_updates() {
        let LiveStylesheetCascadeUpdate::Rules(changes) = update.cascade_update() else {
            unreachable!("full updates were handled above");
        };
        note_exact_rule_change_notifications(changes.len());
        for change in changes {
            let ancestors = change
                .ancestors()
                .iter()
                .map(style::stylesheets::CssRuleRef::from)
                .collect::<Vec<_>>();
            stylist.rule_changed(
                update.stylesheet(),
                change.rule(),
                guard,
                change.change_kind(),
                &ancestors,
            );
        }
    }
    false
}
