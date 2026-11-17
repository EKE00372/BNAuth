use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

const CLOCK_WARNING: &str = "時間不連續，已改用本機時間；請手動校時。";
const NORMAL_SIZE: Vec2 = Vec2::new(400.0, 892.0);

fn ready_ui() -> GroupUi {
    let mut ui = GroupUi::new();
    let vault = bnauth_core::VaultSnapshot::new_empty(IMPORT_TEST_TIME);
    ui.use_memory_vault(vault);
    ui.app.dialog = None;
    for _ in 0..3 {
        visible_frame(&mut ui, NORMAL_SIZE, false);
    }
    ui
}

fn ready_ui_with_authenticator() -> GroupUi {
    ready_ui_with_named_authenticator("既有人工驗證器")
}

fn ready_ui_with_named_authenticator(name: &str) -> GroupUi {
    ready_ui_with_authenticator_details(name, "")
}

fn ready_ui_with_authenticator_details(name: &str, description: &str) -> GroupUi {
    let mut ui = ready_ui();
    let original = ui.app.runtime.app.current.clone().unwrap();
    let material =
        bnauth_core::parse_manual("1111111111111111111111111111111111111111", "", "").unwrap();
    let vault = bnauth_core::prepare_import(
        &original,
        name,
        description,
        original.ungrouped_id().unwrap(),
        material,
        IMPORT_TEST_TIME,
    )
    .unwrap()
    .candidate;
    ui.use_memory_vault(vault);
    for _ in 0..3 {
        visible_frame(&mut ui, NORMAL_SIZE, false);
    }
    ui
}

fn viewport_input(
    ui: &mut GroupUi,
    size: Vec2,
    minimized: bool,
    occluded: bool,
    closing: bool,
) -> egui::RawInput {
    ui.time += 0.04;
    let mut input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
        time: Some(ui.time),
        ..Default::default()
    };
    let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
    viewport.native_pixels_per_point = Some(1.0);
    viewport.maximized = Some(false);
    viewport.minimized = Some(minimized);
    viewport.occluded = Some(occluded);
    let origin = if minimized {
        Pos2::new(-32_000.0, -32_000.0)
    } else {
        Pos2::new(100.0, 60.0)
    };
    viewport.outer_rect = Some(Rect::from_min_size(origin, size));
    viewport.inner_rect = viewport.outer_rect;
    if closing {
        viewport.events.push(egui::ViewportEvent::Close);
    }
    input
}

/// Follow eframe's visible-frame order, including logic before painting.
fn visible_frame(ui: &mut GroupUi, size: Vec2, closing: bool) -> Vec<egui::ViewportCommand> {
    visible_frame_at(ui, size, Pos2::new(100.0, 60.0), closing)
}

fn interactive_frame(ui: &mut GroupUi, events: Vec<egui::Event>) {
    interactive_frame_at(ui, Pos2::new(100.0, 60.0), events);
}

fn interactive_frame_at(ui: &mut GroupUi, origin: Pos2, events: Vec<egui::Event>) {
    let mut input = viewport_input(ui, NORMAL_SIZE, false, false, false);
    input.events = events;
    let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
    viewport.outer_rect = Some(Rect::from_min_size(origin, NORMAL_SIZE));
    viewport.inner_rect = viewport.outer_rect;
    let ctx = ui.ctx.clone();
    let mut frame = eframe::Frame::_new_kittest();
    let mut output = ctx.run_ui(input, |root| {
        eframe::App::logic(&mut ui.app, root.ctx(), &mut frame);
        eframe::App::ui(&mut ui.app, root, &mut frame);
    });
    output.textures_delta.clear();
    ui.shapes = output.shapes;
}

