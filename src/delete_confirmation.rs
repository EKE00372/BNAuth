use super::*;
use std::collections::HashMap;

#[derive(PartialEq)]
struct LayoutKey {
    width: f32,
    pixels_per_point: f32,
    font: FontId,
    spacing: f32,
}

pub(super) struct DeleteConfirmation {
    pub(super) ids: Vec<u64>,
    token: Option<u64>,
    labels: Vec<String>,
    layout: Option<LayoutKey>,
    offsets: Vec<f32>,
    #[cfg(test)]
    pub(super) preparations: usize,
    #[cfg(test)]
    pub(super) layouts: usize,
    #[cfg(test)]
    pub(super) rendered_rows: usize,
}

impl DeleteConfirmation {
    pub(super) fn new(ids: Vec<u64>) -> Self {
        Self {
            ids,
            token: None,
            labels: Vec::new(),
            layout: None,
            offsets: Vec::new(),
            #[cfg(test)]
            preparations: 0,
            #[cfg(test)]
            layouts: 0,
            #[cfg(test)]
            rendered_rows: 0,
        }
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui, model: &VaultModel) {
        if self.token.is_none() {
            // A new batch starts at the top; reflowing an open batch keeps its position.
            let id = ui.make_persistent_id(egui::IdSalt::new("delete_list"));
            ui.ctx()
                .data_mut(|data| data.remove::<egui::scroll_area::State>(id));
        }
        if self.token != Some(model.change_token()) {
            let entries: HashMap<_, _> = model.entries.iter().map(|e| (e.id, e)).collect();
            let groups: HashMap<_, _> = model
                .groups
                .iter()
                .map(|g| (g.id, g.name.as_str()))
                .collect();
            self.labels = self
                .ids
                .iter()
                .filter_map(|id| entries.get(id))
                .map(|e| format!("{} · {}", e.name, groups[&e.group_id]))
                .collect();
            self.token = Some(model.change_token());
            self.layout = None;
            #[cfg(test)]
            {
                self.preparations += 1;
            }
        }
        #[cfg(test)]
        {
            self.rendered_rows = 0;
        }
        egui::ScrollArea::vertical()
            .id_salt("delete_list")
            .max_height(200.0)
            .show_viewport(ui, |ui, viewport| {
                let key = LayoutKey {
                    width: ui.available_width(),
                    pixels_per_point: ui.pixels_per_point(),
                    font: egui::FontSelection::Default.resolve(ui.style()),
                    spacing: ui.spacing().item_spacing.y,
                };
                let reflowed = self.layout.as_ref() != Some(&key);
                if reflowed {
                    self.offsets.clear();
                    self.offsets.push(0.0);
                    for label in &self.labels {
                        let galley = egui::WidgetText::from(label.as_str()).into_galley(
                            ui,
                            Some(egui::TextWrapMode::Wrap),
                            key.width,
                            key.font.clone(),
                        );
                        self.offsets
                            .push(self.offsets.last().unwrap() + galley.size().y + key.spacing);
                    }
                    self.layout = Some(key);
                    #[cfg(test)]
                    {
                        self.layouts += 1;
                    }
                }
                // Keep only numeric geometry: cached Galleys can outlive their font atlas.
                let total = (self.offsets.last().unwrap() - ui.spacing().item_spacing.y).max(0.0);
                ui.set_height(total);
                // Reflow can put the old offset beyond the new scroll range,
                // even when some tail rows still intersect the viewport.
                // ScrollArea clamps after this callback; paint that corrected
                // position in the same frame, without altering normal scrolling.
                let max_offset = (total - viewport.height()).max(0.0);
                if reflowed && viewport.min.y > max_offset {
                    ui.ctx()
                        .request_discard("Delete list shrank; clamp scroll before painting");
                    return;
                }
                let first = self
                    .offsets
                    .partition_point(|&y| y <= viewport.min.y)
                    .saturating_sub(1)
                    .min(self.labels.len());
                let end = self
                    .offsets
                    .partition_point(|&y| y < viewport.max.y)
                    .min(self.labels.len());
                let origin = ui.max_rect().top();
                let rect = Rect::from_x_y_ranges(
                    ui.max_rect().x_range(),
                    origin + self.offsets[first]..=origin + self.offsets[end],
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    ui.skip_ahead_auto_ids(first);
                    for label in &self.labels[first..end] {
                        ui.add(egui::Label::new(label).wrap());
                        #[cfg(test)]
                        {
                            self.rendered_rows += 1;
                        }
                    }
                });
            });
    }
}
