//! Bounded, single-line editing state independent of GPUI and platform APIs.
//!
//! Stored positions are UTF-8 byte offsets. Native IME ranges use UTF-16, while
//! AccessKit text positions count Unicode scalars. Conversions live here so none
//! of those coordinate systems can accidentally be used as a string index.

use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const MAX_INPUT_BYTES: usize = 4096;

/// Native IME callbacks can finish composition before GPUI dispatches the same
/// terminating key, and GPUI may redraw between those callbacks. Rendering must
/// never clear this guard. A key event, key release or explicit pointer gesture
/// ends it instead.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ImeKeyGuard {
    pending: bool,
}

impl ImeKeyGuard {
    pub(crate) fn arm(&mut self) {
        self.pending = true;
    }

    pub(crate) fn key_down(&mut self, is_submit_or_escape: bool) -> bool {
        std::mem::take(&mut self.pending) && is_submit_or_escape
    }

    pub(crate) fn release(&mut self) {
        self.pending = false;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditError {
    TooLong,
    ControlCharacter,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Movement {
    Left,
    Right,
    Start,
    End,
    WordLeft,
    WordRight,
}

#[derive(Clone, Debug)]
struct CompositionBackup {
    text: String,
    anchor: usize,
    cursor: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EditBuffer {
    text: String,
    anchor: usize,
    cursor: usize,
    marked: Option<Range<usize>>,
    composition: Option<CompositionBackup>,
}

impl EditBuffer {
    pub(crate) fn new(text: &str) -> Result<Self, EditError> {
        let text = single_line(text)?;
        let end = text.len();
        Ok(Self {
            text,
            anchor: end,
            cursor: end,
            marked: None,
            composition: None,
        })
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn anchor(&self) -> usize {
        self.anchor
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn selection(&self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }

    pub(crate) fn reversed(&self) -> bool {
        self.cursor < self.anchor
    }

    pub(crate) fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub(crate) fn composing(&self) -> bool {
        self.marked.is_some()
    }

    pub(crate) fn selected_text(&self) -> &str {
        &self.text[self.selection()]
    }

    pub(crate) fn byte_to_utf16(&self, offset: usize) -> usize {
        byte_to_utf16(&self.text, offset)
    }

    pub(crate) fn utf16_range(&self, bytes: Range<usize>) -> Range<usize> {
        self.byte_to_utf16(bytes.start)..self.byte_to_utf16(bytes.end)
    }

    pub(crate) fn bytes_for_utf16(&self, range: Range<usize>) -> Range<usize> {
        bytes_for_utf16(&self.text, range)
    }

    pub(crate) fn byte_to_scalar(&self, offset: usize) -> usize {
        self.text[..floor_boundary(&self.text, offset)]
            .chars()
            .count()
    }

    pub(crate) fn scalar_to_byte(&self, offset: usize) -> usize {
        self.text
            .char_indices()
            .nth(offset)
            .map_or(self.text.len(), |(index, _)| index)
    }

    /// Programmatic replacement uses the same limits as keyboard/clipboard/IME.
    pub(crate) fn set_text(&mut self, value: &str) -> Result<bool, EditError> {
        let next = Self::new(value)?;
        let changed = self.text != next.text;
        if changed {
            *self = next;
        }
        Ok(changed)
    }

    pub(crate) fn set_selection(&mut self, anchor: usize, cursor: usize) {
        self.commit_composition();
        self.anchor = floor_boundary(&self.text, anchor);
        self.cursor = floor_boundary(&self.text, cursor);
    }

    pub(crate) fn select_utf16(&mut self, range: Range<usize>) {
        let reversed = range.start > range.end;
        let bytes = self.bytes_for_utf16(range);
        // Native text services may move the caret inside a still-marked
        // composition. Unlike a mouse/user navigation command, that is not a
        // request to commit the composition.
        if reversed {
            self.anchor = bytes.end;
            self.cursor = bytes.start;
        } else {
            self.anchor = bytes.start;
            self.cursor = bytes.end;
        }
    }

    pub(crate) fn select_all(&mut self) {
        self.set_selection(0, self.text.len());
    }

    pub(crate) fn collapse_selection(&mut self) {
        self.set_selection(self.cursor, self.cursor);
    }

    pub(crate) fn move_cursor(&mut self, movement: Movement, extend: bool) {
        let selection = self.selection();
        let next = match movement {
            Movement::Left if !extend && !selection.is_empty() => selection.start,
            Movement::Right if !extend && !selection.is_empty() => selection.end,
            Movement::Left => self.previous_grapheme(self.cursor),
            Movement::Right => self.next_grapheme(self.cursor),
            Movement::Start => 0,
            Movement::End => self.text.len(),
            Movement::WordLeft => self
                .text
                .unicode_word_indices()
                .map(|(start, _)| start)
                .rfind(|start| *start < self.cursor)
                .unwrap_or(0),
            Movement::WordRight => self
                .text
                .unicode_word_indices()
                .map(|(start, word)| start + word.len())
                .find(|end| *end > self.cursor)
                .unwrap_or(self.text.len()),
        };
        self.set_selection(if extend { self.anchor } else { next }, next);
    }

    pub(crate) fn previous_grapheme(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .take_while(|index| *index < offset)
            .last()
            .unwrap_or(0)
    }

    pub(crate) fn next_grapheme(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .find(|index| *index > offset)
            .unwrap_or(self.text.len())
    }

    pub(crate) fn nearest_grapheme(&self, offset: usize) -> usize {
        let offset = floor_boundary(&self.text, offset);
        if self
            .text
            .grapheme_indices(true)
            .any(|(boundary, _)| boundary == offset)
            || offset == self.text.len()
        {
            offset
        } else {
            self.previous_grapheme(offset)
        }
    }

    pub(crate) fn select_word(&mut self, offset: usize) {
        let offset = floor_boundary(&self.text, offset);
        if let Some((start, piece)) = self
            .text
            .split_word_bound_indices()
            .find(|(start, piece)| *start <= offset && offset < start + piece.len())
        {
            let end = start + piece.len();
            self.set_selection(start, end);
        } else {
            self.set_selection(offset, offset);
        }
    }

    pub(crate) fn delete(&mut self, backwards: bool) -> bool {
        let mut range = self.selection();
        if range.is_empty() {
            let boundary = self.nearest_grapheme(range.start);
            if boundary != range.start {
                range = boundary..self.next_grapheme(boundary);
            } else if backwards {
                range.start = self.previous_grapheme(range.start);
            } else {
                range.end = self.next_grapheme(range.end);
            }
        } else {
            range.start = self.nearest_grapheme(range.start);
            if self.nearest_grapheme(range.end) != range.end {
                range.end = self.next_grapheme(range.end);
            }
        }
        // Empty replacement cannot exceed the size limit or contain controls.
        self.replace_bytes(range, "", None).unwrap_or(false)
    }

    /// An explicit native range wins over the current composition/selection.
    pub(crate) fn replace(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
    ) -> Result<bool, EditError> {
        let range = range_utf16
            .map(|range| self.bytes_for_utf16(range))
            .or_else(|| self.marked())
            .unwrap_or_else(|| self.selection());
        self.replace_bytes(range, text, None)
    }

    /// `selected_utf16` is relative to the newly inserted composition, not the
    /// whole field. Keeping that distinction matters for CJK in the middle of a URL.
    pub(crate) fn replace_marked(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
    ) -> Result<bool, EditError> {
        let range = range_utf16
            .map(|range| self.bytes_for_utf16(range))
            .or_else(|| self.marked())
            .unwrap_or_else(|| self.selection());
        self.replace_bytes(range, text, Some(selected_utf16))
    }

    fn replace_bytes(
        &mut self,
        range: Range<usize>,
        text: &str,
        marked_selection: Option<Option<Range<usize>>>,
    ) -> Result<bool, EditError> {
        let insertion = single_line(text)?;
        if self.text.len() - range.len() + insertion.len() > MAX_INPUT_BYTES {
            return Err(EditError::TooLong);
        }
        // Validate before changing any state, including the IME cancellation backup.
        if marked_selection.is_some() && self.composition.is_none() {
            self.composition = Some(CompositionBackup {
                text: self.text.clone(),
                anchor: self.anchor,
                cursor: self.cursor,
            });
        }
        let changed = self.text[range.clone()] != insertion;
        let start = range.start;
        let end = start + insertion.len();
        self.text.replace_range(range, &insertion);
        self.anchor = end;
        self.cursor = end;
        if let Some(selected_utf16) = marked_selection {
            if insertion.is_empty() {
                self.commit_composition();
            } else {
                self.marked = Some(start..end);
                if let Some(selected) = selected_utf16 {
                    let reversed = selected.start > selected.end;
                    let selected = bytes_for_utf16(&insertion, selected);
                    self.anchor = start
                        + if reversed {
                            selected.end
                        } else {
                            selected.start
                        };
                    self.cursor = start
                        + if reversed {
                            selected.start
                        } else {
                            selected.end
                        };
                }
            }
        } else {
            self.commit_composition();
        }
        Ok(changed)
    }

    pub(crate) fn commit_composition(&mut self) {
        self.marked = None;
        self.composition = None;
    }

    /// Cancel the whole composition rather than deleting a partially committed
    /// surrogate, or leaking Escape into the parent navigation handler.
    pub(crate) fn cancel_composition(&mut self) -> bool {
        self.marked = None;
        if let Some(backup) = self.composition.take() {
            let changed = self.text != backup.text;
            self.text = backup.text;
            self.anchor = backup.anchor;
            self.cursor = backup.cursor;
            changed
        } else {
            false
        }
    }
}

fn single_line(value: &str) -> Result<String, EditError> {
    // Check before allocating or walking an arbitrarily large clipboard item.
    if value.len() > MAX_INPUT_BYTES {
        return Err(EditError::TooLong);
    }
    let mut output = String::with_capacity(value.len());
    let mut previous_cr = false;
    for character in value.chars() {
        match character {
            '\n' if previous_cr => {}
            '\r' | '\n' | '\t' | '\u{2028}' | '\u{2029}' => output.push(' '),
            character if character.is_control() => return Err(EditError::ControlCharacter),
            character => output.push(character),
        }
        previous_cr = character == '\r';
    }
    Ok(output)
}

fn floor_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn byte_to_utf16(text: &str, offset: usize) -> usize {
    text[..floor_boundary(text, offset)].encode_utf16().count()
}

fn byte_for_utf16(text: &str, offset: usize, round_up: bool) -> usize {
    let mut utf16 = 0;
    for (byte, character) in text.char_indices() {
        if utf16 == offset {
            return byte;
        }
        let end = utf16 + character.len_utf16();
        if end > offset {
            return if round_up {
                byte + character.len_utf8()
            } else {
                byte
            };
        }
        utf16 = end;
    }
    text.len()
}

fn bytes_for_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    let start = range.start.min(range.end);
    let end = range.start.max(range.end);
    let byte_start = byte_for_utf16(text, start, false);
    // A caret inside a surrogate pair is snapped left, never expanded into a
    // selection. A nonempty range covers whole Unicode scalars at both ends.
    let byte_end = if start == end {
        byte_start
    } else {
        byte_for_utf16(text, end, true)
    };
    byte_start..byte_end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ime_commit_guard_consumes_the_terminating_key_once() {
        let mut guard = ImeKeyGuard::default();
        guard.arm();
        // Rendering has no guard transition. A native commit cannot be turned
        // into a submit by GPUI's intervening draw before key dispatch.
        assert!(guard.key_down(true));
        assert!(!guard.key_down(true));
    }

    #[test]
    fn ime_key_release_or_next_ordinary_key_allows_a_deliberate_later_submit() {
        let mut guard = ImeKeyGuard::default();
        guard.arm();
        guard.release();
        assert!(!guard.key_down(true));
        guard.arm();
        assert!(!guard.key_down(false));
        assert!(!guard.key_down(true));
    }

    #[test]
    fn native_ranges_cover_cjk_and_surrogates_without_utf8_slicing() {
        let input = EditBuffer::new("a🙂漢字z").unwrap();
        assert_eq!(input.utf16_range(1..8), 1..4);
        assert_eq!(input.bytes_for_utf16(1..3), 1..5);
        assert_eq!(input.bytes_for_utf16(2..2), 1..1);
        assert_eq!(input.bytes_for_utf16(2..4), 1..8);
        assert_eq!(input.bytes_for_utf16(Range { start: 4, end: 2 }), 1..8);
        assert_eq!(input.bytes_for_utf16(usize::MAX..usize::MAX), 12..12);
        assert_eq!(input.bytes_for_utf16(0..usize::MAX), 0..12);
    }

    #[test]
    fn every_scalar_boundary_round_trips_in_all_coordinate_systems() {
        let input = EditBuffer::new("中🙂e\u{301}👩‍👧العربية").unwrap();
        for (scalar, byte) in input
            .text()
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(input.text().len()))
            .enumerate()
        {
            let utf16 = input.byte_to_utf16(byte);
            assert_eq!(input.bytes_for_utf16(utf16..utf16), byte..byte);
            assert_eq!(input.byte_to_scalar(byte), scalar);
            assert_eq!(input.scalar_to_byte(scalar), byte);
        }
    }

    #[test]
    fn shift_navigation_preserves_anchor_and_plain_arrows_collapse() {
        let mut input = EditBuffer::new("中🙂文").unwrap();
        input.move_cursor(Movement::Left, true);
        input.move_cursor(Movement::Left, true);
        assert_eq!(input.selected_text(), "🙂文");
        assert!(input.reversed());
        input.move_cursor(Movement::Right, false);
        assert_eq!(input.selection(), 10..10);
        input.move_cursor(Movement::Start, true);
        assert_eq!(input.selected_text(), "中🙂文");
        input.move_cursor(Movement::End, true);
        assert!(input.selection().is_empty());
    }

    #[test]
    fn deletion_respects_combining_and_zwj_graphemes() {
        let mut input = EditBuffer::new("e\u{301}👩‍👧中").unwrap();
        assert!(input.delete(true));
        assert_eq!(input.text(), "e\u{301}👩‍👧");
        assert!(input.delete(true));
        assert_eq!(input.text(), "e\u{301}");
        input.move_cursor(Movement::Start, false);
        assert!(input.delete(false));
        assert_eq!(input.text(), "");
        assert!(!input.delete(false));
        assert!(!input.delete(true));
    }

    #[test]
    fn paste_replaces_reverse_selection_and_normalizes_a_single_line() {
        let mut input = EditBuffer::new("before中🙂after").unwrap();
        input.set_selection(13, 6);
        input.replace(None, "模型\r\n二\t三\u{2028}四").unwrap();
        assert_eq!(input.text(), "before模型 二 三 四after");
        assert!(input.selection().is_empty());
    }

    #[test]
    fn composition_selection_is_relative_to_inserted_utf16_text() {
        let mut input = EditBuffer::new("ab尾").unwrap();
        input.set_selection(2, 2);
        input.replace_marked(None, "🙂中", Some(2..3)).unwrap();
        assert_eq!(input.text(), "ab🙂中尾");
        assert_eq!(input.marked(), Some(2..9));
        assert_eq!(input.selected_text(), "中");
        input.replace_marked(None, "中文", Some(2..2)).unwrap();
        assert_eq!(input.text(), "ab中文尾");
        assert_eq!(input.cursor(), 8);
        assert!(input.cancel_composition());
        assert_eq!(input.text(), "ab尾");
        assert_eq!(input.selection(), 2..2);
        assert!(!input.composing());
    }

    #[test]
    fn committing_composition_replaces_mark_then_discards_cancel_backup() {
        let mut input = EditBuffer::new("prefix旧tail").unwrap();
        input.select_utf16(6..7);
        input.replace_marked(None, "zhong", None).unwrap();
        input.replace(None, "中").unwrap();
        assert_eq!(input.text(), "prefix中tail");
        assert!(!input.composing());
        assert!(!input.cancel_composition());
        assert_eq!(input.text(), "prefix中tail");
    }

    #[test]
    fn native_caret_updates_preserve_the_composition_and_its_cancel_backup() {
        let mut input = EditBuffer::new("before尾").unwrap();
        input.set_selection(6, 6);
        input.replace_marked(None, "中文", None).unwrap();
        input.select_utf16(7..7);
        assert!(input.composing());
        assert_eq!(input.cursor(), 9);
        input.cancel_composition();
        assert_eq!(input.text(), "before尾");
        assert_eq!(input.selection(), 6..6);
    }

    #[test]
    fn explicit_native_range_overrides_selection_and_marked_range() {
        let mut input = EditBuffer::new("A🙂中Z").unwrap();
        input.select_all();
        input.replace(Some(3..4), "文").unwrap();
        assert_eq!(input.text(), "A🙂文Z");
        input.replace_marked(Some(1..3), "甲", Some(0..1)).unwrap();
        assert_eq!(input.text(), "A甲文Z");
        input.replace(Some(2..3), "乙").unwrap();
        assert_eq!(input.text(), "A甲乙Z");
    }

    #[test]
    fn failed_edits_preserve_text_selection_and_composition() {
        let mut input = EditBuffer::new("中").unwrap();
        input.replace_marked(None, "🙂", Some(0..2)).unwrap();
        let before = format!("{input:?}");
        assert_eq!(
            input.replace(None, &"a".repeat(MAX_INPUT_BYTES + 1)),
            Err(EditError::TooLong)
        );
        assert_eq!(format!("{input:?}"), before);
        assert_eq!(
            input.replace_marked(None, "bad\0", None),
            Err(EditError::ControlCharacter)
        );
        assert_eq!(format!("{input:?}"), before);
        input.cancel_composition();
        assert_eq!(input.text(), "中");
    }

    #[test]
    fn total_bound_applies_to_insertions_without_truncating_the_address() {
        let mut input = EditBuffer::new(&"a".repeat(MAX_INPUT_BYTES)).unwrap();
        assert_eq!(input.replace(None, "中"), Err(EditError::TooLong));
        assert_eq!(input.text().len(), MAX_INPUT_BYTES);
        input.select_all();
        assert_eq!(input.replace(None, "127.0.0.1:3425"), Ok(true));
    }

    #[test]
    fn repeated_programmatic_value_preserves_the_users_selection() {
        let mut input = EditBuffer::new("127.0.0.1:3425").unwrap();
        input.set_selection(3, 9);
        assert_eq!(input.set_text("127.0.0.1:3425"), Ok(false));
        assert_eq!(input.selection(), 3..9);
    }

    #[test]
    fn native_reverse_selection_and_accessibility_scalar_positions() {
        let mut input = EditBuffer::new("a🙂中").unwrap();
        input.select_utf16(Range { start: 4, end: 1 });
        assert!(input.reversed());
        assert_eq!(input.selected_text(), "🙂中");
        assert_eq!(input.byte_to_scalar(input.anchor()), 3);
        assert_eq!(input.byte_to_scalar(input.cursor()), 1);
        input.set_selection(input.scalar_to_byte(1), input.scalar_to_byte(2));
        assert_eq!(input.selected_text(), "🙂");
    }

    #[test]
    fn word_navigation_and_mouse_word_selection_are_unicode_aware() {
        let mut input = EditBuffer::new("模型 native-fixture").unwrap();
        input.move_cursor(Movement::WordLeft, false);
        assert_eq!(&input.text()[input.cursor()..], "fixture");
        input.move_cursor(Movement::WordLeft, true);
        assert_eq!(input.selected_text(), "native-");
        input.move_cursor(Movement::WordRight, false);
        assert_eq!(&input.text()[input.cursor()..], "-fixture");
        input.select_word(1);
        assert!(!input.selected_text().is_empty());
        assert!(input.text().is_char_boundary(input.anchor()));
        assert!(input.text().is_char_boundary(input.cursor()));
    }
}

#[cfg(test)]
mod interior_grapheme_tests {
    use super::EditBuffer;
    #[test]
    fn native_interior_caret_deletes_whole_grapheme_in_either_direction() {
        for grapheme in ["e\u{301}", "👩‍💻", "🇹🇼"] {
            for scalar in 1..grapheme.chars().count() {
                for backwards in [true, false] {
                    let mut input = EditBuffer::new(&format!("a{grapheme}b")).unwrap();
                    let byte = input.scalar_to_byte(1 + scalar);
                    input.set_selection(byte, byte);
                    assert!(input.delete(backwards));
                    assert_eq!(input.text(), "ab");
                    assert_eq!(input.cursor(), 1);
                }
            }
        }
    }
    #[test]
    fn partial_native_selection_is_expanded_for_keyboard_deletion() {
        for backwards in [true, false] {
            let mut input = EditBuffer::new("ae\u{301}b").unwrap();
            input.select_utf16(2..3);
            assert!(input.delete(backwards));
            assert_eq!(input.text(), "ab");
        }
    }
}
