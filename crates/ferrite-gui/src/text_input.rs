use std::ops::Range;

use gpui::{
    App, Bounds, Context, CursorStyle, ElementId, ElementInputHandler, Entity, EntityInputHandler,
    FocusHandle, Focusable, GlobalElementId, Hsla, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill,
    point, prelude::*, px, relative, rgba, size,
};
use unicode_segmentation::UnicodeSegmentation;

actions!(
    ferrite_input,
    [
        InputBackspace,
        InputDelete,
        InputLeft,
        InputRight,
        InputSelectLeft,
        InputSelectRight,
        InputSelectAll,
        InputHome,
        InputEnd,
        InputPaste,
        InputCut,
        InputCopy,
        InputUndo,
        InputRedo,
    ]
);

pub(crate) fn text_input_key_bindings() -> Vec<KeyBinding> {
    let bindings = vec![
        KeyBinding::new("backspace", InputBackspace, Some("FerriteInput")),
        KeyBinding::new("delete", InputDelete, Some("FerriteInput")),
        KeyBinding::new("left", InputLeft, Some("FerriteInput")),
        KeyBinding::new("right", InputRight, Some("FerriteInput")),
        KeyBinding::new("shift-left", InputSelectLeft, Some("FerriteInput")),
        KeyBinding::new("shift-right", InputSelectRight, Some("FerriteInput")),
        KeyBinding::new("secondary-a", InputSelectAll, Some("FerriteInput")),
        KeyBinding::new("secondary-v", InputPaste, Some("FerriteInput")),
        KeyBinding::new("secondary-c", InputCopy, Some("FerriteInput")),
        KeyBinding::new("secondary-x", InputCut, Some("FerriteInput")),
        KeyBinding::new("secondary-z", InputUndo, Some("FerriteInput")),
        KeyBinding::new("secondary-shift-z", InputRedo, Some("FerriteInput")),
        KeyBinding::new("home", InputHome, Some("FerriteInput")),
        KeyBinding::new("end", InputEnd, Some("FerriteInput")),
    ];

    #[cfg(not(target_os = "macos"))]
    let bindings = {
        let mut bindings = bindings;
        bindings.extend([
            KeyBinding::new("ctrl-y", InputRedo, Some("FerriteInput")),
            KeyBinding::new("ctrl-insert", InputCopy, Some("FerriteInput")),
            KeyBinding::new("shift-insert", InputPaste, Some("FerriteInput")),
            KeyBinding::new("shift-delete", InputCut, Some("FerriteInput")),
            KeyBinding::new("copy", InputCopy, Some("FerriteInput")),
            KeyBinding::new("paste", InputPaste, Some("FerriteInput")),
            KeyBinding::new("cut", InputCut, Some("FerriteInput")),
        ]);
        bindings
    };

    bindings
}