fn click_visible(ui: &mut GroupUi, point: Pos2) {
    for pressed in [true, false] {
        interactive_frame(
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
    interactive_frame(ui, Vec::new());
}

#[test]
fn long_unicode_notes_remain_complete_through_preview_details_and_unchanged_edit_without_writes() {
    fn contains_text(shape: &egui::Shape, text: &str) -> bool {
        match shape {
            egui::Shape::Text(shape) => shape.galley.text() == text,
            egui::Shape::Vec(shapes) => shapes.iter().any(|shape| contains_text(shape, text)),
            _ => false,
        }
    }
    fn preview_source_len(shape: &egui::Shape) -> Option<usize> {
        match shape {
            egui::Shape::Text(shape) if shape.galley.text().starts_with("完整備註起點") => {
                Some(shape.galley.text().len())
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(preview_source_len),
            _ => None,
        }
    }

    // Cover both the shortened simple-text preview and the full-layout Unicode
    // fallback: neither rendering path may change the original stored notes.
    for (shortened, note) in [
        (
            true,
            format!(
                "完整備註起點\r\n{}完整備註終點",
                "戰網驗證器備註\t台灣帳號 Latin Account 12345\r\n第二行含全形文字與標點。\r\n".repeat(100)
            ),
        ),
        (
            false,
            format!(
                "完整備註起點\r\n{}完整備註終點",
                "戰網驗證器備註\t台灣帳號 e\u{301} 👨\u{200d}👩\u{200d}👧\u{200d}👦 🇹🇼\r\n第二行含全形文字與標點。\r\n".repeat(80)
            ),
        ),
    ] {
        assert!(note.len() > 8 * 1024 && note.len() <= 16 * 1024);
        // Use the normal import path so the full note is present in saved data.
        let mut ui = ready_ui_with_authenticator_details("長備註帳號", &note);
        let gate = gated_repository(&mut ui);
        gate.release();
        let original = ui.app.runtime.app.current.clone().unwrap();
        let id = ui.app.model.entries[0].id;
        let token = ui.app.model.change_token();
        assert_eq!(original.authenticators[0].description, note);
        assert_eq!(ui.app.model.entries[0].description, note);
        assert_eq!(ui.app.model.entries[0].record.description, note);
        for _ in 0..8 {
            interactive_frame(&mut ui, Vec::new());
        }
        let preview_len = ui
            .shapes
            .iter()
            .find_map(|shape| preview_source_len(&shape.shape))
            .expect("Expected the list note preview");
        if shortened {
            assert!(preview_len < note.len());
        } else {
            assert_eq!(preview_len, note.len());
        }

        ui.app.dialog = Some(Dialog::Details(id));
        for _ in 0..4 {
            interactive_frame(&mut ui, Vec::new());
        }
        assert!(!ui.points("驗證器資料").is_empty());
        // Inspect the actual label's complete layout source. Most of this long
        // label is outside the viewport, so its center need not be visible.
        assert!(
            ui.shapes
                .iter()
                .any(|shape| contains_text(&shape.shape, &note))
        );
        let close = ui.point("×");
        click_visible(&mut ui, close);
        assert!(ui.app.dialog.is_none());

        ui.app.edit(&ui.ctx, Some(id));
        for _ in 0..4 {
            interactive_frame(&mut ui, Vec::new());
        }
        match ui.app.dialog.as_ref() {
            Some(Dialog::Edit {
                name, description, ..
            }) => {
                assert_eq!(name, &original.authenticators[0].name);
                assert_eq!(description, &note);
            }
            _ => panic!("Expected the original complete edit form"),
        }
        let apply = ui.point("套用");
        click_visible(&mut ui, apply);
        assert!(ui.app.dialog.is_none());

        let physical_size = Vec2::new(1200.0, 1800.0);
        let physical_origin = Pos2::new(100.0, 60.0);
        for (native_dpi, zoom) in [(1.0, 1.5), (2.0, 1.25), (1.0, 1.0)] {
            ui.ctx.set_zoom_factor(zoom);
            for _ in 0..4 {
                physical_frame(&mut ui, physical_size, physical_origin, native_dpi, false);
            }
            assert_eq!(gate.commits.load(Ordering::Relaxed), 0);
            assert_eq!(ui.app.model.change_token(), token);
            assert_eq!(ui.app.model.snapshot(), original);
            assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
            assert_eq!(ui.app.model.entries[0].description, note);
            assert_eq!(ui.app.model.entries[0].record.description, note);
            assert!(!ui.app.runtime.app.saving && !ui.app.runtime.app.has_pending());
        }
        let json = bnauth_adapters::native::encode(&ui.app.model.snapshot()).unwrap();
        assert_eq!(bnauth_adapters::native::decode(&json).unwrap(), original);
    }
}

#[test]
fn unchanged_edit_is_silent_and_zero_writes_but_a_real_edit_still_saves_once() {
    for name in [
        "既有人工驗證器",
        "  帳號",
        "帳號  ",
        "  帳號  ",
        "\u{3000}帳號\u{3000}",
    ] {
        // The normal import path preserves these valid names; do not patch rows
        // directly to manufacture a state that users cannot create.
        let mut ui = ready_ui_with_named_authenticator(name);
        let gate = gated_repository(&mut ui);
        gate.release();
        let original = ui.app.runtime.app.current.clone().unwrap();
        assert_eq!(original.authenticators[0].name, name);
        let id = ui.app.model.entries[0].id;
        let token = ui.app.model.change_token();
        for _ in 0..2 {
            ui.app.edit(&ui.ctx, Some(id));
            for _ in 0..4 {
                interactive_frame(&mut ui, Vec::new());
            }
            let apply = ui.point("套用");
            click_visible(&mut ui, apply);
            assert!(ui.app.dialog.is_none());
            assert!(ui.app.notifications.visible(Instant::now()).is_none());
            assert_eq!(gate.commits.load(Ordering::Relaxed), 0, "{name:?}");
            assert_eq!(ui.app.model.change_token(), token, "{name:?}");
            assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
            // Includes the exact name, timestamps, revision and every other field.
            assert_eq!(ui.app.model.snapshot(), original);
            assert!(!ui.app.runtime.app.saving);
            assert!(!ui.app.runtime.app.has_pending());
        }

        ui.app.edit(&ui.ctx, Some(id));
        for _ in 0..4 {
            interactive_frame(&mut ui, Vec::new());
        }
        let text = *ui.points(&original.authenticators[0].name).last().unwrap();
        click_visible(&mut ui, text);
        interactive_frame(
            &mut ui,
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
                egui::Event::Text("實際修改的名稱".into()),
            ],
        );
        let apply = ui.point("套用");
        click_visible(&mut ui, apply);
        wait_hidden(&mut ui, false);
        assert_eq!(gate.commits.load(Ordering::Relaxed), 1);
        let saved = ui.app.runtime.app.current.as_ref().unwrap();
        assert_eq!(saved.revision, original.revision + 1);
        assert_eq!(saved.authenticators[0].name, "實際修改的名稱");
        assert_eq!(ui.app.notifications.visible(Instant::now()), Some("已保存"));
    }
}

#[test]
fn notes_only_edit_preserves_the_exact_imported_name_and_saves_once() {
    for name in [
        "帳號",
        "  帳號",
        "帳號  ",
        "  帳號  ",
        "\u{3000}帳號\u{3000}",
    ] {
        let mut ui = ready_ui_with_authenticator_details(name, "原始備註");
        let gate = gated_repository(&mut ui);
        gate.release();
        let original = ui.app.runtime.app.current.clone().unwrap();
        let id = ui.app.model.entries[0].id;
        ui.app.edit(&ui.ctx, Some(id));
        for _ in 0..4 {
            interactive_frame(&mut ui, Vec::new());
        }
        let notes = *ui.points("原始備註").last().unwrap();
        click_visible(&mut ui, notes);
        interactive_frame(
            &mut ui,
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
                egui::Event::Text("只修改備註".into()),
            ],
        );
        let apply = ui.point("套用");
        click_visible(&mut ui, apply);
        wait_hidden(&mut ui, false);
        assert!(ui.app.dialog.is_none());
        assert_eq!(gate.commits.load(Ordering::Relaxed), 1);
        let saved = ui.app.runtime.app.current.clone().unwrap();
        assert_eq!(saved.revision, original.revision + 1);
        let mut expected = original.authenticators[0].clone();
        expected.description = "只修改備註".into();
        expected.updated_at_ms = saved.authenticators[0].updated_at_ms;
        assert_eq!(saved.authenticators[0], expected, "{name:?}");
        let json = bnauth_adapters::native::encode(&saved).unwrap();
        assert_eq!(bnauth_adapters::native::decode(&json).unwrap(), saved);

        // Reopening and applying this form unchanged must not submit again.
        ui.app.edit(&ui.ctx, Some(id));
        for _ in 0..4 {
            interactive_frame(&mut ui, Vec::new());
        }
        let apply = ui.point("套用");
        click_visible(&mut ui, apply);
        assert!(ui.app.dialog.is_none());
        assert_eq!(gate.commits.load(Ordering::Relaxed), 1);
        assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&saved));
        assert!(!ui.app.runtime.app.has_pending());
    }
}

#[test]
fn moving_to_the_current_group_is_silent_and_zero_writes_but_a_real_move_saves_once() {
    let mut ui = ready_ui_with_authenticator();
    let mut vault = ui.app.runtime.app.current.clone().unwrap();
    let target = uuid::Uuid::new_v4();
    vault.groups.push(bnauth_core::Group {
        id: target,
        name: "移動目標".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    ui.use_memory_vault(vault.clone());
    let gate = gated_repository(&mut ui);
    gate.release();
    let id = ui.app.model.entries[0].id;
    ui.app.selected.insert(id);
    for _ in 0..4 {
        interactive_frame(&mut ui, Vec::new());
    }
    let button = ui.point("移動");
    click_visible(&mut ui, button);
    let current = ui.point("未分組    1/100");
    click_visible(&mut ui, current);
    assert!(ui.app.notifications.visible(Instant::now()).is_none());
    assert_eq!(gate.commits.load(Ordering::Relaxed), 0);
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&vault));
    assert!(!ui.app.runtime.app.has_pending());

    let button = ui.point("移動");
    click_visible(&mut ui, button);
    let target_item = ui.point("移動目標    0/100");
    click_visible(&mut ui, target_item);
    wait_hidden(&mut ui, false);
    assert_eq!(gate.commits.load(Ordering::Relaxed), 1);
    let saved = ui.app.runtime.app.current.as_ref().unwrap();
    assert_eq!(saved.revision, vault.revision + 1);
    assert_eq!(saved.authenticators[0].group_id, target);
    assert_eq!(ui.app.notifications.visible(Instant::now()), Some("已保存"));
}

#[test]
fn unchanged_loaded_group_rename_never_saves_on_close_or_focus_loss() {
    for name in [
        "分組",
        "  分組",
        "分組  ",
        "  分組  ",
        "\u{3000}分組\u{3000}",
    ] {
        for close in [true, false] {
            let mut ui = ready_ui();
            let mut vault = ui.app.runtime.app.current.clone().unwrap();
            vault.groups.push(bnauth_core::Group {
                id: uuid::Uuid::new_v4(),
                name: name.into(),
                sort_order: 1,
                is_ungrouped: false,
            });
            // Exercise the supported readable-file boundary, with no patched UI rows.
            let json = bnauth_adapters::native::encode(&vault).unwrap();
            let original = bnauth_adapters::native::decode(&json).unwrap();
            ui.use_memory_vault(original.clone());
            let gate = gated_repository(&mut ui);
            gate.release();
            let id = ui.app.model.groups[1].id;
            let token = ui.app.model.change_token();
            ui.open(Dialog::Groups {
                name: String::new(),
                rename: None,
                error: String::new(),
            });
            ui.edit_group(id);
            for _ in 0..3 {
                interactive_frame(&mut ui, Vec::new());
            }
            let point = if close {
                ui.point("×")
            } else {
                ui.group_point(0)
            };
            click_visible(&mut ui, point);
            assert_eq!(
                gate.commits.load(Ordering::Relaxed),
                0,
                "{name:?}, close={close}"
            );
            assert_eq!(ui.app.model.change_token(), token);
            assert_eq!(ui.app.model.snapshot(), original);
            assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
            assert!(!ui.app.runtime.app.has_pending());
            assert!(!ui.app.runtime.app.saving);

            // A real edit through the same form must still submit exactly once.
            ui.open(Dialog::Groups {
                name: String::new(),
                rename: None,
                error: String::new(),
            });
            ui.edit_group(id);
            interactive_frame(&mut ui, vec![egui::Event::Text("實際修改的分組".into())]);
            let point = ui.point("×");
            click_visible(&mut ui, point);
            wait_hidden(&mut ui, false);
            assert_eq!(gate.commits.load(Ordering::Relaxed), 1);
            let saved = ui.app.runtime.app.current.as_ref().unwrap();
            assert_eq!(saved.groups[1].name, "實際修改的分組");
            assert_eq!(saved.revision, original.revision + 1);
        }
    }
}

