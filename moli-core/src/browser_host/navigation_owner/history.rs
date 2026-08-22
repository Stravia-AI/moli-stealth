use crate::page::SameDocumentHistoryUpdate;

/// Current browser Page fields used when creating a joint history entry.
///
/// This is a Browser Core input rather than a CDP `NavigationEntry`; entry ids,
/// transition state, and document sequence numbers remain core-owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserNavigationHistoryPageSnapshot {
    url: String,
    title: String,
}

impl BrowserNavigationHistoryPageSnapshot {
    pub fn new(url: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            title: title.into(),
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub(super) fn into_typed_entry(self, id: i32) -> BrowserNavigationHistoryEntry {
        BrowserNavigationHistoryEntry {
            id,
            user_typed_url: self.url.clone(),
            url: self.url,
            title: self.title,
            transition_type: "typed".to_owned(),
            document_sequence_number: None,
        }
    }
}

/// Optional first entry used when a target history is materialized lazily.
///
/// Browser Core derives initial-empty-Document entries from its Target
/// creation registry. During physical Page migration, a frontend adapter may
/// still provide a loaded-Page snapshot fallback; Core allocates every entry
/// and document-sequence identity in either case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserNavigationHistorySeed {
    url: String,
    user_typed_url: String,
    title: String,
    transition_type: String,
}

impl BrowserNavigationHistorySeed {
    pub(super) fn initial_empty_document(url: impl Into<String>) -> Self {
        let url = url.into();
        Self {
            user_typed_url: url.clone(),
            url,
            title: String::new(),
            transition_type: "auto_toplevel".to_owned(),
        }
    }

    pub fn page_snapshot(snapshot: BrowserNavigationHistoryPageSnapshot) -> Self {
        Self {
            user_typed_url: snapshot.url.clone(),
            url: snapshot.url,
            title: snapshot.title,
            transition_type: "typed".to_owned(),
        }
    }

    pub(super) fn into_entry(self, id: i32) -> BrowserNavigationHistoryEntry {
        BrowserNavigationHistoryEntry {
            id,
            url: self.url,
            user_typed_url: self.user_typed_url,
            title: self.title,
            transition_type: self.transition_type,
            document_sequence_number: None,
        }
    }
}

/// Browser-owned representation of one joint session-history entry.
///
/// Protocol frontends may project these fields into their own wire shape, but
/// they do not own the cursor, entry allocation, or pending traversal state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserNavigationHistoryEntry {
    pub id: i32,
    pub url: String,
    pub user_typed_url: String,
    pub title: String,
    pub transition_type: String,
    pub document_sequence_number: Option<u64>,
}

/// Protocol-neutral destination for one joint session-history traversal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserHistoryTraversalDestination {
    Entry(i32),
    Delta(i64),
}

/// Browser-owned resolution of one history traversal destination.
///
/// `same_document_delta` is present only when the current and destination
/// entries belong to the same exact Document sequence. Frontends must not
/// infer this from URLs or from a snapshot taken before Browser Owner selects
/// the command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserHistoryTraversalResolution {
    Noop {
        entry_id: i32,
        url: String,
    },
    Entry {
        entry_id: i32,
        url: String,
        same_document_delta: Option<i64>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserHistoryTraversalResolutionError {
    NoSuchHistoryEntry,
}

impl std::fmt::Display for BrowserHistoryTraversalResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchHistoryEntry => formatter.write_str("no such history entry"),
        }
    }
}

impl std::error::Error for BrowserHistoryTraversalResolutionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserNavigationHistoryUpdate {
    ReplaceCurrent,
    ReplaceInitialEmptyDocument,
    TraverseToEntry(i32),
}

