//! Transactional editing with bounded undo and redo history.

use std::collections::VecDeque;

use crate::datafile::{DataFileBase, MemorySegmentList};
use crate::{Error, Result};

#[derive(Debug, Clone)]
struct Revision {
    label: String,
    image: MemorySegmentList,
}

/// The result of one transactional edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditOutcome<T> {
    /// Value returned by the editing closure.
    pub value: T,
    /// Whether the image differs from its state before the edit.
    pub changed: bool,
}

/// Information returned after an undo or redo operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEvent {
    /// User-facing label supplied when the edit was applied.
    pub label: String,
    pub undo_depth: usize,
    pub redo_depth: usize,
}

/// An owned sparse image with transactional edits and bounded history.
///
/// Each successful, image-changing call to [`EditSession::apply`] stores one
/// complete image snapshot. Group related low-level edits in a single closure
/// to make them one atomic undo step. Failed and no-op edits do not consume a
/// history slot or invalidate the redo branch.
#[derive(Debug, Clone)]
pub struct EditSession {
    image: MemorySegmentList,
    saved_image: MemorySegmentList,
    undo: VecDeque<Revision>,
    redo: Vec<Revision>,
    history_limit: usize,
}

impl EditSession {
    pub const DEFAULT_HISTORY_LIMIT: usize = 64;

    /// Creates a session after validating the initial sparse image.
    pub fn new(image: MemorySegmentList) -> Result<Self> {
        Self::with_history_limit(image, Self::DEFAULT_HISTORY_LIMIT)
    }

    /// Creates a session with a maximum number of undoable edits.
    pub fn with_history_limit(image: MemorySegmentList, history_limit: usize) -> Result<Self> {
        if history_limit == 0 {
            return Err(Error::Value(
                "edit history limit must be greater than zero".to_string(),
            ));
        }
        image.validate_image()?;
        Ok(Self {
            saved_image: image.clone(),
            image,
            undo: VecDeque::new(),
            redo: Vec::new(),
            history_limit,
        })
    }

    /// Starts a session from a file's current memory image.
    pub fn from_base(base: &DataFileBase) -> Result<Self> {
        Self::new(base.segment_list.clone())
    }

    pub fn image(&self) -> &MemorySegmentList {
        &self.image
    }

    pub fn history_limit(&self) -> usize {
        self.history_limit
    }

    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn next_undo_label(&self) -> Option<&str> {
        self.undo.back().map(|revision| revision.label.as_str())
    }

    pub fn next_redo_label(&self) -> Option<&str> {
        self.redo.last().map(|revision| revision.label.as_str())
    }

    /// Reports whether the image differs from the last saved checkpoint.
    pub fn is_dirty(&self) -> bool {
        self.image != self.saved_image
    }

    /// Makes the current image the saved checkpoint without clearing history.
    pub fn mark_saved(&mut self) {
        self.saved_image = self.image.clone();
    }

    /// Applies a group of edits to a private clone, validates the result, and
    /// commits the group only when the closure succeeds.
    pub fn apply<T, F>(&mut self, label: impl Into<String>, edit: F) -> Result<EditOutcome<T>>
    where
        F: FnOnce(&mut MemorySegmentList) -> Result<T>,
    {
        let label = label.into();
        if label.trim().is_empty() {
            return Err(Error::Value("edit label must not be empty".to_string()));
        }

        let mut work = self.image.clone();
        let value = edit(&mut work)?;
        work.validate_image()?;
        let changed = work != self.image;
        if changed {
            if self.undo.len() == self.history_limit {
                self.undo.pop_front();
            }
            self.undo.push_back(Revision {
                label,
                image: std::mem::replace(&mut self.image, work),
            });
            self.redo.clear();
        }
        Ok(EditOutcome { value, changed })
    }

    /// Restores the state before the newest edit.
    pub fn undo(&mut self) -> Option<HistoryEvent> {
        let revision = self.undo.pop_back()?;
        let label = revision.label;
        let current = std::mem::replace(&mut self.image, revision.image);
        self.redo.push(Revision {
            label: label.clone(),
            image: current,
        });
        Some(self.event(label))
    }

    /// Reapplies the newest undone edit.
    pub fn redo(&mut self) -> Option<HistoryEvent> {
        let revision = self.redo.pop()?;
        let label = revision.label;
        let current = std::mem::replace(&mut self.image, revision.image);
        if self.undo.len() == self.history_limit {
            self.undo.pop_front();
        }
        self.undo.push_back(Revision {
            label: label.clone(),
            image: current,
        });
        Some(self.event(label))
    }