#[test]
fn delete_confirmation_is_read_only_until_confirm_and_saves_the_selected_batch_once() {
    let mut ui = ready_ui();
    let vault = large_list_vault();
    ui.use_memory_vault(vault.clone());
    let gate = gated_repository(&mut ui);
    gate.release();
    let ids: BTreeSet<_> = ui.app.model.entries[..100].iter().map(|e| e.id).collect();
    let token = ui.app.model.change_token();
    interactive_frame(&mut ui, Vec::new());
    assert_eq!(gate.commits.load(Ordering::Relaxed), 0);
    for confirm in [false, true] {
        ui.app.selected = ids.clone();
        for _ in 0..3 {
            interactive_frame(&mut ui, Vec::new());
        }
        let delete = ui.point("刪除");
        click_visible(&mut ui, delete);
        for _ in 0..4 {
            interactive_frame(&mut ui, Vec::new());
        }
        assert!(matches!(ui.app.dialog, Some(Dialog::Delete(_))));
        assert_eq!(gate.commits.load(Ordering::Relaxed), 0);
        assert_eq!(ui.app.model.change_token(), token);
        assert_eq!(ui.app.model.snapshot(), vault);
        assert!(!ui.app.runtime.app.has_pending());
        let action = ui.point(if confirm { "確認刪除" } else { "×" });
        click_visible(&mut ui, action);
        assert!(ui.app.dialog.is_none());
        if !confirm {
            assert_eq!(gate.commits.load(Ordering::Relaxed), 0);
            assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&vault));
        }
    }
    wait_hidden(&mut ui, false);
    assert_eq!(gate.commits.load(Ordering::Relaxed), 1);
    let saved = ui.app.runtime.app.current.as_ref().unwrap();
    assert_eq!(saved.revision, vault.revision + 1);
    assert_eq!(saved.authenticators, vault.authenticators[100..]);
    assert_eq!(saved.groups, vault.groups);
    saved.validate().unwrap();
}

fn visible_frame_at(
    ui: &mut GroupUi,
    size: Vec2,
    origin: Pos2,
    closing: bool,
) -> Vec<egui::ViewportCommand> {
    ui.size = size;
    let mut input = viewport_input(ui, size, false, false, closing);
    let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
    viewport.outer_rect = Some(Rect::from_min_size(origin, size));
    viewport.inner_rect = viewport.outer_rect;
    let ctx = ui.ctx.clone();
    let mut frame = eframe::Frame::_new_kittest();
    let mut output = ctx.run_ui(input, |root| {
        eframe::App::logic(&mut ui.app, root.ctx(), &mut frame);
        eframe::App::ui(&mut ui.app, root, &mut frame);
    });
    output.textures_delta.clear();
    ui.shapes = output.shapes;
    output
        .viewport_output
        .remove(&egui::ViewportId::ROOT)
        .unwrap()
        .commands
}

/// Supply native pixel coordinates using the same conversion as egui-winit.
/// The first input after set_zoom_factor still uses the previous GUI zoom.
fn physical_frame(
    ui: &mut GroupUi,
    physical_size: Vec2,
    physical_origin: Pos2,
    native_dpi: f32,
    closing: bool,
) -> Vec<egui::ViewportCommand> {
    let pixels_per_point = native_dpi * ui.ctx.zoom_factor();
    let size = physical_size / pixels_per_point;
    let origin = physical_origin / pixels_per_point;
    ui.size = size;
    let mut input = viewport_input(ui, size, false, false, closing);
    let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
    viewport.native_pixels_per_point = Some(native_dpi);
    viewport.outer_rect = Some(Rect::from_min_size(origin, size));
    viewport.inner_rect = viewport.outer_rect;
    let ctx = ui.ctx.clone();
    let mut frame = eframe::Frame::_new_kittest();
    let mut output = ctx.run_ui(input, |root| {
        eframe::App::logic(&mut ui.app, root.ctx(), &mut frame);
        eframe::App::ui(&mut ui.app, root, &mut frame);
    });
    output.textures_delta.clear();
    ui.shapes = output.shapes;
    output
        .viewport_output
        .remove(&egui::ViewportId::ROOT)
        .unwrap()
        .commands
}

fn hidden_frame(ui: &mut GroupUi, minimized: bool, closing: bool) -> Vec<egui::ViewportCommand> {
    let size = ui.size;
    let input = viewport_input(ui, size, minimized, !minimized, closing);
    let ctx = ui.ctx.clone();
    let mut frame = eframe::Frame::_new_kittest();
    let mut output = ctx.run_logic(&input, |ctx| {
        eframe::App::logic(&mut ui.app, ctx, &mut frame);
    });
    output
        .viewport_commands
        .remove(&egui::ViewportId::ROOT)
        .unwrap_or_default()
}

fn wait_hidden(ui: &mut GroupUi, minimized: bool) -> Vec<egui::ViewportCommand> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut commands = Vec::new();
    loop {
        commands.extend(hidden_frame(ui, minimized, false));
        let app = &ui.app.runtime.app;
        if app.phase != bnauth_app::Phase::Loading && !app.saving && !app.cleaning {
            return commands;
        }
        assert!(Instant::now() < deadline, "hidden worker did not finish");
        std::thread::yield_now();
    }
}

struct SaveGate {
    released: Arc<AtomicBool>,
    commits: Arc<AtomicUsize>,
}

impl SaveGate {
    fn release(&self) {
        self.released.store(true, Ordering::Release);
    }
}

impl Drop for SaveGate {
    fn drop(&mut self) {
        // A failed assertion must not leave Application::drop waiting for a gate.
        self.release();
    }
}

struct GatedRepository {
    released: Arc<AtomicBool>,
    commits: Arc<AtomicUsize>,
    fail_commit: bool,
}

impl GatedRepository {
    fn wait_for_release(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.released.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err("人工保存閘門逾時".into());
            }
            std::thread::yield_now();
        }
        Ok(())
    }
}

impl bnauth_app::Repository for GatedRepository {
    fn commit(
        &mut self,
        candidate: &bnauth_core::VaultSnapshot,
    ) -> Result<bnauth_app::CommitOutcome, String> {
        self.commits.fetch_add(1, Ordering::Relaxed);
        self.wait_for_release()?;
        if self.fail_commit {
            return Err("人工延後保存失敗".into());
        }
        candidate.validate()?;
        let mut snapshot = candidate.clone();
        snapshot.revision += 1;
        Ok(bnauth_app::CommitOutcome {
            snapshot,
            warning: None,
        })
    }

    fn retry_cleanup(&mut self) -> Result<(), String> {
        self.wait_for_release()
    }
}

fn gated_repository(ui: &mut GroupUi) -> SaveGate {
    gated_repository_mode(ui, false)
}

fn gated_repository_mode(ui: &mut GroupUi, fail_commit: bool) -> SaveGate {
    let vault = ui.app.runtime.app.current.clone().unwrap();
    let released = Arc::new(AtomicBool::new(false));
    let commits = Arc::new(AtomicUsize::new(0));
    let repo_release = released.clone();
    let repo_commits = commits.clone();
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        Ok((
            Box::new(GatedRepository {
                released: repo_release.clone(),
                commits: repo_commits.clone(),
                fail_commit,
            }),
            Some(vault.clone()),
        ))
    }));
    drain_memory_app(&mut ui.app.runtime.app);
    SaveGate { released, commits }
}

