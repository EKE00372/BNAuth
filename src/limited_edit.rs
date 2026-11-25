//! Guard legacy long drafts before egui 0.37.0 deletes a replacement's selection.
//!
//! Keep this small preflight aligned with the Windows single-line event paths in
//! egui's widgets/text_edit/builder.rs. The real widget still owns all editing,
//! selection, IME and Undo state; the shadow only decides which input to hide.

use eframe::egui::{self, Event, Id, ImeEvent, Key, Modifiers, TextBuffer};
use egui::text::{CCursor, CCursorRange};

const LIMIT: usize = super::TEXT_INPUT_LIMIT;

fn composition_id(id: Id) -> Id {
    id.with("limited_edit_composing")
}

pub(super) fn reset(ctx: &egui::Context, id: Id) {
    ctx.data_mut(|data| {
        data.remove::<bool>(composition_id(id));
        data.remove::<bool>(id.with("legacy_long_draft"));
    });
}

pub(super) fn name_and_note(
    ui: &mut egui::Ui,
    (name, name_id): (&mut String, Id),
    (note, note_id): (&mut String, Id),
    name_hint: Option<&str>,
) -> (egui::Response, egui::Response) {
    ui.label("名稱 *");
    let name_response = show(ui, name, name_id, name_hint);
    ui.label("備註");
    let note_response = show(ui, note, note_id, None);
    (name_response, note_response)
}

#[must_use]
pub(super) struct FieldPaste(Option<(egui::Context, Vec<(usize, String)>)>);

impl Drop for FieldPaste {
    fn drop(&mut self) {
        if let Some((ctx, originals)) = self.0.take() {
            ctx.input_mut(|input| {
                for (index, text) in originals {
                    input.events[index] = Event::Paste(text);
                }
            });
        }
    }
}

pub(super) fn prepare_paste(
    ui: &egui::Ui,
    id: Id,
    limit: usize,
    keep: impl Fn(char) -> bool,
) -> FieldPaste {
    let mut originals = Vec::new();
    if ui.memory(|memory| memory.has_focus(id)) {
        // The focused editor sees a bounded paste. Another field receiving focus
        // later in the same frame can still process the original clipboard text.
        ui.input_mut(|input| {
            for (index, event) in input.events.iter_mut().enumerate() {
                if let Event::Paste(pasted) = event {
                    if pasted.len() <= limit && pasted.chars().all(&keep) {
                        continue;
                    }
                    let accepted: String =
                        pasted.chars().filter(|ch| keep(*ch)).take(limit).collect();
                    if accepted.len() != pasted.len() {
                        originals.push((index, std::mem::replace(pasted, accepted)));
                    }
                }
            }
        });
    }
    FieldPaste((!originals.is_empty()).then(|| (ui.ctx().clone(), originals)))
}

fn show(ui: &mut egui::Ui, text: &mut String, id: Id, hint: Option<&str>) -> egui::Response {
    let focused = ui.memory(|memory| memory.has_focus(id));
    // The extra character distinguishes an oversized replacement of legacy text from an exact fit.
    let _paste = prepare_paste(ui, id, LIMIT + 1, |ch| ch > '\u{001f}');
    // Only a draft that once exceeded the current limit can restore an oversized
    // value with Undo. Ordinary typing and IME go directly to the real widget.
    let history_id = id.with("legacy_long_draft");
    // The draft and its Undo history are reset together when opening a form.
    let legacy = ui.ctx().data_mut(|data| {
        *data.get_temp_mut_or_insert_with(history_id, || {
            text.len() > LIMIT && text.chars().nth(LIMIT).is_some()
        })
    });
    let owns_ime = legacy && ui.memory(|memory| memory.owns_ime_events(id));
    let mut composing = owns_ime
        && ui
            .ctx()
            .data(|data| data.get_temp::<bool>(composition_id(id)).unwrap_or(false));
    let needs_guard = legacy
        && ui.input(|input| {
            (input.events.iter().any(|event| {
                if focused {
                    replacement(event, id).is_some()
                } else {
                    action_value(event, id, egui::accesskit::Action::SetValue).is_some()
                }
            }) && ((text.len() > LIMIT && text.chars().nth(LIMIT).is_some())
                || (focused && input.events.iter().any(is_undo_redo))))
                || (owns_ime
                    && ((composing && !input.events.is_empty())
                        || input
                            .events
                            .iter()
                            .any(|event| matches!(event, Event::Ime(_)))))
        });
    let mut hidden = Vec::new();
    if needs_guard {
        let mut events = ui.input_mut(|input| std::mem::take(&mut input.events));
        let (rejected, composition) = rejected_events(ui, text, id, focused, composing, &events);
        composing = composition;
        for index in rejected {
            // Move rejected payloads instead of cloning a potentially large paste.
            hidden.push((
                index,
                std::mem::replace(&mut events[index], Event::Text(String::new())),
            ));
        }
        ui.input_mut(|input| input.events = events);
    }
    let mut edit = egui::TextEdit::singleline(text)
        .id(id)
        .char_limit(LIMIT)
        .desired_width(f32::INFINITY);
    if let Some(hint) = hint {
        edit = edit.hint_text(hint);
    }
    let output = edit.show(ui);
    // Pointer interaction follows keyboard/IME handling inside TextEdit.
    let shift = ui.input(|input| input.modifiers.shift);
    if legacy
        && output.response.interact_pointer_pos().is_some()
        && (output.response.clicked()
            || (!shift && (output.response.double_clicked() || output.response.triple_clicked()))
            || (output.response.sense.senses_drag()
                && ((output.response.hovered() && ui.input(|input| input.pointer.any_pressed()))
                    || ui.is_being_dragged(id))))
    {
        composing = false;
    }
    if legacy {
        ui.ctx()
            .data_mut(|data| data.insert_temp(composition_id(id), composing));
    }
    if !hidden.is_empty() {
        ui.input_mut(|input| {
            for (index, event) in hidden {
                input.events[index] = event;
            }
        });
    }
    output.response.response
}