pub fn bind_text_input_keys(cx: &mut App) {
    cx.bind_keys(text_input_key_bindings());
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InputSnapshot {
    content: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

const MAX_UNDO_HISTORY: usize = 64;

pub struct TextInput {
    pub focus_handle: FocusHandle,
    pub content: SharedString,
    pub placeholder: SharedString,
    pub borderless: bool,
    pub masked: bool,
    pub classic_style: bool,
    pub font_scale: f32,
    pub ui_scale: f32,
    pub bg: Hsla,
    pub border: Hsla,
    pub border_focus: Hsla,
    pub text_color: Hsla,
    pub placeholder_color: Hsla,
    pub cursor_color: Hsla,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    undo_stack: Vec<InputSnapshot>,
    redo_stack: Vec<InputSnapshot>,
}

fn content_byte_to_visual_byte(content: &str, byte_offset: usize, masked: bool) -> usize {
    let clamped = byte_offset.min(content.len());
    if !masked {
        return clamped;
    }
    let safe_end = content
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(content.len()))
        .take_while(|&i| i <= clamped)
        .last()
        .unwrap_or(0);
    content[..safe_end].chars().count() * '•'.len_utf8()
}

fn visual_byte_to_content_byte(content: &str, visual_byte: usize, masked: bool) -> usize {
    if !masked {
        return visual_byte.min(content.len());
    }
    let char_idx = visual_byte / '•'.len_utf8();
    content
        .char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(content.len())
}

fn sanitize_single_line_paste(text: &str) -> String {
    text.trim_end_matches(['\r', '\n'])
        .replace('\r', "")
        .replace('\n', " ")
}

fn word_selection_range(content: &str, byte_offset: usize) -> Range<usize> {
    if content.is_empty() {
        return 0..0;
    }
    let mut idx = byte_offset.min(content.len());
    while idx > 0 && !content.is_char_boundary(idx) {
        idx -= 1;
    }
    if idx == content.len()
        && let Some((last_idx, _)) = content.char_indices().next_back()
    {
        idx = last_idx;
    }
    let target_ws = content[idx..]
        .chars()
        .next()
        .is_some_and(char::is_whitespace);
    let start = content[..idx]
        .char_indices()
        .rev()
        .take_while(|(_, ch)| ch.is_whitespace() == target_ws)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(idx);
    let end = content[idx..]
        .char_indices()
        .take_while(|(_, ch)| ch.is_whitespace() == target_ws)
        .last()
        .map(|(i, ch)| idx + i + ch.len_utf8())
        .unwrap_or(idx);
    start..end
}

impl TextInput {
    pub fn new(
        cx: &mut Context<Self>,
        placeholder: impl Into<SharedString>,
        initial: &str,
    ) -> Self {
        let len = initial.len();
        Self {
            focus_handle: cx.focus_handle(),
            content: initial.to_string().into(),
            placeholder: placeholder.into(),
            borderless: false,
            masked: false,
            classic_style: false,
            font_scale: 1.0,
            ui_scale: 1.0,
            bg: rgba(0x0f172aff).into(),
            border: rgba(0x334155ff).into(),
            border_focus: rgba(0x3b82f6ff).into(),
            text_color: rgba(0xf1f5f9ff).into(),
            placeholder_color: rgba(0x64748bff).into(),
            cursor_color: rgba(0x60a5faff).into(),
            selected_range: len..len,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn set_masked(&mut self, masked: bool, cx: &mut Context<Self>) {
        if self.masked != masked {
            self.masked = masked;
            cx.notify();
        }
    }

    pub fn apply_appearance(&mut self, font_scale: f32, ui_scale: f32, classic_style: bool) {
        self.font_scale = font_scale.clamp(0.75, 1.6);
        self.ui_scale = ui_scale.clamp(0.75, 1.5);
        self.classic_style = classic_style;
    }

    pub fn apply_colors(
        &mut self,
        bg: Hsla,
        border: Hsla,
        border_focus: Hsla,
        text_color: Hsla,
        placeholder_color: Hsla,
        cursor_color: Hsla,
    ) {
        self.bg = bg;
        self.border = border;
        self.border_focus = border_focus;
        self.text_color = text_color;
        self.placeholder_color = placeholder_color;
        self.cursor_color = cursor_color;
    }

    pub fn text(&self) -> String {
        self.content.to_string()
    }

    pub fn set_text(&mut self, value: &str, cx: &mut Context<Self>) {
        let len = value.len();
        self.content = value.to_string().into();
        self.selected_range = len..len;
        self.selection_reversed = false;
        self.marked_range = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        cx.notify();
    }

    fn push_undo_snapshot(&mut self) {
        let snap = InputSnapshot {
            content: self.content.to_string(),
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
        };
        if self.undo_stack.last() != Some(&snap) {
            if self.undo_stack.len() >= MAX_UNDO_HISTORY {
                self.undo_stack.remove(0);
            }
            self.undo_stack.push(snap);
        }
        self.redo_stack.clear();
    }

    fn undo(&mut self, _: &InputUndo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(prev) = self.undo_stack.pop() {
            if self.redo_stack.len() >= MAX_UNDO_HISTORY {
                self.redo_stack.remove(0);
            }
            self.redo_stack.push(InputSnapshot {
                content: self.content.to_string(),
                selected_range: self.selected_range.clone(),
                selection_reversed: self.selection_reversed,
            });
            self.content = prev.content.into();
            self.selected_range = prev.selected_range;
            self.selection_reversed = prev.selection_reversed;
            self.marked_range = None;
            cx.notify();
        }
    }

    fn redo(&mut self, _: &InputRedo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(next) = self.redo_stack.pop() {
            if self.undo_stack.len() >= MAX_UNDO_HISTORY {
                self.undo_stack.remove(0);
            }
            self.undo_stack.push(InputSnapshot {
                content: self.content.to_string(),
                selected_range: self.selected_range.clone(),
                selection_reversed: self.selection_reversed,
            });
            self.content = next.content.into();
            self.selected_range = next.selected_range;
            self.selection_reversed = next.selection_reversed;
            self.marked_range = None;
            cx.notify();
        }
    }

    fn left(&mut self, _: &InputLeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &InputRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn select_left(&mut self, _: &InputSelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &InputSelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &InputSelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &InputHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &InputEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &InputBackspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &InputDelete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn paste(&mut self, _: &InputPaste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = crate::helpers::read_clipboard_text(cx) {
            let single = sanitize_single_line_paste(&text);
            self.replace_text_in_range(None, &single, window, cx);
        }
    }

    fn copy(&mut self, _: &InputCopy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.masked && !self.selected_range.is_empty() {
            crate::helpers::write_clipboard_text(
                self.content[self.selected_range.clone()].to_string(),
                false,
                cx,
            );
        }
    }

    fn cut(&mut self, _: &InputCut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.masked && !self.selected_range.is_empty() {
            crate::helpers::write_clipboard_text(
                self.content[self.selected_range.clone()].to_string(),
                false,
                cx,
            );
            self.replace_text_in_range(None, "", window, cx);
        } else if self.masked && !self.selected_range.is_empty() {
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn sync_primary_selection(&self, cx: &mut Context<Self>) {
        #[cfg(target_os = "linux")]
        if !self.masked && !self.selected_range.is_empty() {
            cx.write_to_primary(gpui::ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
        #[cfg(not(target_os = "linux"))]
        let _ = cx;
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window);
        if event.click_count >= 3 {
            self.is_selecting = false;
            self.selection_reversed = false;
            self.selected_range = 0..self.content.len();
            self.sync_primary_selection(cx);
            cx.notify();
            return;
        }
        if event.click_count == 2 {
            self.is_selecting = false;
            self.selection_reversed = false;
            let idx = self.index_for_mouse_position(event.position);
            self.selected_range = word_selection_range(&self.content, idx);
            self.sync_primary_selection(cx);
            cx.notify();
            return;
        }
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    #[cfg(target_os = "linux")]
    fn on_middle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window);
        let idx = self.index_for_mouse_position(event.position);
        self.move_to(idx, cx);
        if let Some(text) = crate::helpers::read_primary_selection_text(cx) {
            let single = sanitize_single_line_paste(&text);
            if !single.is_empty() {
                self.replace_text_in_range(None, &single, window, cx);
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.is_selecting = false;
            self.sync_primary_selection(cx);
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let clamped = offset.min(self.content.len());
        self.selected_range = clamped..clamped;
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        let v = line.closest_index_for_x(position.x - bounds.left());
        visual_byte_to_content_byte(&self.content, v, self.masked)
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.content.len());
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let s = range.start.min(self.content.len());
        let e = range.end.min(self.content.len());
        if s != e || !new_text.is_empty() {
            self.push_undo_snapshot();
        }
        self.content = (self.content[0..s].to_owned() + new_text + &self.content[e..]).into();
        let next = s + new_text.len();
        self.selected_range = next..next;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let s = range.start.min(self.content.len());
        let e = range.end.min(self.content.len());
        self.content = (self.content[0..s].to_owned() + new_text + &self.content[e..]).into();
        if !new_text.is_empty() {
            self.marked_range = Some(s..s + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|nr| (nr.start + s)..(nr.end + s))
            .unwrap_or_else(|| (s + new_text.len())..(s + new_text.len()));
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let vs = content_byte_to_visual_byte(&self.content, range.start, self.masked);
        let ve = content_byte_to_visual_byte(&self.content, range.end, self.masked);
        Some(Bounds::from_corners(
            point(bounds.left() + last_layout.x_for_index(vs), bounds.top()),
            point(bounds.left() + last_layout.x_for_index(ve), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        pt: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&pt)?;
        let last_layout = self.last_layout.as_ref()?;
        let visual_index = last_layout.index_for_x(pt.x - line_point.x)?;
        let utf8_index = visual_byte_to_content_byte(&self.content, visual_index, self.masked);
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct InputTextElement {
    input: Entity<TextInput>,
}

struct InputPrepaint {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for InputTextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InputTextElement {
    type RequestLayoutState = ();
    type PrepaintState = InputPrepaint;

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
        let content = input.content.clone();
        let masked = input.masked;
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let (display_text, color): (SharedString, Hsla) = if content.is_empty() {
            (input.placeholder.clone(), input.placeholder_color)
        } else if masked {
            ("•".repeat(content.chars().count()).into(), input.text_color)
        } else {
            (content.clone(), input.text_color)
        };
        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked) = (!masked).then_some(input.marked_range.as_ref()).flatten()
        {
            vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end.saturating_sub(marked.start),
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len().saturating_sub(marked.end),
                    ..run
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);
        let visual_cursor = if content.is_empty() {
            0
        } else {
            content_byte_to_visual_byte(&content, cursor, masked)
        };
        let cursor_x = line.x_for_index(visual_cursor.min(line.text.len()));
        let (selection, cursor_quad) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_x, bounds.top()),
                        size(px(1.5), bounds.bottom() - bounds.top()),
                    ),
                    input.cursor_color,
                )),
            )
        } else {
            let vs = content_byte_to_visual_byte(&content, selected_range.start, masked);
            let ve = content_byte_to_visual_byte(&content, selected_range.end, masked);
            let s = vs.min(line.text.len());
            let e = ve.min(line.text.len());
            (
                Some(fill(
                    Bounds::from_corners(
                        point(bounds.left() + line.x_for_index(s), bounds.top()),
                        point(bounds.left() + line.x_for_index(e), bounds.bottom()),
                    ),
                    rgba(0x3b82f644),
                )),
                None,
            )
        };
        InputPrepaint {
            line: Some(line),
            cursor: cursor_quad,
            selection,
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
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(sel) = prepaint.selection.take() {
            window.paint_quad(sel);
        }
        let line = prepaint.line.take().unwrap();
        let _ = line.paint(bounds.origin, window.line_height(), window, cx);
        if focus_handle.is_focused(window)
            && let Some(c) = prepaint.cursor.take()
        {
            window.paint_quad(c);
        }
        self.input.update(cx, |inp, _| {
            inp.last_layout = Some(line);
            inp.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let mut root = div().flex().items_center().w_full();
        if self.borderless {
            root = root.h_full().px_1();
        } else {
            let input_h = (30.0 * self.ui_scale).round();
            let input_r = if self.classic_style { 3.0 } else { 6.0 };
            root = root
                .h(px(input_h))
                .px_2()
                .rounded(px(input_r))
                .bg(self.bg)
                .border_1()
                .border_color(if focused {
                    self.border_focus
                } else {
                    self.border
                });
        }
        let font_sz = (13.0 * self.font_scale).round();
        let line_h = (font_sz * 1.54).round().max(18.0);
        let el = root
            .key_context("FerriteInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move));
        #[cfg(target_os = "linux")]
        let el = el.on_mouse_down(MouseButton::Middle, cx.listener(Self::on_middle_mouse_down));
        el.line_height(px(line_h))
            .text_size(px(font_sz))
            .overflow_hidden()
            .child(InputTextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_masked_offsets_across_utf8_bullets() {
        let s = "päss1";
        assert_eq!(content_byte_to_visual_byte(s, 0, true), 0);
        // "p" (1 byte) -> 1 char -> 3 visual bytes
        assert_eq!(content_byte_to_visual_byte(s, 1, true), 3);
        // "pä" (3 bytes) -> 2 chars -> 6 visual bytes
        assert_eq!(content_byte_to_visual_byte(s, 3, true), 6);
        assert_eq!(visual_byte_to_content_byte(s, 6, true), 3);
        assert_eq!(visual_byte_to_content_byte(s, 9, true), 4);
        assert_eq!(visual_byte_to_content_byte(s, 99, true), s.len());
    }

    #[test]
    fn undo_redo_history_stack_bounds_and_restores_cursor() {
        let mut undo: Vec<InputSnapshot> = Vec::new();
        let mut redo: Vec<InputSnapshot> = Vec::new();

        for i in 0..70 {
            let snap = InputSnapshot {
                content: format!("step-{i}"),
                selected_range: i..i,
                selection_reversed: false,
            };
            if undo.len() >= MAX_UNDO_HISTORY {
                undo.remove(0);
            }
            undo.push(snap);
        }
        assert_eq!(undo.len(), MAX_UNDO_HISTORY);
        assert_eq!(undo.first().unwrap().content, "step-6");
        assert_eq!(undo.last().unwrap().content, "step-69");

        let popped = undo.pop().unwrap();
        redo.push(popped.clone());
        assert_eq!(popped.selected_range, 69..69);
        assert_eq!(redo.pop().unwrap(), popped);
    }

    #[test]
    fn sanitizes_single_line_paste_text() {
        assert_eq!(
            sanitize_single_line_paste("https://example.com/file.iso\r\n"),
            "https://example.com/file.iso"
        );
        assert_eq!(
            sanitize_single_line_paste("https://a.test/1\nhttps://a.test/2\n"),
            "https://a.test/1 https://a.test/2"
        );
        assert_eq!(sanitize_single_line_paste("plain text"), "plain text");
    }

    #[test]
    fn word_selection_handles_urls_spaces_and_utf8() {
        let url = "https://cdn.example.com/archive.tar.gz";
        assert_eq!(word_selection_range(url, 8), 0..url.len());
        assert_eq!(word_selection_range(url, url.len()), 0..url.len());

        let words = "hello world foo";
        assert_eq!(word_selection_range(words, 7), 6..11);
        assert_eq!(word_selection_range(words, 5), 5..6);

        let utf8 = "päss word";
        assert_eq!(word_selection_range(utf8, 2), 0..5);
        assert_eq!(word_selection_range("", 0), 0..0);
    }

    #[test]
    fn text_input_key_bindings_match_platform_clipboard_shortcuts() {
        let keymap = gpui::Keymap::new(text_input_key_bindings());
        let mut ctx = gpui::KeyContext::new_with_defaults();
        ctx.add("FerriteInput");
        let contexts = [ctx];

        let primary_mod = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };

        let cases = [
            (format!("{primary_mod}-c"), "ferrite_input::InputCopy"),
            (format!("{primary_mod}-v"), "ferrite_input::InputPaste"),
            (format!("{primary_mod}-x"), "ferrite_input::InputCut"),
            (format!("{primary_mod}-a"), "ferrite_input::InputSelectAll"),
            (format!("{primary_mod}-z"), "ferrite_input::InputUndo"),
            (format!("{primary_mod}-shift-z"), "ferrite_input::InputRedo"),
        ];

        for (stroke_str, expected_action) in cases {
            let stroke = gpui::Keystroke::parse(&stroke_str).expect("valid keystroke");
            let (bindings, _) = keymap.bindings_for_input(&[stroke], &contexts);
            assert!(
                bindings
                    .iter()
                    .any(|b| b.action().name() == expected_action),
                "expected {stroke_str} to bind {expected_action}, got {:?}",
                bindings
                    .iter()
                    .map(|b| b.action().name())
                    .collect::<Vec<_>>()
            );
        }

        #[cfg(not(target_os = "macos"))]
        {
            for (stroke_str, expected_action) in [
                ("ctrl-y", "ferrite_input::InputRedo"),
                ("ctrl-insert", "ferrite_input::InputCopy"),
                ("shift-insert", "ferrite_input::InputPaste"),
                ("shift-delete", "ferrite_input::InputCut"),
                ("copy", "ferrite_input::InputCopy"),
                ("paste", "ferrite_input::InputPaste"),
                ("cut", "ferrite_input::InputCut"),
            ] {
                let stroke = gpui::Keystroke::parse(stroke_str).expect("valid keystroke");
                let (bindings, _) = keymap.bindings_for_input(&[stroke], &contexts);
                assert!(
                    bindings
                        .iter()
                        .any(|b| b.action().name() == expected_action),
                    "expected {stroke_str} to bind {expected_action}"
                );
            }

            // Ensure Super/cmd-v is NOT the primary paste binding on Linux.
            let super_v = gpui::Keystroke::parse("cmd-v").expect("valid keystroke");
            let (bindings, _) = keymap.bindings_for_input(&[super_v], &contexts);
            assert!(bindings.is_empty());
        }
    }
}