fn fresh_sync_sample() -> bnauth_core::clock::SyncSample {
    let measured_at = Instant::now();
    let wall = bnauth_core::now_ms();
    bnauth_core::clock::validate_sample(wall, wall, wall, measured_at, measured_at).unwrap()
}

fn queue_sync_result(ui: &mut GroupUi, result: Result<bnauth_core::clock::SyncSample, String>) {
    let (sender, receiver) = std::sync::mpsc::channel();
    ui.app.runtime.sync_rx = Some(receiver);
    ui.app.runtime.sync_manual = true;
    sender.send(result.map_err(Into::into)).unwrap();
}

fn candidate_with_new_group(ui: &GroupUi) -> bnauth_core::VaultSnapshot {
    let mut candidate = ui.app.runtime.app.current.clone().unwrap();
    candidate.groups.push(bnauth_core::Group {
        id: uuid::Uuid::new_v4(),
        name: "待保存分組".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    candidate
}

#[test]
fn crossflow_failed_save_without_residue_keeps_its_error_and_offers_no_cleanup() {
    let mut ui = ready_ui();
    let original = ui.app.runtime.app.current.clone().unwrap();
    let gate = gated_repository_mode(&mut ui, true);
    ui.app
        .runtime
        .app
        .commit(candidate_with_new_group(&ui), false)
        .unwrap();
    gate.release();
    wait_hidden(&mut ui, true);
    ui.view = TestView::Saving;
    for _ in 0..3 {
        ui.frame(Vec::new());
    }
    assert!(ui.app.runtime.app.has_pending());
    assert!(!ui.app.runtime.app.repository_state.needs_attention());
    assert!(ui.points("只移除暫存檔").is_empty());
    assert!(!ui.points("人工延後保存失敗").is_empty());
    for _ in 0..3 {
        visible_frame(&mut ui, NORMAL_SIZE, false);
        let app = &ui.app.runtime.app;
        assert_eq!(app.error.as_deref(), Some("人工延後保存失敗"));
        assert!(app.has_pending() && !app.saving && !app.cleaning);
        assert_eq!(app.current.as_ref(), Some(&original));
        assert!(
            ui.points("暫存檔已移除，但這次變更尚未保存。請按「重試保存」。")
                .is_empty()
        );
    }
    assert_eq!(gate.commits.load(Ordering::Relaxed), 1);
}

#[test]
fn crossflow_import_saves_once_and_failed_candidate_owns_its_retry() {
    for fail_commit in [false, true] {
        let mut ui = ready_ui_with_authenticator();
        let original = ui.app.runtime.app.current.clone().unwrap();
        let gate = gated_repository_mode(&mut ui, fail_commit);
        // Model a request already in flight so import never uses the network.
        let (_sender, receiver) = std::sync::mpsc::channel();
        ui.app.runtime.sync_rx = Some(receiver);
        ui.app.runtime.sync_manual = true;
        ui.open(Dialog::Import(Box::new(import_draft())));
        ui.click(ui.point("預覽匯入"));
        let mut expected = ui.import().preview.as_ref().unwrap().candidate.clone();
        expected.revision += 1;
        let moved_to = Pos2::new(150.0, 90.0);
        // Moving the viewport cannot create an additional save.
        visible_frame_at(&mut ui, NORMAL_SIZE, moved_to, false);
        ui.frame(Vec::new());
        ui.click(ui.point("確認匯入並保存"));
        assert!(ui.app.dialog.is_none());
        assert!(ui.app.runtime.app.saving && ui.app.runtime.app.has_pending());
        gate.release();
        wait_hidden(&mut ui, true);
        if fail_commit {
            assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
            assert!(ui.app.runtime.app.has_pending());
            ui.app.runtime.app.retry().unwrap();
            wait_hidden(&mut ui, true);
            assert!(ui.app.runtime.app.has_pending());
            ui.view = TestView::Saving;
            for _ in 0..3 {
                ui.frame(Vec::new());
            }
            ui.click(ui.point("放棄這次未保存變更"));
            assert_eq!(ui.app.model.snapshot(), original);
        } else {
            assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&expected));
            assert_eq!(ui.app.model.snapshot(), expected);
        }
        for _ in 0..3 {
            visible_frame_at(&mut ui, NORMAL_SIZE, moved_to, false);
        }
        assert!(!ui.app.runtime.app.saving && !ui.app.runtime.app.has_pending());
        assert_eq!(
            gate.commits.load(Ordering::Relaxed),
            if fail_commit { 2 } else { 1 }
        );
    }
}

struct WarningWithoutResidueRepository {
    commits: Arc<AtomicUsize>,
    cleanups: Arc<AtomicUsize>,
}

impl bnauth_app::Repository for WarningWithoutResidueRepository {
    fn commit(
        &mut self,
        candidate: &bnauth_core::VaultSnapshot,
    ) -> Result<bnauth_app::CommitOutcome, String> {
        candidate.validate()?;
        self.commits.fetch_add(1, Ordering::Relaxed);
        let mut snapshot = candidate.clone();
        snapshot.revision += 1;
        Ok(bnauth_app::CommitOutcome {
            snapshot,
            warning: Some("資料已保存，但保存過程曾發生異常。".into()),
        })
    }

    fn retry_cleanup(&mut self) -> Result<(), String> {
        self.cleanups.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[test]
fn crossflow_saved_warning_without_residue_is_acknowledged_without_cleanup() {
    for closing in [false, true] {
        let mut ui = ready_ui();
        let original = ui.app.runtime.app.current.clone().unwrap();
        let commits = Arc::new(AtomicUsize::new(0));
        let cleanups = Arc::new(AtomicUsize::new(0));
        let repo_commits = commits.clone();
        let repo_cleanups = cleanups.clone();
        ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
            Ok((
                Box::new(WarningWithoutResidueRepository {
                    commits: repo_commits.clone(),
                    cleanups: repo_cleanups.clone(),
                }),
                Some(original.clone()),
            ))
        }));
        drain_memory_app(&mut ui.app.runtime.app);
        ui.app
            .runtime
            .app
            .commit(candidate_with_new_group(&ui), false)
            .unwrap();
        wait_hidden(&mut ui, true);
        let saved = ui.app.runtime.app.current.clone().unwrap();
        assert!(ui.app.runtime.app.warning.is_some());
        assert!(!ui.app.runtime.app.repository_state.needs_attention());
        assert!(ui.app.runtime.close_blocked());
        ui.app.runtime.close_requested = closing;
        ui.view = TestView::Saving;
        for _ in 0..3 {
            ui.frame(Vec::new());
        }
        assert!(ui.points("重試移除暫存檔").is_empty());
        ui.click(ui.point("繼續"));
        assert!(ui.app.runtime.app.warning.is_none());
        assert!(!ui.app.runtime.close_blocked());
        let commands = visible_frame(&mut ui, NORMAL_SIZE, false);
        assert_eq!(commands.contains(&egui::ViewportCommand::Close), closing);
        assert!(!ui.app.runtime.app.cleaning && !ui.app.runtime.app.has_pending());
        assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&saved));
        assert_eq!(commits.load(Ordering::Relaxed), 1);
        assert_eq!(cleanups.load(Ordering::Relaxed), 0);
        assert!(ui.points("暫存檔已移除。").is_empty());
    }
}