/// Why a renderer-completed same-Document history update could not be
/// reconciled with the Browser-owned joint session history.
///
/// Rejection is atomic: callers may publish neither a target URL projection
/// nor a navigation fact when this result is returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserSameDocumentHistoryUpdateError {
    CurrentEntryUnavailable,
    TraversalIndexOverflow {
        current_index: usize,
        delta: i64,
    },
    TraversalTargetOutsideHistory {
        current_index: usize,
        delta: i64,
        entry_count: usize,
    },
    TraversalUrlMismatch {
        target_index: usize,
        browser_url: String,
        renderer_url: String,
    },
}

impl std::fmt::Display for BrowserSameDocumentHistoryUpdateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CurrentEntryUnavailable => {
                formatter.write_str("same-Document traversal has no current history entry")
            }
            Self::TraversalIndexOverflow {
                current_index,
                delta,
            } => write!(
                formatter,
                "same-Document traversal index overflow: current_index={current_index}, delta={delta}"
            ),
            Self::TraversalTargetOutsideHistory {
                current_index,
                delta,
                entry_count,
            } => write!(
                formatter,
                "same-Document traversal target is outside history: current_index={current_index}, delta={delta}, entry_count={entry_count}"
            ),
            Self::TraversalUrlMismatch {
                target_index,
                browser_url,
                renderer_url,
            } => write!(
                formatter,
                "same-Document traversal URL diverged at index {target_index}: browser={browser_url:?}, renderer={renderer_url:?}"
            ),
        }
    }
}

impl std::error::Error for BrowserSameDocumentHistoryUpdateError {}

/// Authoritative browser state for one target's joint session history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserNavigationHistory {
    entries: Vec<BrowserNavigationHistoryEntry>,
    current_index: Option<usize>,
    next_entry_id: i32,
    next_document_sequence_number: u64,
    pending_update: Option<BrowserNavigationHistoryUpdate>,
}

impl Default for BrowserNavigationHistory {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            current_index: None,
            next_entry_id: 1,
            next_document_sequence_number: 1,
            pending_update: None,
        }
    }
}

impl BrowserNavigationHistory {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn allocate_entry_id(&mut self) -> i32 {
        let id = self.next_entry_id;
        self.next_entry_id = self.next_entry_id.saturating_add(1);
        id
    }

    fn push_entry(&mut self, entry: BrowserNavigationHistoryEntry) {
        if let Some(current_index) = self.current_index {
            self.entries.truncate(current_index + 1);
        }
        self.entries.push(entry);
        self.current_index = self.entries.len().checked_sub(1);
    }

    fn allocate_document_sequence_number(&mut self) -> u64 {
        let sequence_number = self.next_document_sequence_number;
        self.next_document_sequence_number = self.next_document_sequence_number.saturating_add(1);
        sequence_number
    }

    fn assign_new_document_sequence_number(&mut self, entry: &mut BrowserNavigationHistoryEntry) {
        entry.document_sequence_number = Some(self.allocate_document_sequence_number());
    }

    fn assign_current_document_sequence_number(
        &mut self,
        entry: &mut BrowserNavigationHistoryEntry,
    ) {
        entry.document_sequence_number = self
            .current_index
            .and_then(|index| self.entries.get(index))
            .and_then(|entry| entry.document_sequence_number)
            .or_else(|| Some(self.allocate_document_sequence_number()));
    }

    fn replace_current_entry(&mut self, mut entry: BrowserNavigationHistoryEntry) {
        if let Some(current_index) = self.current_index
            && let Some(current_entry) = self.entries.get_mut(current_index)
        {
            entry.id = current_entry.id;
            *current_entry = entry;
            return;
        }
        self.push_entry(entry);
    }

    fn traverse_to_entry(
        &mut self,
        entry_id: i32,
        mut loaded_entry: BrowserNavigationHistoryEntry,
    ) {
        if let Some(index) = self.entries.iter().position(|entry| entry.id == entry_id) {
            loaded_entry.transition_type = self.entries[index].transition_type.clone();
            loaded_entry.user_typed_url = self.entries[index].user_typed_url.clone();
            loaded_entry.document_sequence_number = self.entries[index].document_sequence_number;
            loaded_entry.id = entry_id;
            self.entries[index] = loaded_entry;
            self.current_index = Some(index);
            return;
        }
        self.push_entry(loaded_entry);
    }

