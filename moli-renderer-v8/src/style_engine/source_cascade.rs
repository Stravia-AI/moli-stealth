use std::collections::{HashMap, HashSet};

use style::{
    author_styles::AuthorStyles,
    servo_arc::Arc as ServoArc,
    shared_lock::SharedRwLock,
    stylesheets::{CustomMediaMap, DocumentStyleSheet},
    stylist::{CascadeData, Stylist},
};

use super::{
    active_stylesheets::{ActiveStylesheet, ActiveStylesheetCollection},
    shadow_scopes::ShadowScopeStyles,
    source::store::{StyloStylesheetSource, stylesheet_sources_cache_key},
    source_id::{StyleScopeId, StyleSourceId},
    source_key::StyleSourceSetKey,
    source_record::RetainedStylesheetSourceRecord,
    state::RetainedStyleSystem,
};

type SourceCascadeMaps = (
    HashMap<StyleSourceId, ServoArc<CascadeData>>,
    HashMap<StyleSourceId, StyleSourceSetKey>,
);

type InstalledSourceGroup = (Vec<StyloStylesheetSource>, Vec<DocumentStyleSheet>);

#[cfg(test)]
thread_local! {
    static SOURCE_CASCADE_REBUILD_COUNT: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
pub(crate) fn reset_source_cascade_rebuild_count_for_test() {
    SOURCE_CASCADE_REBUILD_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn source_cascade_rebuild_count_for_test() -> usize {
    SOURCE_CASCADE_REBUILD_COUNT.with(std::cell::Cell::get)
}

/// Builds the per-source cascade projection used by CSSOM target queries.
///
/// The document and shadow collections remain the canonical installed-sheet
/// state. Source records are included only when CSSOM still owns a detached or
/// otherwise inactive sheet that must remain queryable.
pub(super) fn build_source_cascade_data(
    stylist: &mut Stylist,
    shared_lock: &SharedRwLock,
    document_stylesheets: &ActiveStylesheetCollection,
    shadow_scopes: &[ShadowScopeStyles],
    retained_source_records: &[RetainedStylesheetSourceRecord<'_>],
    previous: Option<(&SourceCascadeData, &SourceCascadeKeys)>,
    mut install: impl FnMut(&StyloStylesheetSource) -> ActiveStylesheet,
) -> SourceCascadeMaps {
    let mut sources_by_id =
        installed_sources_by_id(document_stylesheets, shadow_scopes, None, None);
    add_retained_source_records(
        &mut sources_by_id,
        retained_source_records,
        None,
        None,
        &mut install,
    );

    let mut cascade_data = HashMap::with_capacity(sources_by_id.len());
    let mut cascade_keys = HashMap::with_capacity(sources_by_id.len());
    for (source_id, (sources, stylesheets)) in sources_by_id {
        let key = stylesheet_sources_cache_key(&sources);
        let retained_data = previous.and_then(|(previous_data, previous_keys)| {
            (previous_keys.get(&source_id) == Some(&key))
                .then(|| previous_data.get(&source_id).cloned())
                .flatten()
                .filter(|data| {
                    source_cascade_matches_device(data, &stylesheets, stylist, shared_lock)
                })
        });
        let data = retained_data
            .unwrap_or_else(|| build_author_cascade_data(stylist, shared_lock, &stylesheets));
        cascade_keys.insert(source_id.clone(), key);
        cascade_data.insert(source_id, data);
    }
    (cascade_data, cascade_keys)
}

/// Reprojects only sources owned by dirty TreeScopes. Entries belonging to a
/// clean scope retain both their key and `CascadeData` allocation.
pub(super) fn update_source_cascade_data_for_scopes(
    retained: &mut RetainedStyleSystem,
    shared_lock: &SharedRwLock,
    retained_source_records: &[RetainedStylesheetSourceRecord<'_>],
    dirty_source_ids: &HashSet<StyleSourceId>,
    dirty_scopes: &HashSet<StyleScopeId>,
    device_changed: bool,
    mut install: impl FnMut(&StyloStylesheetSource) -> ActiveStylesheet,
) {
    if dirty_source_ids.is_empty() && dirty_scopes.is_empty() && !device_changed {
        return;
    }

    let previous_data = std::mem::take(&mut retained.source_cascade_data);
    let previous_keys = std::mem::take(&mut retained.source_cascade_keys);
    let mut next_data = if device_changed {
        HashMap::new()
    } else {
        retain_clean_entries(&previous_data, dirty_source_ids, dirty_scopes)
    };
    let mut next_keys = if device_changed {
        HashMap::new()
    } else {
        retain_clean_entries(&previous_keys, dirty_source_ids, dirty_scopes)
    };
    let source_filter = (!device_changed).then_some((dirty_source_ids, dirty_scopes));

    let mut sources_by_id = installed_sources_by_id(
        &retained.document_stylesheets,
        &retained.shadow_scopes,
        source_filter.map(|(source_ids, _)| source_ids),
        source_filter.map(|(_, scopes)| scopes),
    );
    add_retained_source_records(
        &mut sources_by_id,
        retained_source_records,
        source_filter.map(|(source_ids, _)| source_ids),
        source_filter.map(|(_, scopes)| scopes),
        &mut install,
    );

    for (source_id, (sources, stylesheets)) in sources_by_id {
        let key = stylesheet_sources_cache_key(&sources);
        let data = if previous_keys.get(&source_id) == Some(&key) {
            previous_data.get(&source_id).cloned().filter(|data| {
                source_cascade_matches_device(data, &stylesheets, &retained.stylist, shared_lock)
            })
        } else {
            None
        }
        .unwrap_or_else(|| {
            build_author_cascade_data(&mut retained.stylist, shared_lock, &stylesheets)
        });
        next_keys.insert(source_id.clone(), key);
        next_data.insert(source_id, data);
    }
    retained.source_cascade_data = next_data;
    retained.source_cascade_keys = next_keys;
}

fn source_cascade_matches_device(
    data: &CascadeData,
    stylesheets: &[DocumentStyleSheet],
    stylist: &Stylist,
    shared_lock: &SharedRwLock,
) -> bool {
    let guard = shared_lock.read();
    stylesheets.iter().all(|stylesheet| {
        data.media_feature_affected_matches(
            stylesheet,
            &guard,
            stylist.device(),
            stylist.quirks_mode(),
        )
    })
}

type SourceCascadeData = HashMap<StyleSourceId, ServoArc<CascadeData>>;
type SourceCascadeKeys = HashMap<StyleSourceId, StyleSourceSetKey>;

fn installed_sources_by_id(
    document_stylesheets: &ActiveStylesheetCollection,
    shadow_scopes: &[ShadowScopeStyles],
    dirty_source_ids: Option<&HashSet<StyleSourceId>>,
    dirty_scopes: Option<&HashSet<StyleScopeId>>,
) -> HashMap<StyleSourceId, InstalledSourceGroup> {
    let mut sources_by_id = HashMap::<StyleSourceId, InstalledSourceGroup>::new();
    for entry in document_stylesheets.entries().iter().chain(
        shadow_scopes
            .iter()
            .flat_map(|scope| scope.active_stylesheets().entries()),
    ) {
        let Some(source_id) = entry.source().source_id().cloned() else {
            continue;
        };
        if !source_needs_projection(&source_id, dirty_source_ids, dirty_scopes) {
            continue;
        }
        let (sources, stylesheets) = sources_by_id.entry(source_id).or_default();
        if let Some(index) = stylesheets
            .iter()
            .position(|stylesheet| stylesheet == entry.stylesheet())
        {
            sources.remove(index);
            stylesheets.remove(index);
        }
        sources.push(entry.source().clone());
        stylesheets.push(entry.stylesheet().clone());
    }
    sources_by_id
}

fn add_retained_source_records(
    sources_by_id: &mut HashMap<StyleSourceId, InstalledSourceGroup>,
    records: &[RetainedStylesheetSourceRecord<'_>],
    dirty_source_ids: Option<&HashSet<StyleSourceId>>,
    dirty_scopes: Option<&HashSet<StyleScopeId>>,
    install: &mut impl FnMut(&StyloStylesheetSource) -> ActiveStylesheet,
) {
    for record in records {
        if !source_needs_projection(record.id(), dirty_source_ids, dirty_scopes)
            || sources_by_id.contains_key(record.id())
        {
            continue;
        }
        let source = record.to_stylo_source();
        let installed = install(&source);
        sources_by_id.insert(
            record.id().clone(),
            (vec![source], vec![installed.stylesheet().clone()]),
        );
    }
}

fn retain_clean_entries<T: Clone>(
    previous: &HashMap<StyleSourceId, T>,
    dirty_source_ids: &HashSet<StyleSourceId>,
    dirty_scopes: &HashSet<StyleScopeId>,
) -> HashMap<StyleSourceId, T> {
    previous
        .iter()
        .filter(|(source_id, _)| {
            !dirty_source_ids.contains(*source_id) && !dirty_scopes.contains(&source_id.scope_id)
        })
        .map(|(source_id, value)| (source_id.clone(), value.clone()))
        .collect()
}

fn source_needs_projection(
    source_id: &StyleSourceId,
    dirty_source_ids: Option<&HashSet<StyleSourceId>>,
    dirty_scopes: Option<&HashSet<StyleScopeId>>,
) -> bool {
    match (dirty_source_ids, dirty_scopes) {
        (None, None) => true,
        (dirty_source_ids, dirty_scopes) => {
            dirty_source_ids.is_some_and(|ids| ids.contains(source_id))
                || dirty_scopes.is_some_and(|scopes| scopes.contains(&source_id.scope_id))
        }
    }
}

fn build_author_cascade_data(
    stylist: &mut Stylist,
    shared_lock: &SharedRwLock,
    stylesheets: &[DocumentStyleSheet],
) -> ServoArc<CascadeData> {
    #[cfg(test)]
    SOURCE_CASCADE_REBUILD_COUNT.with(|count| count.set(count.get().saturating_add(1)));
    let mut author_styles = AuthorStyles::<DocumentStyleSheet>::new();
    let custom_media = CustomMediaMap::default();
    let guard = shared_lock.read();
    for stylesheet in stylesheets {
        author_styles.stylesheets.append_stylesheet(
            None,
            &custom_media,
            stylesheet.clone(),
            &guard,
        );
    }
    author_styles.flush(stylist, &guard);
    author_styles.data
}