    /// Drops undo and redo snapshots without changing the image or checkpoint.
    pub fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// Copies the edited image back to a file and marks it dirty on change.
    pub fn commit_to(&self, base: &mut DataFileBase) -> bool {
        let changed = base.segment_list != self.image;
        if changed {
            base.segment_list = self.image.clone();
            base.is_dirty = true;
        }
        changed
    }

    pub fn into_image(self) -> MemorySegmentList {
        self.image
    }

    fn event(&self, label: String) -> HistoryEvent {
        HistoryEvent {
            label,
            undo_depth: self.undo.len(),
            redo_depth: self.redo.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use autors_a2l::model::enums::MemoryPrgType;

    use super::*;
    use crate::datafile::MemorySegment;
    use crate::image::{AddressRange, OverlapPolicy};

    fn image() -> MemorySegmentList {
        MemorySegmentList {
            segments: vec![MemorySegment::from_data(
                0x1000,
                vec![1, 2, 3, 4],
                MemoryPrgType::DATA,
                true,
            )],
        }
    }

    fn bytes(session: &EditSession) -> Vec<u8> {
        session
            .image()
            .read_exact(AddressRange::new(0x1000, 0x1004).unwrap())
            .unwrap()
    }

    #[test]
    fn grouped_edit_is_one_undoable_transaction() {
        let mut session = EditSession::new(image()).unwrap();
        let outcome = session
            .apply("patch calibration", |image| {
                image.write_at(0x1000, &[9])?;
                image.write_at(0x1003, &[8])
            })
            .unwrap();
        assert!(outcome.changed);
        assert_eq!(outcome.value, 1);
        assert_eq!(bytes(&session), [9, 2, 3, 8]);
        assert_eq!(session.next_undo_label(), Some("patch calibration"));

        let event = session.undo().unwrap();
        assert_eq!(event.label, "patch calibration");
        assert_eq!(bytes(&session), [1, 2, 3, 4]);
        assert!(!session.is_dirty());

        session.redo().unwrap();
        assert_eq!(bytes(&session), [9, 2, 3, 8]);
        assert!(session.is_dirty());
    }

    #[test]
    fn failed_and_noop_edits_preserve_history_and_redo_branch() {
        let mut session = EditSession::new(image()).unwrap();
        session
            .apply("change", |image| image.write_at(0x1000, &[9]))
            .unwrap();
        session.undo().unwrap();

        let noop = session
            .apply("noop", |image| image.erase_range(AddressRange::new(0, 1)?))
            .unwrap();
        assert!(!noop.changed);
        assert!(session.can_redo());

        let before = session.image().clone();
        let failed = session.apply("collision", |image| {
            image.fill_range(
                AddressRange::new(0x1000, 0x1002)?,
                &[0],
                OverlapPolicy::Reject,
            )
        });
        assert!(failed.is_err());
        assert_eq!(session.image(), &before);
        assert!(session.can_redo());
    }

    #[test]
    fn new_edit_after_undo_discards_redo_and_history_is_bounded() {
        let mut session = EditSession::with_history_limit(image(), 2).unwrap();
        for (label, value) in [("one", 5), ("two", 6), ("three", 7)] {
            session
                .apply(label, |image| image.write_at(0x1000, &[value]))
                .unwrap();
        }
        assert_eq!(session.undo_depth(), 2);
        session.undo().unwrap();
        session.undo().unwrap();
        assert!(!session.can_undo());
        assert_eq!(bytes(&session), [5, 2, 3, 4]);

        session
            .apply("branch", |image| image.write_at(0x1001, &[8]))
            .unwrap();
        assert!(!session.can_redo());
        assert_eq!(bytes(&session), [5, 8, 3, 4]);
    }

    #[test]
    fn checkpoint_and_commit_track_dirty_state() {
        let original = image();
        let mut base = DataFileBase::new(None, original.clone());
        let mut session = EditSession::from_base(&base).unwrap();
        session
            .apply("change", |image| image.write_at(0x1002, &[0xAA]))
            .unwrap();
        assert!(session.is_dirty());
        assert!(session.commit_to(&mut base));
        assert!(base.is_dirty);
        assert_eq!(base.segment_list, *session.image());

        session.mark_saved();
        assert!(!session.is_dirty());
        session.undo().unwrap();
        assert!(session.is_dirty());
        assert_eq!(session.image(), &original);
    }
}