    pub fn mark_replace_current(&mut self) {
        self.pending_update = Some(BrowserNavigationHistoryUpdate::ReplaceCurrent);
    }

    pub fn mark_replace_initial_empty_document(&mut self) {
        self.pending_update = Some(BrowserNavigationHistoryUpdate::ReplaceInitialEmptyDocument);
    }

    pub fn mark_traverse_to_entry(&mut self, entry_id: i32) {
        self.pending_update = Some(BrowserNavigationHistoryUpdate::TraverseToEntry(entry_id));
    }

    pub fn clear_pending_update(&mut self) {
        self.pending_update = None;
    }

    pub fn snapshot(&self) -> (usize, Vec<BrowserNavigationHistoryEntry>) {
        (self.current_index.unwrap_or(0), self.entries.clone())
    }

    /// Resolves and classifies a traversal without mutating the cursor.
    ///
    /// Browser Core owns both the current cursor and Document sequence
    /// identity, so this decision must remain adjacent to that state rather
    /// than being reconstructed by a protocol frontend from `snapshot()`.
    pub fn resolve_traversal(
        &self,
        destination: BrowserHistoryTraversalDestination,
    ) -> Result<BrowserHistoryTraversalResolution, BrowserHistoryTraversalResolutionError> {
        let current_index = self.current_index.unwrap_or(0);
        let target_index = match destination {
            BrowserHistoryTraversalDestination::Entry(entry_id) => self
                .entries
                .iter()
                .position(|entry| entry.id == entry_id)
                .ok_or(BrowserHistoryTraversalResolutionError::NoSuchHistoryEntry)?,
            BrowserHistoryTraversalDestination::Delta(delta) => {
                let target_index = current_index as i128 + i128::from(delta);
                if target_index < 0 || target_index >= self.entries.len() as i128 {
                    return Err(BrowserHistoryTraversalResolutionError::NoSuchHistoryEntry);
                }
                usize::try_from(target_index)
                    .map_err(|_| BrowserHistoryTraversalResolutionError::NoSuchHistoryEntry)?
            }
        };
        let target_entry = self
            .entries
            .get(target_index)
            .ok_or(BrowserHistoryTraversalResolutionError::NoSuchHistoryEntry)?;
        if target_index == current_index {
            return Ok(BrowserHistoryTraversalResolution::Noop {
                entry_id: target_entry.id,
                url: target_entry.url.clone(),
            });
        }
        let current_entry = self
            .entries
            .get(current_index)
            .ok_or(BrowserHistoryTraversalResolutionError::NoSuchHistoryEntry)?;
        let same_document_delta = (current_entry.document_sequence_number.is_some()
            && current_entry.document_sequence_number == target_entry.document_sequence_number)
            .then(|| {
                let current_index = i64::try_from(current_index)
                    .map_err(|_| BrowserHistoryTraversalResolutionError::NoSuchHistoryEntry)?;
                let target_index = i64::try_from(target_index)
                    .map_err(|_| BrowserHistoryTraversalResolutionError::NoSuchHistoryEntry)?;
                Ok::<_, BrowserHistoryTraversalResolutionError>(target_index - current_index)
            })
            .transpose()?;
        Ok(BrowserHistoryTraversalResolution::Entry {
            entry_id: target_entry.id,
            url: target_entry.url.clone(),
            same_document_delta,
        })
    }

    pub fn can_prune_all_but_current(&self) -> bool {
        !matches!(
            self.pending_update,
            Some(BrowserNavigationHistoryUpdate::TraverseToEntry(_))
        ) && self
            .current_index
            .is_some_and(|current_index| current_index < self.entries.len())
    }

    pub fn prune_all_but_current(&mut self) -> bool {
        if !self.can_prune_all_but_current() {
            return false;
        }
        let Some(current_index) = self.current_index else {
            return false;
        };
        let Some(current_entry) = self.entries.get(current_index).cloned() else {
            return false;
        };
        self.entries.clear();
        self.entries.push(current_entry);
        self.current_index = Some(0);
        true
    }