#[test]
fn hidden_close_protects_failed_candidate_and_residue_until_discard_and_cleanup() {
    for minimized in [true, false] {
        let mut ui = ready_ui();
        let vault = ui.app.runtime.app.current.clone().unwrap();
        ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
            Ok((
                Box::new(CleanupFailureRepository {
                    cleanup_required: false,
                }),
                Some(vault.clone()),
            ))
        }));
        drain_memory_app(&mut ui.app.runtime.app);
        let candidate = candidate_with_new_group(&ui);
        ui.app.runtime.app.commit(candidate, false).unwrap();
        wait_hidden(&mut ui, minimized);
        let app = &ui.app.runtime.app;
        assert!(app.has_pending());
        assert!(app.repository_state.cleanup_required);
        assert!(app.error.is_some());

        let commands = hidden_frame(&mut ui, minimized, true);
        assert!(commands.contains(&egui::ViewportCommand::CancelClose));
        assert!(!commands.contains(&egui::ViewportCommand::Close));
        assert!(commands.contains(&egui::ViewportCommand::Focus));
        assert!(ui.app.runtime.close_requested);
        // Explicit permission to retain residue still cannot discard a candidate.
        ui.app.runtime.allow_close_with_residue = true;
        let commands = hidden_frame(&mut ui, minimized, true);
        assert!(commands.contains(&egui::ViewportCommand::CancelClose));
        assert!(!commands.contains(&egui::ViewportCommand::Close));
        ui.app.runtime.allow_close_with_residue = false;
        let commands = visible_frame(&mut ui, NORMAL_SIZE, true);
        assert!(commands.contains(&egui::ViewportCommand::CancelClose));
        assert!(!commands.contains(&egui::ViewportCommand::Close));

        ui.app.runtime.app.discard();
        let commands = hidden_frame(&mut ui, minimized, true);
        assert!(commands.contains(&egui::ViewportCommand::CancelClose));
        assert!(!commands.contains(&egui::ViewportCommand::Close));
        assert_eq!(ui.app.runtime.app.current.as_ref().unwrap().groups.len(), 1);
        ui.app.runtime.app.retry_cleanup().unwrap();
        let commands = wait_hidden(&mut ui, minimized);
        assert!(commands.contains(&egui::ViewportCommand::Close));
        let app = &ui.app.runtime.app;
        assert!(!app.has_pending());
        assert!(!app.repository_state.needs_attention());
        assert_eq!(app.current.as_ref().unwrap().revision, 0);
    }
}

#[test]
fn time_sync_waits_for_a_result_and_reports_failures_after_the_sender_exits() {
    for previously_synced in [false, true] {
        for error in [
            "校時連線逾時；請手動重試。",
            "讀取校時回覆失敗或逾時；請手動重試。",
            "校時回覆格式錯誤，timeMs 必須是唯一的整數 UTC 毫秒。",
        ] {
            let mut ui = ready_ui_with_authenticator();
            let original = ui.app.runtime.app.current.clone().unwrap();
            let gate = gated_repository(&mut ui);
            gate.release();
            if previously_synced {
                ui.app.runtime.clock.apply(fresh_sync_sample()).unwrap();
            }
            let last_success = ui.app.runtime.clock.last_success().cloned();
            let (sender, receiver) = std::sync::mpsc::channel();
            ui.app.runtime.sync_rx = Some(receiver);
            ui.app.runtime.sync_manual = true;

            // An in-flight request has no result yet; keep waiting without
            // changing the current clock or reporting an error.
            for _ in 0..3 {
                visible_frame(&mut ui, NORMAL_SIZE, false);
                assert!(ui.app.runtime.sync_rx.is_some());
                assert_eq!(ui.app.runtime.clock.last_success(), last_success.as_ref());
                assert!(ui.app.runtime.clock.last_error().is_none());
                assert_eq!(ui.app.notifications.pending_warning_count(), 0);
            }

            // Match the production worker: send one Result, then exit. The
            // receive operation must preserve the Result's inner error.
            sender.send(Err(error.to_owned().into())).unwrap();
            drop(sender);
            visible_frame(&mut ui, NORMAL_SIZE, false);
            let runtime = &ui.app.runtime;
            assert!(runtime.sync_rx.is_none());
            assert_eq!(runtime.clock.is_synced(), previously_synced);
            assert_eq!(runtime.clock.last_success(), last_success.as_ref());
            assert_eq!(runtime.clock.last_error(), Some(error));
            let first_paint = Instant::now();
            let message = format!("校時失敗：{error}");
            assert_eq!(ui.app.notifications.pending_warning_count(), 1);
            assert_eq!(
                ui.app.notifications.visible(first_paint),
                Some(message.as_str())
            );
            assert!(
                ui.app
                    .notifications
                    .visible(first_paint + Duration::from_secs(5))
                    .is_none()
            );
            for _ in 0..3 {
                visible_frame(&mut ui, NORMAL_SIZE, false);
                assert!(ui.app.notifications.visible(Instant::now()).is_none());
            }
            let runtime = &ui.app.runtime;
            assert!(!runtime.app.saving && !runtime.app.cleaning && !runtime.app.has_pending());
            assert!(runtime.app.error.is_none());
            assert_eq!(runtime.app.current.as_ref(), Some(&original));
            assert_eq!(ui.app.model.snapshot(), original);
            assert_eq!(gate.commits.load(Ordering::Relaxed), 0);
        }
    }
}

#[test]
fn time_sync_success_is_saved_and_failed_rounds_keep_the_last_saved_calibration() {
    for manual in [false, true] {
        for hidden in [None, Some(true), Some(false)] {
            let mut ui = ready_ui_with_authenticator();
            let original = ui.app.runtime.app.current.clone().unwrap();
            let gate = gated_repository(&mut ui);
            gate.release();
            let mut expected_record = None;
            let mut successful_rounds = 0;
            for succeeds in [true, false, true] {
                ui.app.notifications = Default::default();
                let result = if succeeds {
                    successful_rounds += 1;
                    let at = Instant::now();
                    let wall = bnauth_core::now_ms();
                    let sample = bnauth_core::clock::validate_sample(
                        wall + successful_rounds as i64 * 100,
                        wall,
                        wall,
                        at,
                        at,
                    )
                    .unwrap();
                    expected_record = Some(sample.record.clone());
                    Ok(sample)
                } else {
                    Err("人工校時失敗".into())
                };
                queue_sync_result(&mut ui, result);
                ui.app.runtime.sync_manual = manual;
                match hidden {
                    Some(minimized) => {
                        hidden_frame(&mut ui, minimized, false);
                    }
                    None => {
                        visible_frame(&mut ui, NORMAL_SIZE, false);
                    }
                }
                if !manual {
                    assert!(ui.app.notifications.visible(Instant::now()).is_none());
                    assert_eq!(ui.app.notifications.pending_warning_count(), 0);
                }
                wait_hidden(&mut ui, true);
                let runtime = &ui.app.runtime;
                assert!(runtime.sync_rx.is_none() && runtime.clock.is_synced());
                assert!(!runtime.app.saving && !runtime.app.has_pending());
                let current = runtime.app.current.as_ref().unwrap();
                assert_eq!(current.service_time_sync.battlenet, expected_record);
                assert_eq!(current.authenticators, original.authenticators);
                assert_eq!(current.groups, original.groups);
                assert_eq!(current.revision, original.revision + successful_rounds);
                assert_eq!(
                    gate.commits.load(Ordering::Relaxed),
                    successful_rounds as usize
                );
                if manual {
                    assert_eq!(
                        ui.app.notifications.visible(Instant::now()),
                        Some(if succeeds {
                            "校時成功並已保存"
                        } else {
                            "校時失敗：人工校時失敗"
                        })
                    );
                } else {
                    assert!(ui.app.notifications.visible(Instant::now()).is_none());
                    assert_eq!(ui.app.notifications.pending_warning_count(), 0);
                }
            }
        }
    }
}

#[test]
fn a_successful_sync_received_during_close_is_saved_before_close_but_failure_does_not_write() {
    for hidden in [None, Some(true), Some(false)] {
        for succeeds in [true, false] {
            let mut ui = ready_ui_with_authenticator();
            let original = ui.app.runtime.app.current.clone().unwrap();
            let gate = gated_repository(&mut ui);
            let sample = fresh_sync_sample();
            let expected = sample.record.clone();
            queue_sync_result(
                &mut ui,
                if succeeds {
                    Ok(sample)
                } else {
                    Err("人工關閉時校時失敗".into())
                },
            );
            let commands = match hidden {
                Some(minimized) => hidden_frame(&mut ui, minimized, true),
                None => visible_frame(&mut ui, NORMAL_SIZE, true),
            };
            assert_eq!(
                commands.contains(&egui::ViewportCommand::CancelClose),
                succeeds
            );
            assert_eq!(ui.app.runtime.close_blocked(), succeeds);
            gate.release();
            wait_hidden(&mut ui, true);
            let runtime = &ui.app.runtime;
            assert!(runtime.close_requested && !runtime.close_blocked());
            assert_eq!(runtime.clock.is_synced(), succeeds);
            let current = runtime.app.current.as_ref().unwrap();
            assert_eq!(current.revision, original.revision + u64::from(succeeds));
            assert_eq!(
                current.service_time_sync.battlenet.as_ref(),
                succeeds.then_some(&expected)
            );
            assert_eq!(current.authenticators, original.authenticators);
            assert_eq!(gate.commits.load(Ordering::Relaxed), usize::from(succeeds));
        }
    }
}

