use super::*;

fn ready_list() -> GroupUi {
    let mut ui = GroupUi::new();
    let vault = large_list_vault();
    ui.use_memory_vault(vault);
    ui.app.dialog = None;
    for _ in 0..3 {
        frame(&mut ui, Vec::new());
    }
    ui
}

fn frame(ui: &mut GroupUi, events: Vec<egui::Event>) {
    ui.time += 0.1;
    let mut input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, ui.size)),
        time: Some(ui.time),
        events,
        ..Default::default()
    };
    let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
    viewport.native_pixels_per_point = Some(1.0);
    viewport.maximized = Some(false);
    viewport.minimized = Some(false);
    viewport.outer_rect = Some(Rect::from_min_size(Pos2::new(100.0, 60.0), ui.size));
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

fn settle(ui: &mut GroupUi) {
    for _ in 0..10 {
        frame(ui, Vec::new());
    }
}

fn scroll_down(ui: &mut GroupUi) {
    frame(
        ui,
        vec![
            egui::Event::PointerMoved(Pos2::new(100.0, 230.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, -100_000.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    settle(ui);
}

fn click(ui: &mut GroupUi, point: Pos2) {
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
    frame(ui, Vec::new());
}

fn replace_query(ui: &mut GroupUi, query: &str) {
    let point = ui.point(&ui.app.query);
    click(ui, point);
    frame(
        ui,
        vec![
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                },
            },
            egui::Event::Text(query.into()),
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    assert_eq!(ui.app.query, query);
}

#[test]
fn search_query_history_keeps_scroll_state_bounded_without_saving() {
    let mut ui = ready_list();
    let original = ui.app.runtime.app.current.clone().unwrap();
    let token = ui.app.model.change_token();
    ui.app.query = "row".into();
    frame(&mut ui, Vec::new());
    let count = ui.ctx.data(|data| data.count::<egui::scroll_area::State>());
    // Every value is a normal possible query, alternating populated and empty
    // results. No ScrollState or private library internals are manufactured.
    for index in 0..1_000 {
        ui.app.query = if index % 2 == 0 {
            format!("row {index:04}")
        } else {
            format!("not present {index}")
        };
        frame(&mut ui, Vec::new());
        assert_eq!(
            ui.ctx.data(|data| data.count::<egui::scroll_area::State>()),
            count
        );
        assert_eq!(ui.app.model.change_token(), token);
        let app = &ui.app.runtime.app;
        assert_eq!(app.current.as_ref(), Some(&original));
        assert!(!app.saving && !app.cleaning && !app.has_pending());
    }
}

#[test]
fn changed_query_resets_on_first_frame_and_unchanged_query_keeps_scroll() {
    let mut ui = ready_list();
    ui.app.query = "row".into();
    settle(&mut ui);
    scroll_down(&mut ui);
    assert!(!ui.points("Row 0999").is_empty());
    settle(&mut ui);
    assert!(!ui.points("Row 0999").is_empty());
    // Use the actual search TextEdit, including its changed response.
    replace_query(&mut ui, "row 0");
    assert!(!ui.points("Row 0000").is_empty());
    assert!(ui.points("Row 0999").is_empty());
    settle(&mut ui);
    assert!(!ui.points("Row 0000").is_empty());
    scroll_down(&mut ui);
    assert!(!ui.points("Row 0999").is_empty());
    // An ordinary model refresh must not reset an unchanged search.
    let previous_revision = ui.app.runtime.app.current.as_ref().unwrap().revision;
    let before = ui.app.model.change_token();
    ui.app.model.add_group("New empty group").unwrap();
    ui.app.persist_frame_changes(before, &ui.ctx);
    wait_runtime(&mut ui);
    settle(&mut ui);
    assert!(!ui.points("Row 0999").is_empty());
    assert_eq!(
        ui.app.runtime.app.current.as_ref().unwrap().revision,
        previous_revision + 1
    );
}

#[test]
fn ordinary_group_scroll_positions_survive_switching_and_search() {
    let mut ui = ready_list();
    let original = ui.app.runtime.app.current.clone().unwrap();
    let token = ui.app.model.change_token();
    scroll_down(&mut ui);
    assert!(!ui.points("Row 0099").is_empty());
    ui.app.switch_group(1);
    settle(&mut ui);
    assert!(!ui.points("Row 0100").is_empty());
    scroll_down(&mut ui);
    assert!(!ui.points("Row 0199").is_empty());
    ui.app.switch_group(0);
    settle(&mut ui);
    assert!(!ui.points("Row 0099").is_empty());
    ui.app.query = "row".into();
    settle(&mut ui);
    scroll_down(&mut ui);
    assert!(!ui.points("Row 0999").is_empty());
    ui.app.switch_group(1);
    settle(&mut ui);
    assert!(!ui.points("Row 0999").is_empty());
    ui.app.query.clear();
    settle(&mut ui);
    assert!(!ui.points("Row 0199").is_empty());
    ui.app.switch_group(0);
    settle(&mut ui);
    assert!(!ui.points("Row 0099").is_empty());
    assert_eq!(ui.app.model.change_token(), token);
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    assert!(!ui.app.runtime.app.saving && !ui.app.runtime.app.has_pending());
}