fn action_value(event: &Event, id: Id, action: egui::accesskit::Action) -> Option<&str> {
    match event {
        Event::AccessKitActionRequest(request)
            if request.action == action
                && request.target_node == id.accesskit_id()
                && request.target_tree == egui::accesskit::TreeId::ROOT =>
        {
            match &request.data {
                Some(egui::accesskit::ActionData::Value(value)) => Some(value),
                _ => None,
            }
        }
        _ => None,
    }
}

fn replacement(event: &Event, id: Id) -> Option<&str> {
    match event {
        Event::Text(text) | Event::Paste(text) | Event::Ime(ImeEvent::Commit(text)) => Some(text),
        Event::Ime(ImeEvent::Preedit { text, .. }) => Some(text),
        _ => action_value(event, id, egui::accesskit::Action::SetValue)
            .or_else(|| action_value(event, id, egui::accesskit::Action::ReplaceSelectedText)),
    }
}

fn is_undo_redo(event: &Event) -> bool {
    matches!(event, Event::Key { key: Key::Z | Key::Y, pressed: true, modifiers, .. }
        if modifiers.command)
}

fn galley(ui: &egui::Ui, id: Id, text: &str) -> std::sync::Arc<egui::Galley> {
    let style: egui::widget_style::TextEditStyle = ui.widget_style(id, &Default::default());
    let color = ui
        .visuals()
        .override_text_color
        .unwrap_or(style.atom_layout.text_style.color);
    let font = style.atom_layout.text_style.font_id;
    let line_height =
        ui.fonts_mut(|fonts| fonts.row_height(&font)) + ui.spacing().extra_text_line_spacing;
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.halign = style
        .atom_layout
        .align2
        .unwrap_or_else(|| ui.layout().align2())
        .x();
    job.keep_trailing_whitespace = true;
    for section in &mut job.sections {
        section.format.line_height = Some(line_height);
    }
    ui.fonts_mut(|fonts| fonts.layout_job(job))
}

fn replace(
    text: &mut String,
    range: CCursorRange,
    value: &str,
    normalize_newlines: bool,
) -> Option<CCursor> {
    let count = text.chars().count();
    let [start, end] = range.sorted_cursors();
    let retained = count.saturating_sub(usize::from(end.index - start.index));
    let capacity = LIMIT.saturating_sub(retained);
    if count > LIMIT
        && !value.is_empty()
        && (retained > LIMIT || value.chars().take(capacity + 1).count() > capacity)
    {
        return None;
    }
    // Bound the shadow input before allocation, including later ordinary pastes
    // in the same frame after a valid replacement has reduced the legacy draft.
    let accepted: String = value
        .chars()
        .take(capacity)
        .map(|ch| {
            if normalize_newlines && matches!(ch, '\r' | '\n') {
                ' '
            } else {
                ch
            }
        })
        .collect();
    let mut cursor = text.delete_selected(&range);
    text.insert_text_at(&mut cursor, &accepted, LIMIT);
    Some(cursor)
}