#[test]
fn time_sync_during_a_data_save_waits_for_its_ack_and_preserves_both_changes() {
    for fail_commit in [false, true] {
        let mut ui = ready_ui_with_authenticator();
        let original = ui.app.runtime.app.current.clone().unwrap();
        let gate = gated_repository_mode(&mut ui, fail_commit);
        let (sender, receiver) = std::sync::mpsc::channel();
        ui.app.runtime.sync_rx = Some(receiver);
        ui.app.runtime.sync_manual = true;
        let before = ui.app.model.change_token();
        ui.app.model.add_group("與校時同時保存的資料").unwrap();
        ui.app.persist_frame_changes(before, &ui.ctx);
        assert!(ui.app.runtime.app.saving);
        let sample = fresh_sync_sample();
        let expected = sample.record.clone();
        sender.send(Ok(sample)).unwrap();
        drop(sender);
        visible_frame(&mut ui, NORMAL_SIZE, false);
        assert!(ui.app.runtime.clock.is_synced());
        assert_eq!(
            ui.app.runtime.pending_clock_record.as_ref(),
            Some(&expected)
        );
        assert!(ui.app.runtime.clock_save_inflight.is_none());
        gate.release();
        wait_hidden(&mut ui, true);
        let runtime = &ui.app.runtime;
        assert_eq!(runtime.app.has_pending(), fail_commit);
        let current = runtime.app.current.as_ref().unwrap();
        if fail_commit {
            assert_eq!(current, &original);
            assert_eq!(runtime.pending_clock_record.as_ref(), Some(&expected));
            assert_eq!(runtime.app.error.as_deref(), Some("人工延後保存失敗"));
        } else {
            assert_eq!(current.revision, original.revision + 2);
            assert_eq!(current.groups.len(), original.groups.len() + 1);
            assert_eq!(
                current.service_time_sync.battlenet.as_ref(),
                Some(&expected)
            );
            assert_eq!(current.authenticators, original.authenticators);
            assert!(
                runtime.pending_clock_record.is_none() && runtime.clock_save_inflight.is_none()
            );
        }
        assert_eq!(
            gate.commits.load(Ordering::Relaxed),
            if fail_commit { 1 } else { 2 }
        );
    }
}

#[test]
fn new_secret_import_reuses_an_active_sync_and_saves_its_result_after_the_import() {
    let mut ui = ready_ui_with_authenticator();
    let original = ui.app.runtime.app.current.clone().unwrap();
    let gate = gated_repository(&mut ui);
    let material = bnauth_core::parse_manual(
        "3132333435363738393031323334353637383930",
        "US-1234-5678-9012",
        "AB12CD34EF",
    )
    .unwrap();
    let preview = bnauth_core::prepare_import(
        &original,
        "人工匯入驗證器",
        "",
        original.ungrouped_id().unwrap(),
        material,
        bnauth_core::now_ms(),
    )
    .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    ui.app.runtime.sync_rx = Some(receiver);
    ui.app.runtime.sync_manual = true;
    ui.app.runtime.app.commit(preview.candidate, true).unwrap();
    gate.release();
    wait_hidden(&mut ui, true);
    let imported = ui.app.runtime.app.current.clone().unwrap();
    assert_eq!(
        imported.authenticators.len(),
        original.authenticators.len() + 1
    );
    assert_eq!(imported.revision, original.revision + 1);
    assert!(ui.app.runtime.sync_rx.is_some());
    let sample = fresh_sync_sample();
    let expected = sample.record.clone();
    sender.send(Ok(sample)).unwrap();
    drop(sender);
    wait_hidden(&mut ui, true);
    let runtime = &ui.app.runtime;
    assert!(runtime.clock.is_synced() && runtime.sync_rx.is_none());
    assert!(!runtime.app.saving && !runtime.app.has_pending());
    let current = runtime.app.current.as_ref().unwrap();
    assert_eq!(current.authenticators, imported.authenticators);
    assert_eq!(current.groups, imported.groups);
    assert_eq!(
        current.service_time_sync.battlenet.as_ref(),
        Some(&expected)
    );
    assert_eq!(current.revision, imported.revision + 1);
    assert_eq!(gate.commits.load(Ordering::Relaxed), 2);
}

#[test]
fn hidden_save_completion_finishes_close_after_data_and_calibration_are_saved() {
    let mut ui = ready_ui_with_authenticator();
    let gate = gated_repository(&mut ui);
    let (sender, receiver) = std::sync::mpsc::channel();
    ui.app.runtime.sync_rx = Some(receiver);
    ui.app.runtime.sync_manual = true;
    let candidate = candidate_with_new_group(&ui);
    ui.app.runtime.app.commit(candidate, false).unwrap();
    visible_frame_at(
        &mut ui,
        Vec2::new(460.0, 932.0),
        Pos2::new(150.0, 90.0),
        false,
    );
    let sample = fresh_sync_sample();
    let history = sample.record.clone();
    sender.send(Ok(sample)).unwrap();
    drop(sender);
    let commands = hidden_frame(&mut ui, true, true);
    assert!(commands.contains(&egui::ViewportCommand::CancelClose));
    assert!(!commands.contains(&egui::ViewportCommand::Close));
    gate.release();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let commands = hidden_frame(&mut ui, true, false);
        let runtime = &ui.app.runtime;
        let current = runtime.app.current.as_ref().unwrap();
        if commands.contains(&egui::ViewportCommand::Close) {
            assert_eq!(
                current.revision, 2,
                "both the data edit and successful calibration must be acknowledged"
            );
            assert!(!runtime.app.saving && !runtime.app.has_pending());
            assert_eq!(current.groups.len(), 2);
            assert_eq!(current.service_time_sync.battlenet.as_ref(), Some(&history));
            assert_eq!(runtime.clock.last_success(), Some(&history));
            assert!(runtime.clock.is_synced());
            break;
        }
        assert!(Instant::now() < deadline, "hidden close did not complete");
        std::thread::yield_now();
    }
    assert_eq!(gate.commits.load(Ordering::Relaxed), 2);
}

#[test]
fn clock_warning_is_shown_on_busy_pages_and_not_replaced_by_hidden_commit_ack() {
    for paint_before_ack in [true, false] {
        let mut ui = ready_ui();
        let gate = gated_repository(&mut ui);
        let candidate = candidate_with_new_group(&ui);
        ui.app.runtime.app.commit(candidate, false).unwrap();
        ui.app.warn(CLOCK_WARNING);
        for _ in 0..3 {
            if paint_before_ack {
                visible_frame(&mut ui, NORMAL_SIZE, false);
            } else {
                hidden_frame(&mut ui, true, false);
            }
        }
        if paint_before_ack {
            assert!(!ui.points(CLOCK_WARNING).is_empty());
        }
        gate.release();
        wait_hidden(&mut ui, true);
        assert_eq!(ui.app.runtime.app.current.as_ref().unwrap().revision, 1);
        for _ in 0..3 {
            visible_frame(&mut ui, NORMAL_SIZE, false);
        }
        assert!(
            !ui.points(CLOCK_WARNING).is_empty(),
            "the ordinary saved message must not replace the warning"
        );
    }
}

