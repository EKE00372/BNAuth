//! Bounded list previews; original notes and font-atlas Galleys stay with their owners.
use super::{Color32, FontId, egui, single_line};
use std::{collections::HashMap, sync::Arc};

#[derive(PartialEq)]
struct LayoutKey {
    width: f32,
    font: FontId,
    pixels_per_point: f32,
}

struct Preview {
    key: LayoutKey,
    // None uses the original complete layout, including complex shaping.
    text: Option<String>,
}

#[derive(Default)]
pub(super) struct NotePreviews {
    token: Option<u64>,
    entries: HashMap<u64, Preview>,
    #[cfg(test)]
    pub(super) preparations: usize,
}

impl NotePreviews {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn layout(
        &mut self,
        painter: &egui::Painter,
        id: u64,
        token: u64,
        text: &str,
        size: f32,
        color: Color32,
        width: f32,
    ) -> Arc<egui::Galley> {
        if self.token != Some(token) {
            self.entries.clear();
            self.token = Some(token);
        }
        // Medium notes cost less to lay out once than to try several prefixes.
        if text.len() <= 1024 {
            return single_line(painter, text, size, color, width);
        }
        let key = LayoutKey {
            width: width.max(0.0),
            font: FontId::proportional(size),
            pixels_per_point: painter.pixels_per_point(),
        };
        if self
            .entries
            .get(&id)
            .is_none_or(|preview| preview.key != key)
        {
            let preview = prepare(painter, text, size, color, key.width);
            self.entries.insert(id, Preview { key, text: preview });
            #[cfg(test)]
            {
                self.preparations += 1;
            }
        }
        match &self.entries[&id].text {
            Some(preview) => single_line(painter, preview, size, color, width),
            None => single_line(painter, text, size, color, width),
        }
    }
}

fn prepare(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    color: Color32,
    width: f32,
) -> Option<String> {
    // These characters need no cross-cluster direction/script context. Other
    // scripts, combining sequences and emoji keep the original full layout.
    if !text.chars().all(simple_preview_character) {
        return None;
    }
    // Estimate a visible row from the actual font advances before shaping it.
    // This handles ASCII and CJK without trying several too-short prefixes.
    let font = FontId::proportional(size);
    let target = painter.fonts_mut(|fonts| {
        let mut advance = 0.0;
        text.char_indices()
            .find_map(|(start, c)| {
                let displayed = if matches!(c, '\r' | '\n' | '\t') {
                    ' '
                } else {
                    c
                };
                advance += fonts.glyph_width(&font, displayed);
                (advance >= width).then_some(start + c.len_utf8())
            })
            .unwrap_or(text.len())
    });
    let mut end = prefix_end(text, target.max(128).next_power_of_two());
    loop {
        let next_end = prefix_end(text, end * 2);
        // Require a substantial reduction before trying multiple layouts.
        // A complete ASCII word can otherwise extend a prefix near the full note.
        if next_end >= text.len() / 2 {
            return None;
        }
        let prefix = &text[..end];
        let current = single_line(painter, prefix, size, color, width);
        if current.elided {
            let lookahead = &text[..next_end];
            let expanded = single_line(painter, lookahead, size, color, width);
            // Keep a complete word and extra shaping context beyond the
            // visible tail; don't decide from an elided prefix alone.
            if current.elided == expanded.elided
                && current.rect == expanded.rect
                && current.rows == expanded.rows
            {
                return Some(lookahead.replace(['\r', '\n', '\t'], " "));
            }
        }
        end = next_end;
    }
}

fn prefix_end(text: &str, bytes: usize) -> usize {
    let mut end = text.floor_char_boundary(bytes.min(text.len()));
    // Do not split an ASCII word's shaping context. Unbroken long words use
    // the complete layout instead of inventing a display-character limit.
    while end < text.len()
        && text.as_bytes()[end - 1].is_ascii_alphanumeric()
        && text.as_bytes()[end].is_ascii_alphanumeric()
    {
        end += 1;
    }
    end
}

fn simple_preview_character(c: char) -> bool {
    matches!(c,
        '\r' | '\n' | '\t' | ' '..='~'
        | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}'
        | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{2fa1f}'
        | '\u{30000}'..='\u{323af}'
        | '，' | '。' | '！' | '？' | '；' | '：' | '「' | '」'
        | '『' | '』' | '（' | '）' | '【' | '】' | '《' | '》'
        | '、' | '…' | '—' | '　' | '／'
    )
}

#[cfg(test)]
#[path = "text_preview/tests.rs"]
mod tests;
