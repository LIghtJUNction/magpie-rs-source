//! Native GPUI single-line input for gateway addresses and model filtering.
//!
//! Implements the pinned GPUI `EntityInputHandler` contract. Printable text and
//! IME composition come from the platform text service, never from key strings.
//! The custom element supplies shaped text, selection, clipping and IME geometry.

use std::{cell::Cell, ops::Range, rc::Rc};

use gpui::{
    AccessibleAction, App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, Element,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, IntoElement, KeyDownEvent, KeyUpEvent, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render, Role,
    ShapedLine, SharedString, Style, TextInputAction, TextInputConfiguration, TextRun,
    UTF16Selection, UnderlineStyle, Window, div, fill, point, prelude::*, px, relative, rgb, size,
};

use crate::{
    input_model::{EditBuffer, EditError, ImeKeyGuard, Movement},
    theme::Palette,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputEvent {
    Changed,
    Submit,
    Escape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InputColors {
    surface: u32,
    inset: u32,
    border: u32,
    text: u32,
    muted: u32,
    accent: u32,
    selected: u32,
}

impl From<Palette> for InputColors {
    fn from(palette: Palette) -> Self {
        Self {
            surface: palette.surface,
            inset: palette.inset,
            border: palette.border,
            text: palette.text,
            muted: palette.muted,
            accent: palette.accent,
            selected: palette.selected,
        }
    }
}

pub(crate) struct TextInput {
    id: &'static str,
    focus_handle: FocusHandle,
    edit: EditBuffer,
    placeholder: SharedString,
    label: SharedString,
    colors: InputColors,
    disabled: bool,
    secret: bool,
    dragging: bool,
    last_line: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    scroll_x: Pixels,
    native_state_dirty: bool,
    ime_keys: ImeKeyGuard,
    a11y_run_id: Rc<Cell<Option<accesskit::NodeId>>>,
}

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    pub(crate) fn new(
        id: &'static str,
        value: &str,
        placeholder: &str,
        label: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            id,
            focus_handle: cx.focus_handle().tab_stop(true),
            // Callers only supply the small validated initial gateway address.
            // Subsequent programmatic edits expose refusal via set_text's bool.
            edit: EditBuffer::new(value).unwrap_or_default(),
            placeholder: placeholder.to_owned().into(),
            label: label.to_owned().into(),
            colors: InputColors {
                surface: 0xffffff,
                inset: 0xf0f0f2,
                border: 0xe3e3e7,
                text: 0x202124,
                muted: 0x65666d,
                accent: 0x3e61d4,
                selected: 0xeef2ff,
            },
            disabled: false,
            secret: false,
            dragging: false,
            last_line: None,
            last_bounds: None,
            scroll_x: px(0.),
            native_state_dirty: false,
            ime_keys: ImeKeyGuard::default(),
            a11y_run_id: Rc::new(Cell::new(None)),
        }
    }

    /// Password mode keeps the real value out of paint, accessibility and
    /// clipboard output. Only the owning form reads `text()` when saving.
    pub(crate) fn set_limit(&mut self, limit: usize) {
        if let Ok(edit) = EditBuffer::with_limit(self.edit.text(), limit) {
            self.edit = edit;
        }
    }

    pub(crate) fn set_secret(&mut self, secret: bool) {
        self.secret = secret;
        self.last_line = None;
        self.native_state_dirty = true;
    }

    fn display_text(&self) -> String {
        if self.secret {
            mask_text(self.edit.text())
        } else {
            self.edit.text().to_owned()
        }
    }

    fn display_index(&self, byte: usize) -> usize {
        if self.secret {
            self.edit.byte_to_scalar(byte) * '•'.len_utf8()
        } else {
            byte
        }
    }

    pub(crate) fn text(&self) -> &str {
        self.edit.text()
    }

    pub(crate) fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// Returns false only when the complete proposed value is refused. No URL is
    /// silently truncated. A changed value emits the same event as a user edit.
    pub(crate) fn set_text(&mut self, value: &str, cx: &mut Context<Self>) -> bool {
        match self.edit.set_text(value) {
            Ok(changed) => {
                self.native_state_dirty = true;
                if changed {
                    self.ime_keys.release();
                    cx.emit(InputEvent::Changed);
                }
                cx.notify();
                true
            }
            Err(_) => false,
        }
    }

    /// Update locale/theme/busy presentation without reconstructing the Entity,
    /// losing focus, or discarding the user's selection and pending input.
    pub(crate) fn set_presentation(
        &mut self,
        placeholder: &str,
        label: &str,
        palette: Palette,
        disabled: bool,
        cx: &mut Context<Self>,
    ) {
        let colors = InputColors::from(palette);
        if self.placeholder.as_ref() == placeholder
            && self.label.as_ref() == label
            && self.colors == colors
            && self.disabled == disabled
        {
            return;
        }
        self.placeholder = placeholder.to_owned().into();
        self.label = label.to_owned().into();
        self.colors = colors;
        if disabled && !self.disabled {
            self.edit.commit_composition();
            self.dragging = false;
            self.native_state_dirty = true;
        }
        self.disabled = disabled;
        self.focus_handle = self.focus_handle.clone().tab_stop(!disabled);
        cx.notify();
    }

    fn changed(&mut self, text_changed: bool, window: &mut Window, cx: &mut Context<Self>) {
        if text_changed {
            window.notify_text_input_changed(&self.focus_handle);
            cx.emit(InputEvent::Changed);
        }
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn finish_edit(
        &mut self,
        result: Result<bool, EditError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(changed) => self.changed(changed, window, cx),
            Err(_) => window.play_system_bell(),
        }
    }

    fn copy(&self, cx: &mut Context<Self>) {
        if !self.secret && !self.edit.selection().is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.edit.selected_text().to_owned(),
            ));
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        // Native composition may end immediately before this same terminating
        // key is dispatched. GPUI can draw first, so paint must not clear this
        // barrier. A release/ordinary key/pointer gesture does clear it.
        if self.ime_keys.key_down(matches!(key, "enter" | "escape")) {
            cx.stop_propagation();
            return;
        }
        // The root owns traversal plus scroll-to-focused. Do not move twice or
        // insert Tab as text. GPUI forwards printable text through the IME.
        if key == "tab" || (event.prefer_character_input && !matches!(key, "enter" | "escape")) {
            return;
        }
        let command = (modifiers.control || modifiers.platform) && !modifiers.alt;
        if command && key == "c" {
            self.copy(cx);
            cx.stop_propagation();
            return;
        }
        if self.disabled {
            if matches!(key, "enter" | "escape") {
                // A field can retain focus while its transaction is in flight.
                // Submit stays blocked; Escape remains an explicit request for
                // the parent to cancel/leave according to its transaction state.
                if key == "escape" && !event.is_held {
                    cx.emit(InputEvent::Escape);
                }
                cx.stop_propagation();
            }
            return;
        }
        if command {
            match key {
                "a" => self.edit.select_all(),
                "v" => {
                    if let Some(item) = cx.read_from_clipboard() {
                        EntityInputHandler::paste(self, item, window, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                "x" => {
                    self.edit.commit_composition();
                    self.copy(cx);
                    if !self.edit.selection().is_empty() {
                        let result = self.edit.replace(None, "");
                        self.finish_edit(result, window, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                "left" | "right" => {
                    let movement = if modifiers.platform && cx.platform_info().is_mac() {
                        if key == "left" {
                            Movement::Start
                        } else {
                            Movement::End
                        }
                    } else if key == "left" {
                        Movement::WordLeft
                    } else {
                        Movement::WordRight
                    };
                    self.edit.move_cursor(movement, modifiers.shift);
                }
                "home" => self.edit.move_cursor(Movement::Start, modifiers.shift),
                "end" => self.edit.move_cursor(Movement::End, modifiers.shift),
                // Preserve parent shortcuts such as Ctrl/Cmd+R.
                _ => return,
            }
        } else {
            match key {
                "left" | "right" if modifiers.alt => {
                    self.edit.move_cursor(
                        if key == "left" {
                            Movement::WordLeft
                        } else {
                            Movement::WordRight
                        },
                        modifiers.shift,
                    );
                }
                "left" => self.edit.move_cursor(Movement::Left, modifiers.shift),
                "right" => self.edit.move_cursor(Movement::Right, modifiers.shift),
                "home" => self.edit.move_cursor(Movement::Start, modifiers.shift),
                "end" => self.edit.move_cursor(Movement::End, modifiers.shift),
                "backspace" | "delete" => {
                    let changed = self.edit.delete(key == "backspace");
                    self.changed(changed, window, cx);
                    cx.stop_propagation();
                    return;
                }
                "enter" => {
                    if !self.edit.composing() && !event.is_held {
                        cx.emit(InputEvent::Submit);
                    }
                    cx.stop_propagation();
                    return;
                }
                "escape" => {
                    if self.edit.composing() {
                        let changed = self.edit.cancel_composition();
                        self.changed(changed, window, cx);
                    } else if !self.edit.selection().is_empty() {
                        self.edit.collapse_selection();
                        self.changed(false, window, cx);
                    } else if !event.is_held {
                        cx.emit(InputEvent::Escape);
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => return,
            }
        }
        self.changed(false, window, cx);
        cx.stop_propagation();
    }

    fn key_up(&mut self, _: &KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.ime_keys.release();
    }

    fn index_for_point(&self, position: Point<Pixels>) -> usize {
        let (Some(line), Some(bounds)) = (&self.last_line, self.last_bounds) else {
            return self.edit.cursor();
        };
        if self.edit.text().is_empty() {
            return 0;
        }
        if line.text.as_ref() != self.display_text() {
            return self.edit.cursor();
        }
        let index = line.closest_index_for_x(position.x - bounds.left() + self.scroll_x);
        let byte = if self.secret {
            self.edit.scalar_to_byte(index / '•'.len_utf8())
        } else {
            index
        };
        self.edit.nearest_grapheme(byte)
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        self.ime_keys.release();
        self.focus_handle.focus(window, cx);
        window.request_virtual_keyboard();
        let index = self.index_for_point(event.position);
        if event.modifiers.shift {
            self.edit.set_selection(self.edit.anchor(), index);
        } else if event.click_count >= 3 {
            self.edit.select_all();
        } else if event.click_count == 2 {
            self.edit.select_word(index);
        } else {
            self.edit.set_selection(index, index);
        }
        self.dragging = true;
        self.changed(false, window, cx);
        cx.stop_propagation();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.dragging && !event.dragging() {
            self.dragging = false;
        }
        if self.disabled || !self.dragging {
            return;
        }
        let index = self.index_for_point(event.position);
        self.edit.set_selection(self.edit.anchor(), index);
        self.changed(false, window, cx);
        cx.stop_propagation();
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.dragging = false;
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let bytes = self.edit.bytes_for_utf16(range);
        *adjusted_range = Some(self.edit.utf16_range(bytes.clone()));
        Some(if self.secret {
            // Match native UTF-16 lengths without exposing password characters.
            "*".repeat(self.edit.text()[bytes].encode_utf16().count())
        } else {
            self.edit.text()[bytes].to_owned()
        })
    }

    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if self.disabled && !ignore_disabled_input {
            return None;
        }
        Some(UTF16Selection {
            range: self.edit.utf16_range(self.edit.selection()),
            reversed: self.edit.reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.edit.marked().map(|bytes| self.edit.utf16_range(bytes))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.composing() {
            self.ime_keys.arm();
        }
        self.edit.commit_composition();
        self.changed(false, window, cx);
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.disabled {
            let was_composing = self.edit.composing();
            let result = self.edit.replace(range, text);
            if result.is_ok() && was_composing && !self.edit.composing() {
                self.ime_keys.arm();
            }
            self.finish_edit(result, window, cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.disabled {
            let was_composing = self.edit.composing();
            let result = self
                .edit
                .replace_marked(range, new_text, new_selected_range);
            if result.is_ok() && was_composing && !self.edit.composing() {
                self.ime_keys.arm();
            }
            self.finish_edit(result, window, cx);
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_line.as_ref()?;
        if !self.edit.text().is_empty() && line.text.as_ref() != self.display_text() {
            return None;
        }
        let range = self.edit.bytes_for_utf16(range_utf16);
        let origin_x = bounds.left() - self.scroll_x;
        let first = origin_x + line.x_for_index(self.display_index(range.start));
        let last = origin_x + line.x_for_index(self.display_index(range.end));
        let start = first.min(last).max(bounds.left()).min(bounds.right());
        let end = first.max(last).max(start).min(bounds.right());
        Some(Bounds::new(
            point(start, bounds.top()),
            size((end - start).max(px(1.)), bounds.size.height),
        ))
    }

    fn selection_bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        self.bounds_for_range(range_utf16, bounds, window, cx)
            .into_iter()
            .collect()
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        if !self.last_bounds?.contains(&position) {
            return None;
        }
        Some(self.edit.byte_to_utf16(self.index_for_point(position)))
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.disabled {
            self.edit.select_utf16(range_utf16);
            self.changed(false, window, cx);
        }
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.edit.byte_to_utf16(self.edit.text().len()))
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        !self.disabled
    }

    fn text_input_configuration(
        &mut self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> TextInputConfiguration {
        TextInputConfiguration {
            input_action: TextInputAction::Done,
            ..Default::default()
        }
    }

    fn text_input_editable_range(
        &mut self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        Some(0..self.edit.byte_to_utf16(self.edit.text().len()))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct TextPrepaint {
    line: Option<ShapedLine>,
    origin: Point<Pixels>,
    scroll_x: Pixels,
    selection: Option<PaintQuad>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = TextPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let style = window.text_style();
        let is_placeholder = input.edit.text().is_empty();
        let text: SharedString = if is_placeholder {
            input.placeholder.clone()
        } else {
            input.display_text().into()
        };
        let color = if is_placeholder || input.disabled {
            input.colors.muted
        } else {
            input.colors.text
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color: rgb(color).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked) = input.edit.marked() {
            let marked = input.display_index(marked.start)..input.display_index(marked.end);
            vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.len(),
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: text.len() - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect::<Vec<_>>()
        } else {
            vec![run]
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(text, font_size, &runs, None);
        let caret_x = if is_placeholder {
            px(0.)
        } else {
            line.x_for_index(input.display_index(input.edit.cursor()))
        };
        let viewport = (bounds.size.width - px(2.)).max(px(0.));
        let mut scroll_x = if is_placeholder {
            px(0.)
        } else {
            input.scroll_x
        };
        if caret_x < scroll_x {
            scroll_x = caret_x;
        } else if caret_x > scroll_x + viewport {
            scroll_x = caret_x - viewport;
        }
        scroll_x = scroll_x
            .max(px(0.))
            .min((line.width() - viewport).max(px(0.)));
        let origin = point(bounds.left() - scroll_x, bounds.top());
        let range = input.edit.selection();
        let selection = (!range.is_empty()).then(|| {
            let first = origin.x + line.x_for_index(input.display_index(range.start));
            let last = origin.x + line.x_for_index(input.display_index(range.end));
            fill(
                Bounds::from_corners(
                    point(first.min(last), bounds.top()),
                    point(first.max(last), bounds.bottom()),
                ),
                rgb(input.colors.selected),
            )
        });
        let cursor = (!input.disabled && input.focus_handle.is_focused(window)).then(|| {
            fill(
                Bounds::new(
                    point(origin.x + caret_x, bounds.top()),
                    size(px(1.5), bounds.size.height),
                ),
                rgb(input.colors.accent),
            )
        });
        TextPrepaint {
            line: Some(line),
            origin,
            scroll_x,
            selection,
            cursor,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let input = self.input.read(cx);
        let focus = input.focus_handle.clone();
        let disabled = input.disabled;
        let native_state_dirty = input.native_state_dirty;
        // Div's on_mouse_move is hover-only in the pinned GPUI. A native
        // capture listener keeps an active text selection dragging outside the
        // input's clipped viewport, without allocating an auto-scroll timer.
        let dragging_input = self.input.downgrade();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase.capture() {
                let _ = dragging_input.update(cx, |input, cx| input.mouse_move(event, window, cx));
            }
        });
        if !disabled {
            window.handle_input(
                &focus,
                ElementInputHandler::new(bounds, self.input.clone()),
                cx,
            );
        }
        if native_state_dirty {
            window.notify_text_input_changed(&focus);
            window.notify_text_selection_changed(&focus);
        }
        let Some(line) = prepaint.line.take() else {
            return;
        };
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection);
            }
            // Shaping/painting failures must not terminate the process while a
            // configuration transaction is running in the background.
            let _ = line.paint(
                prepaint.origin,
                window.line_height(),
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            );
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        });
        self.input.update(cx, |input, _| {
            input.last_line = Some(line);
            input.last_bounds = Some(bounds);
            input.scroll_x = prepaint.scroll_x;
            input.native_state_dirty = false;
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus_handle.is_focused(window) {
            self.ime_keys.release();
        }
        let colors = self.colors;
        let disabled = self.disabled;
        let text = self.display_text();
        let anchor = self.edit.byte_to_scalar(self.edit.anchor());
        let cursor = self.edit.byte_to_scalar(self.edit.cursor());
        let run_id = self.a11y_run_id.clone();
        let set_value = cx.weak_entity();
        let set_selection = cx.weak_entity();
        div()
            .id(self.id)
            .accessibility_id(self.id)
            .track_focus(&self.focus_handle)
            .tab_index(0)
            .tab_stop(!disabled)
            .role(if self.secret {
                Role::PasswordInput
            } else {
                Role::TextInput
            })
            .aria_label(self.label.clone())
            .aria_value(text.clone())
            .aria_placeholder(self.placeholder.clone())
            .a11y_synthetic_children(move |builder| {
                let id = builder.synthetic_node_id(0);
                run_id.set(Some(id));
                let mut run = accesskit::Node::new(Role::TextRun);
                run.set_character_lengths(
                    text.chars()
                        .map(|character| character.len_utf8() as u8)
                        .collect::<Vec<_>>(),
                );
                run.set_value(text);
                if let Some(bounds) = builder.parent_node().bounds() {
                    run.set_bounds(bounds);
                }
                builder.push_child(id, run);
                let parent = builder.parent_node();
                if disabled {
                    parent.set_disabled();
                    parent.set_read_only();
                }
                parent.set_text_selection(accesskit::TextSelection {
                    anchor: accesskit::TextPosition {
                        node: id,
                        character_index: anchor,
                    },
                    focus: accesskit::TextPosition {
                        node: id,
                        character_index: cursor,
                    },
                });
            })
            .when(!disabled, move |field| {
                field
                    .on_a11y_action(AccessibleAction::SetValue, move |data, window, cx| {
                        if let Some(accesskit::ActionData::Value(value)) = data {
                            let _ = set_value.update(cx, |input, cx| {
                                if !input.disabled {
                                    let result = input.edit.set_text(value);
                                    input.finish_edit(result, window, cx);
                                }
                            });
                        }
                    })
                    .on_a11y_action(
                        AccessibleAction::SetTextSelection,
                        move |data, window, cx| {
                            if let Some(accesskit::ActionData::SetTextSelection(selection)) = data {
                                let _ = set_selection.update(cx, |input, cx| {
                                    let id = input.a11y_run_id.get();
                                    if !input.disabled
                                        && Some(selection.anchor.node) == id
                                        && Some(selection.focus.node) == id
                                    {
                                        input.focus_handle.focus(window, cx);
                                        input.edit.set_selection(
                                            input
                                                .edit
                                                .scalar_to_byte(selection.anchor.character_index),
                                            input
                                                .edit
                                                .scalar_to_byte(selection.focus.character_index),
                                        );
                                        input.changed(false, window, cx);
                                    }
                                });
                            }
                        },
                    )
            })
            .w_full()
            .min_w_0()
            .px(px(10.))
            .py(px(7.))
            .border_1()
            .border_color(rgb(colors.border))
            .rounded(px(7.))
            .bg(rgb(if disabled {
                colors.inset
            } else {
                colors.surface
            }))
            .text_size(px(13.))
            .line_height(px(20.))
            .cursor(if disabled {
                CursorStyle::Arrow
            } else {
                CursorStyle::IBeam
            })
            .focus_visible(move |style| style.border_color(rgb(colors.accent)))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(TextElement { input: cx.entity() })
    }
}

fn mask_text(value: &str) -> String {
    "•".repeat(value.chars().count())
}

#[cfg(test)]
mod secret_tests {
    use super::mask_text;

    #[test]
    fn secret_presentation_contains_no_original_characters() {
        assert_eq!(mask_text("sk-test-密钥😀"), "•".repeat(11));
        assert_eq!(mask_text(""), "");
    }

    #[cfg(feature = "visual-test")]
    #[gpui::test]
    fn copying_a_secret_keeps_the_clipboard_unchanged(cx: &mut gpui::TestAppContext) {
        use gpui::{AppContext, ClipboardItem};
        let input = cx.new(|cx| {
            let mut input =
                super::TextInput::new("test-secret", "synthetic-secret", "", "API key", cx);
            input.set_secret(true);
            input.edit.select_all();
            input
        });
        cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("clipboard-before".into())));
        input.update(cx, |input, cx| {
            input.copy(cx);
            assert_eq!(input.display_text(), "•".repeat(16));
        });
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("clipboard-before".into())
        );
    }
}