#[test]
fn clock_warning_is_also_painted_on_startup_error_page() {
    let mut ui = ready_ui();
    ui.app.runtime.app =
        bnauth_app::Application::start(Box::new(|| Err("人工資料讀取失敗".into())));
    wait_hidden(&mut ui, true);
    assert_eq!(ui.app.runtime.app.phase, bnauth_app::Phase::Error);
    ui.app.warn(CLOCK_WARNING);
    for _ in 0..3 {
        visible_frame(&mut ui, NORMAL_SIZE, false);
    }
    assert!(!ui.points("無法開啟資料檔").is_empty());
    assert!(!ui.points(CLOCK_WARNING).is_empty());
}

#[test]
fn explicit_residue_exit_from_warning_page_does_not_add_a_commit_for_a_late_sync_result() {
    let mut ui = ready_ui_with_authenticator();
    let vault = ui.app.runtime.app.current.clone().unwrap();
    let released = Arc::new(AtomicBool::new(false));
    let commits = Arc::new(AtomicUsize::new(0));
    let repo_release = released.clone();
    let repo_commits = commits.clone();
    let gate = SaveGate { released, commits };
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        Ok((
            Box::new(CompletionNeedsAttentionRepository {
                gate: GatedRepository {
                    released: repo_release.clone(),
                    commits: repo_commits.clone(),
                    fail_commit: false,
                },
                committed_warning: true,
                cleanup_required: false,
            }),
            Some(vault.clone()),
        ))
    }));
    drain_memory_app(&mut ui.app.runtime.app);
    // Start a request before a real edit; its reply stays pending throughout
    // the save and the user's decision on the resulting cleanup-warning page.
    let (sender, receiver) = std::sync::mpsc::channel();
    ui.app.runtime.sync_rx = Some(receiver);
    ui.app.runtime.sync_manual = true;
    let before = ui.app.model.change_token();
    ui.app.model.add_group("已保存但暫存未移除").unwrap();
    ui.app.persist_frame_changes(before, &ui.ctx);
    gate.release();
    wait_hidden(&mut ui, true);
    let saved = ui.app.runtime.app.current.clone().unwrap();
    let app = &ui.app.runtime.app;
    assert!(!app.saving && !app.cleaning && !app.has_pending());
    assert!(app.warning.is_some() && app.repository_state.cleanup_required);
    assert!(!ui.app.runtime.allow_close_with_residue);

    let moved_to = Pos2::new(150.0, 90.0);
    visible_frame_at(&mut ui, NORMAL_SIZE, moved_to, false);
    ui.view = TestView::Saving;
    for _ in 0..3 {
        ui.frame(Vec::new());
    }
    ui.click(ui.point("保留檔案並關閉"));
    let runtime = &ui.app.runtime;
    assert!(runtime.allow_close_with_residue && runtime.close_requested);
    assert!(!runtime.close_blocked());

    let sample = fresh_sync_sample();
    let history = sample.record.clone();
    sender.send(Ok(sample)).unwrap();
    drop(sender);
    let commands = visible_frame_at(&mut ui, NORMAL_SIZE, moved_to, false);
    assert!(commands.contains(&egui::ViewportCommand::Close));
    assert!(!commands.contains(&egui::ViewportCommand::CancelClose));
    let runtime = &ui.app.runtime;
    assert!(runtime.clock.is_synced() && runtime.sync_rx.is_none());
    assert_eq!(runtime.clock.last_success(), Some(&history));
    assert!(!runtime.app.saving && !runtime.app.cleaning && !runtime.app.has_pending());
    assert!(runtime.app.warning.is_some() && runtime.app.repository_state.cleanup_required);
    assert_eq!(runtime.app.current.as_ref(), Some(&saved));
    assert_eq!(gate.commits.load(Ordering::Relaxed), 1);
}

#[test]
fn hidden_close_during_save_reveals_a_later_failure_without_losing_the_candidate() {
    let mut ui = ready_ui();
    let gate = gated_repository_mode(&mut ui, true);
    let candidate = candidate_with_new_group(&ui);
    ui.app.runtime.app.commit(candidate, false).unwrap();
    let commands = hidden_frame(&mut ui, true, true);
    assert!(commands.contains(&egui::ViewportCommand::CancelClose));
    assert!(!commands.contains(&egui::ViewportCommand::Close));
    assert!(!commands.contains(&egui::ViewportCommand::Minimized(false)));
    assert!(!commands.contains(&egui::ViewportCommand::Focus));
    gate.release();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // The original raw Close exists only in the earlier busy frame.
        let commands = hidden_frame(&mut ui, true, false);
        assert!(!commands.contains(&egui::ViewportCommand::Close));
        let app = &ui.app.runtime.app;
        if !app.saving {
            assert!(app.has_pending());
            assert_eq!(app.error.as_deref(), Some("人工延後保存失敗"));
            assert!(commands.contains(&egui::ViewportCommand::CancelClose));
            assert!(commands.contains(&egui::ViewportCommand::Minimized(false)));
            assert!(commands.contains(&egui::ViewportCommand::Visible(true)));
            assert!(commands.contains(&egui::ViewportCommand::Focus));
            assert_eq!(app.current.as_ref().unwrap().groups.len(), 1);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "failed save was not polled while hidden"
        );
        std::thread::yield_now();
    }
    let commands = hidden_frame(&mut ui, true, false);
    assert!(commands.contains(&egui::ViewportCommand::CancelClose));
    assert!(!commands.contains(&egui::ViewportCommand::Close));
    assert!(!commands.contains(&egui::ViewportCommand::Focus));
    assert!(ui.app.runtime.app.has_pending());
}

struct CompletionNeedsAttentionRepository {
    gate: GatedRepository,
    committed_warning: bool,
    cleanup_required: bool,
}

impl bnauth_app::Repository for CompletionNeedsAttentionRepository {
    fn commit(
        &mut self,
        candidate: &bnauth_core::VaultSnapshot,
    ) -> Result<bnauth_app::CommitOutcome, String> {
        self.cleanup_required = true;
        if self.committed_warning {
            let mut outcome = self.gate.commit(candidate)?;
            outcome.warning = Some("人工提交成功，但清理暫存失敗".into());
            Ok(outcome)
        } else {
            self.gate.commits.fetch_add(1, Ordering::Relaxed);
            Err("人工保存失敗，暫存檔需清理".into())
        }
    }

    fn retry_cleanup(&mut self) -> Result<(), String> {
        self.gate.wait_for_release()?;
        self.cleanup_required = false;
        Ok(())
    }

    fn state(&self) -> bnauth_app::RepositoryState {
        bnauth_app::RepositoryState {
            cleanup_required: self.cleanup_required,
            blocked: false,
        }
    }
}

fn assert_hidden_close_reveals_completed_work_once(committed_warning: bool) {
    for minimized in [true, false] {
        let mut ui = ready_ui();
        let vault = ui.app.runtime.app.current.clone().unwrap();
        let released = Arc::new(AtomicBool::new(false));
        let commits = Arc::new(AtomicUsize::new(0));
        let repo_release = released.clone();
        let repo_commits = commits.clone();
        let gate = SaveGate { released, commits };
        ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
            Ok((
                Box::new(CompletionNeedsAttentionRepository {
                    gate: GatedRepository {
                        released: repo_release.clone(),
                        commits: repo_commits.clone(),
                        fail_commit: false,
                    },
                    committed_warning,
                    cleanup_required: false,
                }),
                Some(vault.clone()),
            ))
        }));
        drain_memory_app(&mut ui.app.runtime.app);
        let candidate = candidate_with_new_group(&ui);
        ui.app.runtime.app.commit(candidate, false).unwrap();
        if !committed_warning {
            wait_hidden(&mut ui, minimized);
            assert!(ui.app.runtime.app.has_pending());
            ui.app.runtime.app.retry_cleanup().unwrap();
        }

        // The raw Close occurs while saving/cleaning, before its result exists.
        let commands = hidden_frame(&mut ui, minimized, true);
        assert!(commands.contains(&egui::ViewportCommand::CancelClose));
        assert!(!commands.contains(&egui::ViewportCommand::Close));
        assert!(!commands.contains(&egui::ViewportCommand::Focus));
        gate.release();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let commands = hidden_frame(&mut ui, minimized, false);
            assert!(!commands.contains(&egui::ViewportCommand::Close));
            let app = &ui.app.runtime.app;
            if !app.saving && !app.cleaning {
                assert_eq!(app.has_pending(), !committed_warning);
                assert_eq!(app.warning.is_some(), committed_warning);
                assert_eq!(app.repository_state.cleanup_required, committed_warning);
                assert_eq!(
                    app.current.as_ref().unwrap().revision,
                    u64::from(committed_warning)
                );
                assert_eq!(
                    app.current.as_ref().unwrap().groups.len(),
                    1 + usize::from(committed_warning)
                );
                assert!(commands.contains(&egui::ViewportCommand::CancelClose));
                assert!(commands.contains(&egui::ViewportCommand::Minimized(false)));
                assert!(commands.contains(&egui::ViewportCommand::Visible(true)));
                assert!(commands.contains(&egui::ViewportCommand::Focus));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "background completion was not polled"
            );
            std::thread::yield_now();
        }
        for _ in 0..3 {
            let commands = hidden_frame(&mut ui, minimized, false);
            assert!(commands.contains(&egui::ViewportCommand::CancelClose));
            assert!(!commands.contains(&egui::ViewportCommand::Close));
            assert!(!commands.contains(&egui::ViewportCommand::Minimized(false)));
            assert!(!commands.contains(&egui::ViewportCommand::Visible(true)));
            assert!(!commands.contains(&egui::ViewportCommand::Focus));
        }
        assert_eq!(
            gate.commits.load(Ordering::Relaxed),
            1,
            "cleanup must not resubmit the candidate"
        );
    }
}

