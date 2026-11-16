use super::*;

fn frame(ui: &mut GroupUi, events: Vec<egui::Event>) {
    ui.time += 0.04;
    // ui.size is physical here, matching egui-winit across GUI zoom changes.
    let pixels_per_point = ui.ctx.zoom_factor();
    let size = ui.size / pixels_per_point;
    let origin = Pos2::new(100.0, 60.0) / pixels_per_point;
    let mut input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
        time: Some(ui.time),
        events,
        ..Default::default()
    };
    let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
    viewport.native_pixels_per_point = Some(1.0);
    viewport.maximized = Some(false);
    viewport.minimized = Some(false);
    viewport.outer_rect = Some(Rect::from_min_size(origin, size));
    viewport.inner_rect = viewport.outer_rect;
    let ctx = ui.ctx.clone();
    let mut native_frame = eframe::Frame::_new_kittest();
    let mut output = ctx.run_ui(input, |root| {
        eframe::App::logic(&mut ui.app, root.ctx(), &mut native_frame);
        eframe::App::ui(&mut ui.app, root, &mut native_frame);
    });
    output.textures_delta.clear();
    ui.shapes = output.shapes;
}

fn click(ui: &mut GroupUi, label: &str) {
    let point = ui.point(label);
    for pressed in [true, false] {
        frame(
            ui,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    // Keep the release frame: opening a confirmation must already paint its top.
}

fn scroll_to_tail(ui: &mut GroupUi) {
    let pointer = ui.dialog_content_rect().min + Vec2::new(60.0, 90.0);
    frame(
        ui,
        vec![
            egui::Event::PointerMoved(pointer),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, -100_000.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    for _ in 0..30 {
        frame(ui, Vec::new());
    }
}

fn visible_group_rows(ui: &GroupUi) -> Vec<usize> {
    (100..200)
        .filter(|row| !ui.points(&format!("Row {row:04} · Group 1")).is_empty())
        .collect()
}

#[test]
fn each_delete_batch_starts_at_the_top_without_resetting_an_open_batch_or_saving() {
    let mut ui = GroupUi::new();
    let vault = large_list_vault();
    ui.use_memory_vault(vault.clone());
    ui.app.dialog = None;
    for _ in 0..4 {
        frame(&mut ui, Vec::new());
    }
    let token = ui.app.model.change_token();
    click(&mut ui, "全選");
    assert_eq!(ui.app.selected.len(), 100);
    click(&mut ui, "刪除");
    assert!(!ui.points("Row 0000 · 未分組").is_empty());
    scroll_to_tail(&mut ui);
    assert!(!ui.points("Row 0099 · 未分組").is_empty());
    click(&mut ui, "×");
    assert!(ui.app.dialog.is_none());
    frame(&mut ui, Vec::new());
    click(&mut ui, "Group 1");
    assert_eq!(ui.app.group, 1);
    click(&mut ui, "全選");
    assert_eq!(ui.app.selected.len(), 100);
    click(&mut ui, "刪除");
    assert!(!ui.points("Row 0100 · Group 1").is_empty());
    assert!(ui.points("Row 0199 · Group 1").is_empty());
    let scroll_states = ui.ctx.data(|data| data.count::<egui::scroll_area::State>());

    scroll_to_tail(&mut ui);
    assert!(!ui.points("Row 0199 · Group 1").is_empty());
    // Reflow preserves the current offset, rather than forcing the tail to stay
    // anchored when rounded line heights change with the GUI scale.
    for (width, zoom) in [(360.0, 1.0), (500.0, 1.25), (400.0, 1.0)] {
        ui.size.x = width;
        ui.ctx.set_zoom_factor(zoom);
        frame(&mut ui, Vec::new());
        let first_frame = visible_group_rows(&ui);
        assert!(!first_frame.is_empty());
        assert!(
            first_frame[0] > 100,
            "an open batch must not reset to the top"
        );
        for _ in 0..4 {
            frame(&mut ui, Vec::new());
        }
        assert_eq!(
            visible_group_rows(&ui),
            first_frame,
            "reflow must settle in its first frame"
        );
    }
    scroll_to_tail(&mut ui);
    assert!(!ui.points("Row 0199 · Group 1").is_empty());

    // Reopening even the same selection is a fresh confirmation, with stable IDs.
    click(&mut ui, "×");
    frame(&mut ui, Vec::new());
    click(&mut ui, "刪除");
    assert!(!ui.points("Row 0100 · Group 1").is_empty());
    assert!(ui.points("Row 0199 · Group 1").is_empty());
    assert_eq!(
        ui.ctx.data(|data| data.count::<egui::scroll_area::State>()),
        scroll_states
    );
    assert_eq!(ui.app.model.snapshot(), vault);
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&vault));
    assert_eq!(ui.app.model.change_token(), token);
    assert!(!ui.app.runtime.app.saving && !ui.app.runtime.app.has_pending());
}