    pub fn seed_entry(&mut self, mut entry: BrowserNavigationHistoryEntry) {
        self.assign_new_document_sequence_number(&mut entry);
        self.push_entry(entry);
    }

    pub fn record_loaded_entry(&mut self, mut entry: BrowserNavigationHistoryEntry) {
        match self.pending_update.take() {
            Some(BrowserNavigationHistoryUpdate::ReplaceCurrent) => {
                entry.transition_type = "reload".to_owned();
                if let Some(current_entry) =
                    self.current_index.and_then(|index| self.entries.get(index))
                {
                    entry.user_typed_url = current_entry.user_typed_url.clone();
                }
                self.assign_new_document_sequence_number(&mut entry);
                self.replace_current_entry(entry);
            }
            Some(BrowserNavigationHistoryUpdate::ReplaceInitialEmptyDocument) => {
                entry.transition_type = "auto_toplevel".to_owned();
                self.assign_new_document_sequence_number(&mut entry);
                self.replace_current_entry(entry);
            }
            Some(BrowserNavigationHistoryUpdate::TraverseToEntry(entry_id)) => {
                self.traverse_to_entry(entry_id, entry);
            }
            None => {
                self.assign_new_document_sequence_number(&mut entry);
                self.push_entry(entry);
            }
        }
    }

    /// Applies a title observed after the current Document committed.
    ///
    /// Navigation commit can precede parser completion, so the Browser-owned
    /// history entry is initially allowed to carry an empty title. Renderer
    /// title output updates only the current entry; callers are responsible
    /// for validating the exact Page residence before reaching this method.
    pub fn update_current_entry_title(&mut self, title: String) -> Option<bool> {
        let current_entry = self
            .current_index
            .and_then(|current_index| self.entries.get_mut(current_index))?;
        if current_entry.title == title {
            return Some(false);
        }
        current_entry.title = title;
        Some(true)
    }