#[test]
fn hidden_close_reveals_committed_cleanup_warning_once() {
    assert_hidden_close_reveals_completed_work_once(true);
}

#[test]
fn hidden_close_reveals_successful_cleanup_with_pending_candidate_once() {
    assert_hidden_close_reveals_completed_work_once(false);
}

#[test]
fn window_movement_resizing_zoom_and_close_never_commit() {
    let mut ui = ready_ui_with_authenticator();
    let gate = gated_repository(&mut ui);
    gate.release();
    let original = ui.app.runtime.app.current.clone().unwrap();
    for (size, origin, native_dpi, zoom) in [
        (Vec2::new(400.0, 892.0), Pos2::new(150.0, 90.0), 1.0, 1.0),
        (
            Vec2::new(2160.0, 3840.0),
            Pos2::new(-1800.0, 60.0),
            1.0,
            1.0,
        ),
        (Vec2::new(1600.0, 1808.0), Pos2::new(600.0, 120.0), 2.0, 1.5),
        (Vec2::new(320.0, 452.0), Pos2::new(80.0, 40.0), 1.0, 1.0),
    ] {
        ui.ctx.set_zoom_factor(zoom);
        for _ in 0..4 {
            physical_frame(&mut ui, size, origin, native_dpi, false);
        }
        assert!(!ui.app.runtime.app.saving && !ui.app.runtime.app.has_pending());
        assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
        assert_eq!(ui.app.model.snapshot(), original);
        assert_eq!(gate.commits.load(Ordering::Relaxed), 0);
    }
    let commands = hidden_frame(&mut ui, true, true);
    assert!(!commands.contains(&egui::ViewportCommand::CancelClose));
    assert!(!ui.app.runtime.close_blocked());
    assert_eq!(gate.commits.load(Ordering::Relaxed), 0);
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
}

#[test]
fn moving_then_editing_saves_once_without_a_followup_window_commit() {
    let mut ui = ready_ui_with_authenticator();
    let gate = gated_repository(&mut ui);
    gate.release();
    let original = ui.app.runtime.app.current.clone().unwrap();
    let id = ui.app.model.entries[0].id;
    ui.app.edit(&ui.ctx, Some(id));
    for _ in 0..4 {
        interactive_frame(&mut ui, Vec::new());
    }
    let text = *ui.points(&original.authenticators[0].name).last().unwrap();
    click_visible(&mut ui, text);
    let ctrl = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    interactive_frame(
        &mut ui,
        vec![
            egui::Event::ModifiersChanged(ctrl),
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers: ctrl,
            },
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: false,
                repeat: false,
                modifiers: ctrl,
            },
            egui::Event::Paste("移動後立即套用".into()),
            egui::Event::ModifiersChanged(egui::Modifiers::NONE),
        ],
    );
    let moved_to = Pos2::new(150.0, 90.0);
    interactive_frame_at(&mut ui, moved_to, Vec::new());
    let apply = ui.point("套用");
    for pressed in [true, false] {
        interactive_frame_at(
            &mut ui,
            moved_to,
            vec![
                egui::Event::PointerMoved(apply),
                egui::Event::PointerButton {
                    pos: apply,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert!(ui.app.dialog.is_none());
    let deadline = Instant::now() + Duration::from_secs(5);
    while ui.app.runtime.app.saving {
        assert!(Instant::now() < deadline);
        visible_frame_at(&mut ui, NORMAL_SIZE, moved_to, false);
        std::thread::yield_now();
    }
    let saved = ui.app.runtime.app.current.clone().unwrap();
    let mut expected = original;
    expected.revision += 1;
    expected.authenticators[0].name = "移動後立即套用".into();
    expected.authenticators[0].updated_at_ms = saved.authenticators[0].updated_at_ms;
    assert_eq!(saved, expected);
    // Follow-up frames and closing must not submit another data commit.
    for _ in 0..4 {
        visible_frame_at(&mut ui, NORMAL_SIZE, moved_to, false);
    }
    assert_eq!(gate.commits.load(Ordering::Relaxed), 1);
    let commands = visible_frame_at(&mut ui, NORMAL_SIZE, moved_to, true);
    assert!(!commands.contains(&egui::ViewportCommand::CancelClose));
    assert_eq!(ui.app.model.snapshot(), expected);
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&expected));
    assert_eq!(gate.commits.load(Ordering::Relaxed), 1);
}

#[test]
fn loading_hidden_and_loaded_frames_do_not_reposition_or_write() {
    let mut ui = ready_ui();
    let original = ui.app.runtime.app.current.clone().unwrap();
    let vault = original.clone();
    let released = Arc::new(AtomicBool::new(false));
    let commits = Arc::new(AtomicUsize::new(0));
    let factory_release = released.clone();
    let factory_commits = commits.clone();
    let gate = SaveGate { released, commits };
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !factory_release.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "load gate did not open");
            std::thread::yield_now();
        }
        Ok((
            Box::new(GatedRepository {
                released: Arc::new(AtomicBool::new(true)),
                commits: factory_commits.clone(),
                fail_commit: false,
            }),
            Some(vault.clone()),
        ))
    }));
    let mut commands = Vec::new();
    for _ in 0..3 {
        commands.extend(hidden_frame(&mut ui, false, false));
        assert_eq!(ui.app.runtime.app.phase, bnauth_app::Phase::Loading);
    }
    for _ in 0..3 {
        commands.extend(visible_frame_at(
            &mut ui,
            NORMAL_SIZE,
            Pos2::new(150.0, 90.0),
            false,
        ));
        assert_eq!(ui.app.runtime.app.phase, bnauth_app::Phase::Loading);
    }
    for _ in 0..3 {
        commands.extend(hidden_frame(&mut ui, false, false));
        assert_eq!(ui.app.runtime.app.phase, bnauth_app::Phase::Loading);
    }
    gate.release();
    commands.extend(wait_hidden(&mut ui, false));
    for _ in 0..3 {
        commands.extend(visible_frame_at(
            &mut ui,
            NORMAL_SIZE,
            Pos2::new(120.0, 80.0),
            false,
        ));
    }
    assert!(!commands.iter().any(|command| matches!(
        command,
        egui::ViewportCommand::OuterPosition(_) | egui::ViewportCommand::InnerSize(_)
    )));
    assert_eq!(ui.app.runtime.app.phase, bnauth_app::Phase::Ready);
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    assert_eq!(ui.app.model.snapshot(), original);
    assert!(!ui.app.runtime.app.saving && !ui.app.runtime.app.has_pending());
    assert_eq!(gate.commits.load(Ordering::Relaxed), 0);
}