fn rejected_events(
    ui: &egui::Ui,
    original: &str,
    id: Id,
    focused: bool,
    mut composing: bool,
    events: &[Event],
) -> (Vec<usize>, bool) {
    use egui::accesskit::Action;
    let owns_ime = ui.memory(|memory| memory.owns_ime_events(id));
    let state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
    let mut text = original.to_owned();
    let mut layout = galley(ui, id, &text);
    let mut range = state
        .cursor
        .range(&layout)
        .unwrap_or_else(|| CCursorRange::one(layout.end()));
    if !owns_ime {
        range = CCursorRange::one(range.primary);
    }
    let mut rejected = Vec::new();
    // SetValue is handled before focus/keyboard input, and egui uses the last
    // matching request. Hide its predecessors too if that last request is refused.
    if let Some((_, value)) = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            action_value(event, id, Action::SetValue).map(|value| (index, value))
        })
        .next_back()
    {
        let everything = CCursorRange::select_all_str(&text);
        if let Some(cursor) = replace(&mut text, everything, value, true) {
            range = CCursorRange::one(cursor);
            layout = galley(ui, id, &text);
        } else {
            rejected.extend(events.iter().enumerate().filter_map(|(index, event)| {
                action_value(event, id, Action::SetValue).map(|_| index)
            }));
        }
    }
    if !focused {
        return (rejected, false);
    }
    let initial = (range, text.clone());
    let mut undoer = None;
    let filter = egui::EventFilter {
        horizontal_arrows: true,
        vertical_arrows: true,
        ..Default::default()
    };
    for (index, event) in events
        .iter()
        .enumerate()
        .filter(|(_, event)| filter.matches(event))
    {
        if range.on_event(egui::os::OperatingSystem::Windows, event, &layout, id) {
            continue;
        }
        let input = match event {
            Event::Text(value) if !value.is_empty() && value != "\n" && value != "\r" => {
                Some((value.as_str(), false, false))
            }
            Event::Paste(value) if !value.is_empty() => Some((value.as_str(), true, false)),
            Event::Ime(ImeEvent::Preedit { text, .. })
                if owns_ime && text != "\n" && text != "\r" && (!text.is_empty() || composing) =>
            {
                Some((text.as_str(), false, !text.is_empty()))
            }
            Event::Ime(ImeEvent::Commit(value))
                if owns_ime
                    && value != "\n"
                    && value != "\r"
                    && (!value.is_empty() || composing) =>
            {
                Some((value.as_str(), false, false))
            }
            _ => action_value(event, id, Action::ReplaceSelectedText)
                .map(|value| (value, true, false)),
        };
        if let Some((value, normalize, preedit)) = input {
            if let Some(cursor) = replace(&mut text, range, value, normalize) {
                range = if preedit {
                    let start = CCursor {
                        index: range.sorted_cursors()[0].index,
                        prefer_next_row: true,
                    };
                    CCursorRange::two(start, cursor)
                } else {
                    CCursorRange::one(cursor)
                };
                composing = preedit;
                layout = galley(ui, id, &text);
            } else {
                rejected.push(index);
            }
            continue;
        }
        let cursor = match event {
            Event::Cut if !range.is_empty() => Some(text.delete_selected(&range)),
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                if *key == Key::Enter && modifiers.matches_logically(Modifiers::NONE) {
                    break;
                }
                let redo = (*key == Key::Y && modifiers.matches_logically(Modifiers::COMMAND))
                    || (*key == Key::Z
                        && modifiers.matches_logically(Modifiers::SHIFT | Modifiers::COMMAND));
                if redo || (*key == Key::Z && modifiers.matches_logically(Modifiers::COMMAND)) {
                    let undoer = undoer.get_or_insert_with(|| {
                        let mut undoer = state.undoer();
                        undoer.feed_state(ui.input(|input| input.time), &initial);
                        undoer
                    });
                    let current = (range, text.clone());
                    let restored = if redo {
                        undoer.redo(&current)
                    } else {
                        undoer.undo(&current)
                    };
                    if let Some((cursor, value)) = restored {
                        range = *cursor;
                        text.clone_from(value);
                        layout = galley(ui, id, &text);
                        composing = false;
                    }
                    continue;
                }
                match key {
                    Key::Backspace => Some(if let Some(cursor) = range.single() {
                        if modifiers.alt || modifiers.ctrl {
                            text.delete_previous_word(cursor)
                        } else {
                            text.delete_previous_char(cursor)
                        }
                    } else {
                        text.delete_selected(&range)
                    }),
                    Key::Delete if !modifiers.shift => {
                        let mut cursor = if let Some(cursor) = range.single() {
                            if modifiers.alt || modifiers.ctrl {
                                text.delete_next_word(cursor)
                            } else {
                                text.delete_next_char(cursor)
                            }
                        } else {
                            text.delete_selected(&range)
                        };
                        cursor.prefer_next_row = true;
                        Some(cursor)
                    }
                    Key::H if modifiers.ctrl => Some(text.delete_previous_char(range.primary)),
                    Key::K if modifiers.ctrl => {
                        Some(text.delete_paragraph_after_cursor(&layout, &range))
                    }
                    Key::U if modifiers.ctrl => {
                        Some(text.delete_paragraph_before_cursor(&layout, &range))
                    }
                    Key::W if modifiers.ctrl => Some(if let Some(cursor) = range.single() {
                        text.delete_previous_word(cursor)
                    } else {
                        text.delete_selected(&range)
                    }),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(cursor) = cursor {
            range = CCursorRange::one(cursor);
            composing = false;
            layout = galley(ui, id, &text);
        }
    }
    (rejected, composing)
}