    pub fn record_same_document_update(
        &mut self,
        url: String,
        title: String,
        history_update: SameDocumentHistoryUpdate,
    ) -> Result<(), BrowserSameDocumentHistoryUpdateError> {
        match history_update {
            SameDocumentHistoryUpdate::Push | SameDocumentHistoryUpdate::Replace => {
                let mut entry = BrowserNavigationHistoryEntry {
                    id: self.allocate_entry_id(),
                    url,
                    user_typed_url: self
                        .current_index
                        .and_then(|index| self.entries.get(index))
                        .map(|entry| entry.user_typed_url.clone())
                        .unwrap_or_default(),
                    title,
                    transition_type: "link".to_owned(),
                    document_sequence_number: None,
                };
                self.assign_current_document_sequence_number(&mut entry);
                match history_update {
                    SameDocumentHistoryUpdate::Push => self.push_entry(entry),
                    SameDocumentHistoryUpdate::Replace => self.replace_current_entry(entry),
                    SameDocumentHistoryUpdate::Traverse { .. } => unreachable!(),
                }
                Ok(())
            }
            SameDocumentHistoryUpdate::Traverse { delta } => {
                let Some(current_index) = self.current_index else {
                    return Err(BrowserSameDocumentHistoryUpdateError::CurrentEntryUnavailable);
                };
                let Ok(signed_current_index) = i64::try_from(current_index) else {
                    return Err(
                        BrowserSameDocumentHistoryUpdateError::TraversalIndexOverflow {
                            current_index,
                            delta,
                        },
                    );
                };
                let Some(signed_target_index) = signed_current_index.checked_add(delta) else {
                    return Err(
                        BrowserSameDocumentHistoryUpdateError::TraversalIndexOverflow {
                            current_index,
                            delta,
                        },
                    );
                };
                let Ok(target_index) = usize::try_from(signed_target_index) else {
                    return Err(
                        BrowserSameDocumentHistoryUpdateError::TraversalTargetOutsideHistory {
                            current_index,
                            delta,
                            entry_count: self.entries.len(),
                        },
                    );
                };
                let Some(target_entry) = self.entries.get(target_index) else {
                    return Err(
                        BrowserSameDocumentHistoryUpdateError::TraversalTargetOutsideHistory {
                            current_index,
                            delta,
                            entry_count: self.entries.len(),
                        },
                    );
                };
                if target_entry.url != url {
                    return Err(
                        BrowserSameDocumentHistoryUpdateError::TraversalUrlMismatch {
                            target_index,
                            browser_url: target_entry.url.clone(),
                            renderer_url: url,
                        },
                    );
                }
                self.current_index = Some(target_index);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_document_title_updates_the_current_loaded_entry() {
        let mut history = BrowserNavigationHistory::default();
        assert_eq!(
            history.update_current_entry_title("before commit".to_owned()),
            None
        );

        let entry_id = history.allocate_entry_id();
        history.record_loaded_entry(BrowserNavigationHistoryEntry {
            id: entry_id,
            url: "https://example.test/".to_owned(),
            user_typed_url: "https://example.test/".to_owned(),
            title: String::new(),
            transition_type: "typed".to_owned(),
            document_sequence_number: None,
        });

        assert_eq!(
            history.update_current_entry_title("Example".to_owned()),
            Some(true)
        );
        assert_eq!(
            history.update_current_entry_title("Example".to_owned()),
            Some(false)
        );
        let (current_index, entries) = history.snapshot();
        assert_eq!(entries[current_index].title, "Example");
    }

    #[test]
    fn traversal_resolution_classifies_document_sequence_without_moving_cursor() {
        let mut history = BrowserNavigationHistory::default();
        let first_id = history.allocate_entry_id();
        history.record_loaded_entry(BrowserNavigationHistoryEntry {
            id: first_id,
            url: "https://example.test/first".to_owned(),
            user_typed_url: "https://example.test/first".to_owned(),
            title: "First".to_owned(),
            transition_type: "typed".to_owned(),
            document_sequence_number: None,
        });
        history
            .record_same_document_update(
                "https://example.test/first#state".to_owned(),
                "First state".to_owned(),
                SameDocumentHistoryUpdate::Push,
            )
            .expect("same-Document entry should commit");
        let second_document_id = history.allocate_entry_id();
        history.record_loaded_entry(BrowserNavigationHistoryEntry {
            id: second_document_id,
            url: "https://example.test/second".to_owned(),
            user_typed_url: "https://example.test/second".to_owned(),
            title: "Second".to_owned(),
            transition_type: "typed".to_owned(),
            document_sequence_number: None,
        });
        history
            .record_same_document_update(
                "https://example.test/second#state".to_owned(),
                "Second state".to_owned(),
                SameDocumentHistoryUpdate::Push,
            )
            .expect("second same-Document entry should commit");
        let before = history.snapshot();
        let current_id = before.1[before.0].id;

        assert_eq!(
            history.resolve_traversal(BrowserHistoryTraversalDestination::Entry(
                second_document_id,
            )),
            Ok(BrowserHistoryTraversalResolution::Entry {
                entry_id: second_document_id,
                url: "https://example.test/second".to_owned(),
                same_document_delta: Some(-1),
            })
        );
        assert_eq!(
            history.resolve_traversal(BrowserHistoryTraversalDestination::Entry(first_id)),
            Ok(BrowserHistoryTraversalResolution::Entry {
                entry_id: first_id,
                url: "https://example.test/first".to_owned(),
                same_document_delta: None,
            })
        );
        assert_eq!(
            history.resolve_traversal(BrowserHistoryTraversalDestination::Entry(current_id)),
            Ok(BrowserHistoryTraversalResolution::Noop {
                entry_id: current_id,
                url: "https://example.test/second#state".to_owned(),
            })
        );
        assert_eq!(
            history.resolve_traversal(BrowserHistoryTraversalDestination::Delta(1)),
            Err(BrowserHistoryTraversalResolutionError::NoSuchHistoryEntry)
        );
        assert_eq!(
            history.snapshot(),
            before,
            "resolution must not move the cursor"
        );
    }

    #[test]
    fn same_document_traversal_moves_cursor_without_allocating_or_appending() {
        let mut history = BrowserNavigationHistory::default();
        history
            .record_same_document_update(
                "https://example.test/a".to_owned(),
                "A".to_owned(),
                SameDocumentHistoryUpdate::Push,
            )
            .expect("first push should commit");
        history
            .record_same_document_update(
                "https://example.test/b".to_owned(),
                "B".to_owned(),
                SameDocumentHistoryUpdate::Push,
            )
            .expect("second push should commit");

        history
            .record_same_document_update(
                "https://example.test/a".to_owned(),
                "ignored during traversal".to_owned(),
                SameDocumentHistoryUpdate::Traverse { delta: -1 },
            )
            .expect("in-range traversal should commit");
        let (current_index, entries) = history.snapshot();
        assert_eq!(current_index, 0);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![1, 2]
        );

        history
            .record_same_document_update(
                "https://example.test/c".to_owned(),
                "C".to_owned(),
                SameDocumentHistoryUpdate::Push,
            )
            .expect("push after traversal should commit");
        let (current_index, entries) = history.snapshot();
        assert_eq!(current_index, 1);
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.id, entry.url.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, "https://example.test/a"), (3, "https://example.test/c")],
            "a traversal must neither allocate an id nor survive as a forward entry after push"
        );
    }

    #[test]
    fn same_document_replace_preserves_entry_id_and_out_of_range_traverse_is_atomic() {
        let mut history = BrowserNavigationHistory::default();
        history
            .record_same_document_update(
                "https://example.test/a".to_owned(),
                "A".to_owned(),
                SameDocumentHistoryUpdate::Push,
            )
            .expect("initial push should commit");
        history
            .record_same_document_update(
                "https://example.test/replaced".to_owned(),
                "Replaced".to_owned(),
                SameDocumentHistoryUpdate::Replace,
            )
            .expect("replace should commit");
        let before = history.snapshot();
        assert_eq!(before.0, 0);
        assert_eq!(before.1[0].id, 1);
        assert_eq!(before.1[0].url, "https://example.test/replaced");

        assert!(matches!(
            history.record_same_document_update(
                "https://example.test/missing".to_owned(),
                String::new(),
                SameDocumentHistoryUpdate::Traverse { delta: -1 },
            ),
            Err(
                BrowserSameDocumentHistoryUpdateError::TraversalTargetOutsideHistory {
                    current_index: 0,
                    delta: -1,
                    entry_count: 1,
                }
            )
        ));
        assert_eq!(history.snapshot(), before);
    }

    #[test]
    fn same_document_traversal_url_divergence_is_typed_and_atomic() {
        let mut history = BrowserNavigationHistory::default();
        history
            .record_same_document_update(
                "https://example.test/a".to_owned(),
                "A".to_owned(),
                SameDocumentHistoryUpdate::Push,
            )
            .expect("first push should commit");
        history
            .record_same_document_update(
                "https://example.test/b".to_owned(),
                "B".to_owned(),
                SameDocumentHistoryUpdate::Push,
            )
            .expect("second push should commit");
        let before = history.snapshot();

        assert_eq!(
            history.record_same_document_update(
                "https://renderer.example/wrong".to_owned(),
                String::new(),
                SameDocumentHistoryUpdate::Traverse { delta: -1 },
            ),
            Err(
                BrowserSameDocumentHistoryUpdateError::TraversalUrlMismatch {
                    target_index: 0,
                    browser_url: "https://example.test/a".to_owned(),
                    renderer_url: "https://renderer.example/wrong".to_owned(),
                }
            )
        );
        assert_eq!(history.snapshot(), before);
    }
}
