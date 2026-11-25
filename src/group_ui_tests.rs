use super::*;
use crate::test_support::drain_memory_app;

#[path = "lifecycle_ui_tests.rs"]
mod lifecycle_ui_tests;

#[path = "scroll_ui_tests.rs"]
mod scroll_ui_tests;

#[path = "delete_batch_ui_tests.rs"]
mod delete_batch_ui_tests;

#[path = "editor_state_tests.rs"]
mod editor_state_tests;

#[path = "input_limit_tests.rs"]
mod input_limit_tests;

#[path = "material_input_tests.rs"]
mod material_input_tests;

#[path = "email_recovery_tests.rs"]
mod email_recovery_tests;

#[path = "xml_export_ui_tests.rs"]
mod xml_export_ui_tests;

#[path = "group_import_ui_tests.rs"]
mod group_import_ui_tests;

#[path = "full_restore_ui_tests.rs"]
mod full_restore_ui_tests;

#[path = "shared_clock_ui_tests.rs"]
mod shared_clock_ui_tests;

struct GroupUi {
    app: BnAuthApp,
    ctx: egui::Context,
    time: f64,
    size: Vec2,
    shapes: Vec<egui::epaint::ClippedShape>,
    passes: usize,
    view: TestView,
}

enum TestView {
    Dialog,
    DeleteList,
    List,
    Saving,
}

const IMPORT_TEST_TIME: i64 = 1_800_000_000_000;

fn import_draft() -> crate::runtime::ImportForm {
    let mut form = crate::runtime::ImportForm::new(0);
    form.name = "人工匯入草稿".into();
    form.description = "外點及Escape後仍須保留".into();
    form.secret = "3132333435363738393031323334353637383930".into();
    form.serial = "US-1234-5678-9012".into();
    form.restore = "AB12CD34EF".into();
    form
}

fn large_list_vault() -> bnauth_core::VaultSnapshot {
    let mut vault = bnauth_core::VaultSnapshot::new_empty();
    for group in 1..10 {
        vault.groups.push(bnauth_core::Group {
            id: uuid::Uuid::new_v4(),
            name: format!("Group {group}"),
            sort_order: group,
            is_ungrouped: false,
        });
    }
    let material =
        bnauth_core::parse_manual("3132333435363738393031323334353637383930", "", "").unwrap();
    let mut record = bnauth_core::prepare_import(&vault, "Row", "", vault.groups[0].id, material)
        .unwrap()
        .record();
    for index in 0..1000usize {
        let mut secret = [0; 20];
        secret[..8].copy_from_slice(&(index as u64 + 1).to_le_bytes());
        record.id = uuid::Uuid::new_v4();
        record.group_id = vault.groups[index / 100].id;
        record.sort_order = (index % 100) as u32;
        record.name = format!("Row {index:04}");
        record.secret = bnauth_core::Secret::from_bytes(secret);
        vault.authenticators.push(record.clone());
    }
    vault.validate().unwrap();
    vault
}

fn wait_runtime(ui: &mut GroupUi) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        ui.app.poll_runtime(&ui.ctx);
        let app = &ui.app.runtime.app;
        if app.phase != bnauth_app::Phase::Loading && !app.saving && !app.cleaning {
            break;
        }
        assert!(Instant::now() < deadline, "worker did not finish");
        std::thread::yield_now();
    }
}

#[test]
fn large_search_renders_visible_rows_and_reuses_codes_until_scrolling_or_time_step_changes() {
    let mut ui = GroupUi::new();
    ui.use_memory_vault(large_list_vault());
    ui.view = TestView::List;
    ui.app.dialog = None;
    ui.app.query = "row".into();
    for _ in 0..4 {
        ui.frame(Vec::new());
    }
    assert_eq!(ui.app.visible_ids().len(), 1000);
    assert!(!ui.points("Row 0000").is_empty());
    assert!(ui.points("Row 0999").is_empty());
    let calculations = ui.app.otp_cache.calculations;
    assert!(
        calculations > 0 && calculations < 20,
        "calculated {calculations} rows"
    );
    for _ in 0..10 {
        ui.frame(Vec::new());
    }
    assert_eq!(ui.app.otp_cache.calculations, calculations);
    ui.frame(vec![
        egui::Event::PointerMoved(Pos2::new(100.0, 200.0)),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, -100_000.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    for _ in 0..30 {
        ui.frame(Vec::new());
    }
    assert!(ui.points("Row 0000").is_empty());
    assert!(!ui.points("Row 0999").is_empty());
    let scrolled = ui.app.otp_cache.calculations;
    assert!(
        scrolled > calculations && scrolled < 100,
        "calculated {scrolled} rows while scrolling"
    );
    ui.app.runtime.frame_time += 30_000;
    ui.frame(Vec::new());
    assert!(ui.app.otp_cache.calculations > scrolled);
    ui.app.query = "row 0000".into();
    for _ in 0..4 {
        ui.frame(Vec::new());
    }
    let matching = ui.app.visible_ids();
    assert_eq!(matching.len(), 1);
    assert!(!ui.points("Row 0000").is_empty());
    ui.app
        .model
        .update_entry(matching[0], "Changed name", "")
        .unwrap();
    for _ in 0..4 {
        ui.frame(Vec::new());
    }
    assert!(ui.app.visible_ids().is_empty());
    assert!(!ui.points("沒有符合的驗證器").is_empty());
    ui.app.query.clear();
    ui.app.group = 0;
    ui.frame(Vec::new());
    assert_eq!(ui.app.visible_ids().len(), 100);
}

#[test]
fn delete_confirmation_only_renders_visible_rows_and_reflows_wrapped_names() {
    let mut ui = GroupUi::new();
    let mut vault = large_list_vault();
    vault.authenticators[500].name = format!("{} 0500", "完整長名稱".repeat(60));
    vault.authenticators[999].name = format!("{} 0999", "最後的完整名稱".repeat(8));
    vault.validate().unwrap();
    ui.use_memory_vault(vault.clone());
    let ids = ui.app.model.entries.iter().map(|e| e.id).collect();
    ui.open(Dialog::delete(ids));
    let initial_layouts = match ui.app.dialog.as_ref().unwrap() {
        Dialog::Delete(form) => {
            assert!(form.rendered_rows > 0 && form.rendered_rows < 20);
            assert_eq!(form.preparations, 1);
            form.layouts
        }
        _ => unreachable!(),
    };
    assert!(!ui.points("Row 0000 · 預設").is_empty());
    assert!(ui.points("Row 0998 · Group 9").is_empty());
    for _ in 0..10 {
        ui.frame(Vec::new());
    }
    if let Some(Dialog::Delete(form)) = &ui.app.dialog {
        assert_eq!(form.preparations, 1);
        assert_eq!(form.layouts, initial_layouts);
        assert!(form.rendered_rows < 20);
    }

    // Resizing and zooming must recompute wrapped heights, without rebuilding data.
    for (size, zoom) in [
        (Vec2::new(320.0, 600.0), 1.0),
        (Vec2::new(400.0, 892.0), 1.25),
    ] {
        ui.size = size;
        ui.ctx.set_zoom_factor(zoom);
        for _ in 0..5 {
            ui.frame(Vec::new());
        }
        let form = match ui.app.dialog.as_ref().unwrap() {
            Dialog::Delete(form) => form,
            _ => unreachable!(),
        };
        assert!(form.layouts > initial_layouts);
        assert_eq!(form.preparations, 1);
        assert!(form.rendered_rows < 20);
        let pointer = ui.dialog_content_rect().min + Vec2::new(60.0, 90.0);
        ui.frame(vec![
            egui::Event::PointerMoved(pointer),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, -100_000.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        for _ in 0..30 {
            ui.frame(Vec::new());
        }
        let last = format!("{} · Group 9", vault.authenticators[999].name);
        assert!(
            !ui.points(&last).is_empty(),
            "last wrapped row must remain reachable"
        );
    }
    assert_eq!(ui.app.model.snapshot(), vault);
}

#[test]
fn delete_confirmation_tail_is_correct_on_the_first_scaled_frame() {
    for component_only in [false, true] {
        for count in [10, 20, 50, 100, 1000] {
            for native_dpi in [false, true] {
                let mut ui = GroupUi::new();
                let mut vault = large_list_vault();
                vault.authenticators.truncate(count);
                for (index, entry) in vault.authenticators.iter_mut().enumerate() {
                    entry.name = format!("Account {index:04} {}", "wrapped name ".repeat(4));
                }
                vault.validate().unwrap();
                ui.use_memory_vault(vault.clone());
                if component_only {
                    ui.view = TestView::DeleteList;
                }
                ui.size = Vec2::new(1000.0, 892.0);
                let last = ui.app.model.entries.last().unwrap();
                let label = format!(
                    "{} · {}",
                    last.record.name,
                    ui.app.model.group_name(last.group_id).unwrap()
                );
                let ids = ui.app.model.entries.iter().map(|e| e.id).collect();
                ui.open(Dialog::delete(ids));
                let pointer = if component_only {
                    Pos2::new(60.0, 90.0)
                } else {
                    ui.dialog_content_rect().min + Vec2::new(60.0, 90.0)
                };
                ui.frame(vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(0.0, -100_000.0),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Wheel,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
                for _ in 0..30 {
                    ui.frame(Vec::new());
                }

                // Native DPI uses the new logical viewport for the same physical size.
                // UI zoom is tested separately because egui applies it between frames.
                let native_scale = if native_dpi { 1.25 } else { 1.0 };
                if !native_dpi {
                    ui.ctx.set_zoom_factor(1.25);
                }
                ui.frame_scaled(Vec::new(), native_scale);
                let first = ui.text_bottom_relative_to_clip(&label);
                assert!(ui.passes <= 2);
                for _ in 0..4 {
                    ui.frame_scaled(Vec::new(), native_scale);
                }
                let stable = ui.text_bottom_relative_to_clip(&label);
                assert!(
                    (first - stable).abs() < 0.75,
                    "tail jumped: component_only={component_only}, count={count}, native_dpi={native_dpi}, first={first}, stable={stable}"
                );
                assert_eq!(ui.passes, 1, "stable frames must not keep discarding");
                assert_eq!(ui.app.model.snapshot(), vault);
            }
        }
    }
}

#[test]
fn idle_frames_do_not_save_and_new_group_keeps_its_ui_identity_after_commit() {
    let mut ui = GroupUi::new();
    ui.use_memory_vault(bnauth_core::VaultSnapshot::new_empty());
    for _ in 0..20 {
        ui.frame(Vec::new());
        assert!(!ui.app.runtime.app.saving);
    }
    assert_eq!(ui.app.runtime.app.current.as_deref().unwrap().revision, 0);
    let id = ui.submit_group("保存後保持分組");
    wait_runtime(&mut ui);
    assert_eq!(ui.app.model.group_name(id), Some("保存後保持分組"));
    assert_eq!(ui.app.runtime.app.current.as_deref().unwrap().revision, 1);
    ui.frame(Vec::new());
    assert!(!ui.app.runtime.app.saving);
}

struct CleanupFailureRepository {
    cleanup_required: bool,
}

impl bnauth_app::Repository for CleanupFailureRepository {
    fn state(&self) -> bnauth_app::RepositoryState {
        bnauth_app::RepositoryState {
            cleanup_required: self.cleanup_required,
            ..Default::default()
        }
    }
    fn commit(
        &mut self,
        _: &bnauth_core::VaultSnapshot,
    ) -> Result<bnauth_app::CommitOutcome, String> {
        self.cleanup_required = true;
        Err("人工保存與清理失敗".into())
    }
    fn retry_cleanup(&mut self) -> Result<(), String> {
        panic!("discard must not remove the temporary file")
    }
    fn defer_cleanup(&mut self) {
        self.cleanup_required = false;
    }
}

#[test]
fn discarding_failed_group_changes_keeps_original_rows_and_allows_close() {
    let mut ui = GroupUi::new();
    let mut model = VaultModel::from_snapshot(&bnauth_core::VaultSnapshot::new_empty());
    model.add_test_entry("既有驗證器", "", 0).unwrap();
    let vault = model.snapshot();
    ui.use_memory_vault(vault.clone());
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        Ok((
            Box::new(CleanupFailureRepository {
                cleanup_required: false,
            }),
            Some(vault.clone()),
        ))
    }));
    wait_runtime(&mut ui);
    let code = ui
        .app
        .otp_cache
        .code(&ui.app.model.entries[0].record, IMPORT_TEST_TIME);
    let calculations = ui.app.otp_cache.calculations;
    let token = ui.app.model.change_token();
    ui.open(Dialog::groups(&ui.app.model));
    ui.click(ui.point("輸入名稱後按 Enter 新增"));
    ui.type_text("不能保存的分組");
    ui.click(ui.point("套用"));
    wait_runtime(&mut ui);
    assert!(ui.app.runtime.app.has_pending());
    assert_eq!(ui.app.model.change_token(), token);
    ui.view = TestView::Saving;
    for _ in 0..3 {
        ui.frame(Vec::new());
    }
    assert!(!ui.points(bnauth_core::WRITE_FAILED_NOTICE).is_empty());
    ui.click(ui.point("放棄"));
    assert_eq!(ui.app.model.groups.len(), 1);
    assert_eq!(ui.app.model.change_token(), token);
    assert_eq!(
        ui.app
            .otp_cache
            .code(&ui.app.model.entries[0].record, IMPORT_TEST_TIME),
        code
    );
    assert_eq!(ui.app.otp_cache.calculations, calculations);
    let app = &ui.app.runtime.app;
    assert_eq!(&ui.app.model.snapshot(), app.current.as_deref().unwrap());
    assert!(!app.has_pending());
    assert!(!app.repository_state.needs_attention());
    assert!(app.error.is_none());
    let mut input = egui::RawInput::default();
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .events
        .push(egui::ViewportEvent::Close);
    let mut frame = eframe::Frame::_new_kittest();
    let output = ui.ctx.run_ui(input, |root| {
        eframe::App::logic(&mut ui.app, root.ctx(), &mut frame);
    });
    assert!(
        !output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .contains(&egui::ViewportCommand::CancelClose)
    );
    assert!(ui.app.runtime.close_requested);
}

impl GroupUi {
    fn submit_group(&mut self, name: &str) -> u64 {
        let mut draft = self.app.model.group_draft();
        let id = draft.add_group(name).unwrap();
        let runtime = &mut self.app.runtime;
        let candidate = draft.snapshot(runtime.app.current.as_deref().unwrap());
        runtime.commit_with_pending_clock(candidate, None).unwrap();
        runtime.pending_groups = Some(Box::new(draft));
        id
    }

    fn new() -> Self {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::empty();
        fonts.font_data.insert(
            "Nowar".into(),
            egui::FontData::from_static(include_bytes!("../assets/fonts/NowarSansTW-Regular.ttf"))
                .into(),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.insert(family, vec!["Nowar".into()]);
        }
        ctx.set_fonts(fonts);
        let mut style = egui::Style {
            animation_time: 0.0,
            ..Default::default()
        };
        style.spacing.button_padding = Vec2::new(10.0, 7.0);
        style.spacing.item_spacing = Vec2::new(6.0, 6.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(13.0));
        ctx.set_global_style(style);
        let mut model = VaultModel::test_fixture();
        let runtime = crate::test_support::runtime(model.snapshot(), IMPORT_TEST_TIME);
        let mut groups = Dialog::groups(&model);
        if let Dialog::Groups { name, .. } = &mut groups {
            *name = "新增草稿".into();
        }
        let mut ui = Self {
            app: BnAuthApp {
                window_chrome: crate::window_chrome::WindowChrome::default(),
                otp_cache: crate::otp_cache::OtpCache::default(),
                projection: EntryProjection::default(),
                model,
                group: 0,
                query: String::new(),
                selected: RowSelection::default(),
                dialog: Some(groups),
                notifications: crate::notifications::Notifications::default(),
                runtime,
                drag_origin: None,
                dragging: None,
                drop: None,
                tab_reveal: Some(0),
                tab_scroll: None,
                tab_labels: TabLabels::default(),
            },
            ctx,
            time: 0.0,
            size: Vec2::new(400.0, 860.0),
            shapes: Vec::new(),
            passes: 0,
            view: TestView::Dialog,
        };
        for _ in 0..4 {
            ui.frame(Vec::new());
        }
        ui
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        self.frame_scaled(events, 1.0);
    }

    fn frame_scaled(
        &mut self,
        events: Vec<egui::Event>,
        native_scale: f32,
    ) -> egui::PlatformOutput {
        self.time += 0.04;
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size / native_scale)),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(native_scale);
        let ctx = self.ctx.clone();
        let mut output = ctx.run_ui(input, |ui| match self.view {
            TestView::Dialog => self.app.show_dialog(ui.ctx()),
            TestView::DeleteList => {
                // Hold the parent layout fixed so its own discard cannot hide
                // a first-frame error in the scrollable component.
                ui.set_width(370.0);
                let Some(Dialog::Delete(form)) = &mut self.app.dialog else {
                    unreachable!()
                };
                form.show(ui, &self.app.model);
            }
            TestView::List => {
                self.app.entry_list(ui, false);
            }
            TestView::Saving => self.app.saving_ui(ui),
        });
        // This harness inspects paint shapes without a GPU texture consumer.
        output.textures_delta.clear();
        self.passes = output.platform_output.num_completed_passes;
        self.shapes = output.shapes;
        output.platform_output
    }

    fn open(&mut self, dialog: Dialog) {
        self.app.open_dialog(&self.ctx, dialog);
        for _ in 0..4 {
            self.frame(Vec::new());
        }
    }

    fn use_memory_vault(&mut self, vault: bnauth_core::VaultSnapshot) {
        self.app.model = VaultModel::from_snapshot(&vault);
        // Fixture replacement starts a fresh model whose token may match the
        // previous one; unlike a production reload it does not apply_snapshot.
        self.app.projection = EntryProjection::default();
        self.app.tab_labels = TabLabels::default();
        self.app.runtime = crate::test_support::runtime(vault, IMPORT_TEST_TIME);
    }

    fn import(&self) -> &crate::runtime::ImportForm {
        match self.app.dialog.as_ref() {
            Some(Dialog::Import(form)) => form,
            _ => panic!("Expected import input or preview"),
        }
    }

    fn points(&self, label: &str) -> Vec<Pos2> {
        fn visit(shape: &egui::Shape, clip: Rect, label: &str, out: &mut Vec<Pos2>) {
            match shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    let center = text.pos + text.galley.rect.center().to_vec2();
                    if clip.contains(center) {
                        out.push(center);
                    }
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        visit(shape, clip, label, out);
                    }
                }
                _ => {}
            }
        }
        let mut points = Vec::new();
        for shape in &self.shapes {
            visit(&shape.shape, shape.clip_rect, label, &mut points);
        }
        points
    }

    fn point(&self, label: &str) -> Pos2 {
        *self
            .points(label)
            .first()
            .unwrap_or_else(|| panic!("No visible text: {label}"))
    }

    fn text_bottom_relative_to_clip(&self, label: &str) -> f32 {
        fn find(shape: &egui::Shape, clip: Rect, label: &str) -> Option<f32> {
            match shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    clip.intersects(rect)
                        .then_some(rect.bottom() - clip.bottom())
                }
                egui::Shape::Vec(shapes) => {
                    shapes.iter().find_map(|shape| find(shape, clip, label))
                }
                _ => None,
            }
        }
        self.shapes
            .iter()
            .find_map(|shape| find(&shape.shape, shape.clip_rect, label))
            .unwrap_or_else(|| panic!("Expected a painted, visible label: {label}"))
    }

    fn dialog_content_rect(&self) -> Rect {
        let dialog = self.app.dialog.as_ref().expect("Expected an open dialog");
        let id = egui::Id::unique(("dialog", std::mem::discriminant(dialog)));
        let outer = self
            .ctx
            .memory(|memory| memory.area_rect(id))
            .expect("Expected the modal's actual area bounds");
        // Modal uses Frame::popup by default. Its area includes the frame's
        // margin/stroke; the header must align with the inner content bounds.
        outer - egui::Frame::popup(&self.ctx.global_style()).total_margin()
    }

    fn group_point(&self, id: u64) -> Pos2 {
        if matches!(&self.app.dialog, Some(Dialog::Groups { rename: Some(edit), .. }) if edit.id == id)
        {
            return self
                .ctx
                .read_response(DialogEditor::GroupRename.id())
                .unwrap()
                .rect
                .center();
        }
        self.point(&format!(
            "{}  ({})",
            self.group_draft().group_name(id).unwrap(),
            self.group_draft().group_count(id)
        ))
    }

    fn action_point(&self, id: u64, label: &str) -> Pos2 {
        let row_y = self.group_point(id).y;
        self.points(label)
            .into_iter()
            .min_by(|a, b| (a.y - row_y).abs().total_cmp(&(b.y - row_y).abs()))
            .filter(|p| (p.y - row_y).abs() < 10.0)
            .unwrap_or_else(|| panic!("No {label} button for group {id}"))
    }

    fn click(&mut self, point: Pos2) {
        for pressed in [true, false] {
            self.frame(vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        self.frame(Vec::new());
    }

    fn edit_group(&mut self, id: u64) {
        // Separate this gesture from any earlier single/double click.
        self.time += 0.5;
        self.frame(Vec::new());
        let point = self.group_point(id);
        self.click(point);
        self.click(point);
        self.frame(Vec::new());
        assert_eq!(self.rename().id, id);
        assert!(self.ctx.memory(|memory| memory.focused()).is_some());
    }

    fn key(&mut self, key: egui::Key) {
        for pressed in [true, false] {
            self.frame(vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
        self.frame(Vec::new());
    }

    fn type_text(&mut self, text: &str) {
        self.frame(vec![egui::Event::Text(text.into())]);
    }

    fn has_invalid_border(&self, editor: DialogEditor) -> bool {
        let field = self.ctx.read_response(editor.id()).unwrap().rect;
        self.shapes.iter().any(|shape| matches!(
            &shape.shape, egui::Shape::Rect(rect) if rect.rect == field && rect.stroke.color == RED
        ))
    }

    fn rename(&self) -> &GroupRename {
        match self.app.dialog.as_ref() {
            Some(Dialog::Groups {
                rename: Some(edit), ..
            }) => edit,
            _ => panic!("Expected an active inline rename in Groups"),
        }
    }

    fn assert_idle_groups(&self) {
        assert!(matches!(
            self.app.dialog,
            Some(Dialog::Groups { rename: None, .. })
        ));
    }

    fn group_draft(&self) -> &GroupDraft {
        match &self.app.dialog {
            Some(Dialog::Groups { draft, .. }) => draft,
            _ => panic!("Expected open group management"),
        }
    }

    fn order(&self) -> Vec<u64> {
        self.group_draft()
            .groups
            .iter()
            .map(|group| group.id)
            .collect()
    }
}

#[test]
fn about_github_click_requests_native_open_without_changing_data() {
    let mut ui = GroupUi::new();
    let original = ui.app.model.snapshot();
    let token = ui.app.model.change_token();
    ui.open(Dialog::Info);
    let url = "https://github.com/EKE00372/BNAuth";
    let point = ui.point(url);
    let mut requests = Vec::new();
    for pressed in [true, false] {
        let output = ui.frame_scaled(
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            1.0,
        );
        requests.extend(
            output
                .commands
                .into_iter()
                .filter_map(|command| match command {
                    egui::OutputCommand::OpenUrl(request) => Some(request.url),
                    _ => None,
                }),
        );
    }
    assert_eq!(requests, vec![url]);
    assert!(matches!(ui.app.dialog, Some(Dialog::Info)));
    assert_eq!(ui.app.model.change_token(), token);
    assert_eq!(ui.app.model.snapshot(), original);
    assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
    assert!(!ui.app.runtime.app.saving && !ui.app.runtime.app.has_pending());
}

#[test]
fn single_click_stays_put_double_click_selects_inline_name_and_enter_stages_rename() {
    let mut ui = GroupUi::new();
    ui.click(ui.group_point(1));
    ui.assert_idle_groups();
    assert_eq!(ui.app.group, 0);
    ui.edit_group(1);
    ui.type_text("改名主帳號");
    // Text input replaces the whole original name, proving focus and select-all.
    assert_eq!(ui.rename().name, "改名主帳號");
    assert_eq!(ui.app.model.group_name(1), Some("主帳號"));
    if let Some(Dialog::Groups { name, .. }) = &ui.app.dialog {
        assert_eq!(name, "新增草稿");
    }
    ui.key(egui::Key::Enter);
    ui.assert_idle_groups();
    assert_eq!(ui.group_draft().group_name(1), Some("改名主帳號"));
    assert_eq!(ui.app.model.group_name(1), Some("主帳號"));
    assert_eq!(ui.app.group, 0);
    assert_eq!(ui.app.model.groups.len(), 5);
}

#[test]
fn escape_discards_inline_draft_without_closing_management() {
    let mut ui = GroupUi::new();
    ui.edit_group(3);
    ui.type_text("不應保存");
    ui.key(egui::Key::Escape);
    ui.assert_idle_groups();
    assert_eq!(ui.app.model.group_name(3), Some("備用"));
    ui.key(egui::Key::Escape);
    ui.assert_idle_groups();
}

#[test]
fn backdrop_keeps_invalid_inline_draft_and_sorting_still_validates_it() {
    let mut ui = GroupUi::new();
    let order = ui.order();
    ui.edit_group(2);
    ui.type_text("主帳號");
    ui.click(Pos2::new(5.0, 5.0));
    assert_eq!(ui.rename().id, 2);
    assert!(ui.rename().error.duplicate && ui.rename().error.message.is_none());
    ui.click(ui.action_point(1, "↓"));
    assert_eq!(ui.order(), order);
    assert_eq!(ui.group_draft().group_name(2), Some("遊戲帳號"));
    assert_eq!(ui.app.model.group_name(2), Some("遊戲帳號"));
    assert!(ui.rename().error.duplicate && ui.rename().error.message.is_none());
}

#[test]
fn empty_name_blocks_deleting_another_group_and_adding_a_group() {
    let mut ui = GroupUi::new();
    let groups = ui.app.model.groups.clone();
    ui.edit_group(3);
    ui.key(egui::Key::Backspace);
    assert!(ui.rename().name.is_empty());
    ui.click(ui.action_point(4, "刪除"));
    assert!(ui.rename().error.message.is_some());
    assert_eq!(ui.group_draft().groups, groups);
    assert_eq!(ui.app.model.groups, groups);
    ui.click(ui.point("新增草稿"));
    ui.key(egui::Key::Enter);
    assert!(ui.rename().error.message.is_some());
    assert_eq!(ui.app.model.groups, groups);
}

#[test]
fn sorting_above_and_below_editor_commits_the_correct_group_first() {
    for (editing, moving, arrow, expected_order) in [
        (3, 1, "↓", vec![0, 2, 1, 3, 4]),
        (1, 3, "↑", vec![0, 1, 3, 2, 4]),
    ] {
        let mut ui = GroupUi::new();
        ui.edit_group(editing);
        let name = format!("已改名{editing}");
        ui.type_text(&name);
        ui.click(ui.action_point(moving, arrow));
        ui.assert_idle_groups();
        assert_eq!(ui.group_draft().group_name(editing), Some(name.as_str()));
        assert_ne!(ui.app.model.group_name(editing), Some(name.as_str()));
        assert_eq!(ui.order(), expected_order);
        let at_edge = if arrow == "↑" {
            expected_order[1]
        } else {
            *expected_order.last().unwrap()
        };
        let revision = ui.group_draft().change_token();
        ui.click(ui.action_point(at_edge, arrow));
        assert_eq!(ui.order(), expected_order);
        assert_eq!(ui.group_draft().change_token(), revision);
        assert!(
            matches!(&ui.app.dialog, Some(Dialog::Groups { error, .. }) if error.message.is_none())
        );
    }
}

#[test]
fn enter_adds_group_clears_draft_and_keeps_focus_for_the_next_group() {
    let mut ui = GroupUi::new();
    assert!(ui.points("新增").is_empty());
    assert!(ui.points("返回").is_empty());
    ui.click(ui.point("新增草稿"));
    ui.key(egui::Key::Enter);
    ui.assert_idle_groups();
    assert_eq!(ui.group_draft().groups.len(), 6);
    assert_eq!(ui.group_draft().groups.last().unwrap().name, "新增草稿");
    assert!(
        matches!(ui.app.dialog, Some(Dialog::Groups { ref name, ref error, .. }) if name.is_empty() && !error.duplicate && error.message.is_none())
    );
    // No second click: typing must go to the freshly cleared input.
    ui.type_text("第二個新分組");
    assert!(
        matches!(ui.app.dialog, Some(Dialog::Groups { ref name, .. }) if name == "第二個新分組")
    );
    ui.key(egui::Key::Enter);
    assert_eq!(ui.group_draft().groups.len(), 7);
    assert_eq!(ui.group_draft().groups.last().unwrap().name, "第二個新分組");
    assert_eq!(ui.app.model.groups.len(), 5);
    assert!(!ui.app.runtime.app.has_pending());
    assert_eq!(ui.app.group, 0);
    ui.click(ui.point("套用"));
    wait_runtime(&mut ui);
    assert!(ui.app.dialog.is_none());
    assert_eq!(ui.app.model.groups.len(), 7);
    assert_eq!(ui.app.model.groups.last().unwrap().name, "第二個新分組");
    assert_eq!(ui.app.runtime.app.current.as_deref().unwrap().revision, 1);
}

#[test]
fn enter_add_rejects_blank_duplicate_and_group_limit_without_losing_draft() {
    for (draft, maxed) in [("", false), ("主帳號", false), ("超額新組", true)] {
        let mut ui = GroupUi::new();
        if maxed {
            while ui.app.model.groups.len() < 20 {
                let name = format!("填充分組{}", ui.app.model.groups.len());
                ui.app.model.add_group(&name).unwrap();
            }
            ui.size = Vec2::new(400.0, 1600.0);
            ui.open(Dialog::groups(&ui.app.model));
        }
        if let Some(Dialog::Groups { name, .. }) = &mut ui.app.dialog {
            *name = draft.into();
        }
        for _ in 0..4 {
            ui.frame(Vec::new());
        }
        let groups = ui.app.model.groups.clone();
        let label = if draft.is_empty() {
            "輸入名稱後按 Enter 新增"
        } else {
            draft
        };
        ui.click(ui.point(label));
        ui.key(egui::Key::Enter);
        ui.assert_idle_groups();
        assert_eq!(ui.app.model.groups, groups);
        assert!(
            matches!(ui.app.dialog, Some(Dialog::Groups { ref name, ref error, .. }) if name == draft && (error.duplicate || error.message.is_some()))
        );
        // A keystroke after rejection still edits the input, rather than being dropped.
        ui.type_text("x");
        assert!(
            matches!(ui.app.dialog, Some(Dialog::Groups { ref name, .. }) if name.len() == draft.len() + 1)
        );
    }
}

#[test]
fn close_discards_staged_order_and_valid_or_invalid_inline_name() {
    for (name, close) in [("關閉時放棄", "取消"), ("遊戲帳號", "×")] {
        let mut ui = GroupUi::new();
        let original = ui.app.model.snapshot();
        ui.click(ui.action_point(1, "↓"));
        ui.edit_group(1);
        ui.type_text(name);
        ui.click(ui.point(close));
        assert!(ui.app.dialog.is_none());
        assert_eq!(ui.app.model.snapshot(), original);
        assert!(!ui.app.runtime.app.has_pending());
    }
}

#[test]
fn twenty_groups_scroll_under_a_fixed_header_and_close_button() {
    let mut ui = GroupUi::new();
    while ui.app.model.groups.len() < 20 {
        let name = format!("填充分組{}", ui.app.model.groups.len());
        ui.app.model.add_group(&name).unwrap();
    }
    ui.size = Vec2::new(400.0, 420.0);
    ui.open(Dialog::groups(&ui.app.model));
    for _ in 0..4 {
        ui.frame(Vec::new());
    }
    let close = ui.point("×");
    let heading = ui.point("分組管理");
    let body = ui.group_point(0);
    ui.frame(vec![
        egui::Event::PointerMoved(body),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, -1800.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    for _ in 0..24 {
        ui.frame(Vec::new());
    }
    assert!(ui.points("預設  (8)").is_empty());
    assert_eq!(ui.points("輸入名稱後按 Enter 新增").len(), 1);
    assert!((ui.point("×") - close).length() < 1.0);
    assert!((ui.point("分組管理") - heading).length() < 1.0);
    ui.click(ui.point("×"));
    assert!(ui.app.dialog.is_none());
}

#[test]
fn authenticator_details_scroll_under_a_fixed_header_and_close_button() {
    let mut ui = GroupUi::new();
    let entry = &mut ui.app.model.entries[0];
    entry.record.description = "足夠長的備註，用來確認只有內容隨捲動移動。\n".repeat(30);
    let id = entry.id;
    let name = entry.record.name.clone();
    let secret = entry.record.secret.hex().to_ascii_uppercase();
    ui.app.model.snapshot();
    ui.app.dialog = Some(Dialog::details(&ui.app.model, &ui.app.model.entries[0]));
    ui.size = Vec2::new(320.0, 420.0);
    for _ in 0..4 {
        ui.frame(Vec::new());
    }
    let close = ui.point("×");
    let heading = ui.point("驗證器資料");
    let body = ui.point(&name);
    ui.frame(vec![
        egui::Event::PointerMoved(body),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, -4000.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    for _ in 0..24 {
        ui.frame(Vec::new());
    }
    assert!(ui.points(&name).is_empty());
    assert_eq!(ui.points(&secret).len(), 1);
    assert!(ui.points("上次校時").is_empty());
    assert!(ui.points("返回").is_empty());
    assert!((ui.point("×") - close).length() < 1.0);
    assert!((ui.point("驗證器資料") - heading).length() < 1.0);
    ui.click(ui.point("×"));
    assert!(ui.app.dialog.is_none());
    assert!(ui.app.model.entry(id).is_some());
}

#[test]
fn abrupt_dialog_resizes_recenter_in_the_same_frame_without_repeating_stable_passes() {
    for kind in 0..3 {
        let mut ui = GroupUi::new();
        let (dialog, title) = match kind {
            0 => {
                ui.app.model.entries[0].record.description =
                    "長備註，用來驗證縮小後的第一幀。\n".repeat(30);
                ui.app.model.snapshot();
                (
                    Dialog::details(&ui.app.model, &ui.app.model.entries[0]),
                    "驗證器資料",
                )
            }
            1 => {
                while ui.app.model.groups.len() < 20 {
                    ui.app
                        .model
                        .add_group(&format!("填充分組{}", ui.app.model.groups.len()))
                        .unwrap();
                }
                (Dialog::groups(&ui.app.model), "分組管理")
            }
            _ => {
                // Exercise both one-line and wrapped headings as the width changes.
                ui.ctx.global_style_mut(|style| {
                    style
                        .text_styles
                        .insert(egui::TextStyle::Heading, FontId::proportional(28.0));
                });
                (Dialog::delete(vec![1_001]), "確定要刪除下列驗證器？")
            }
        };
        ui.size = Vec2::new(400.0, 892.0);
        ui.open(dialog);
        let model = ui.app.model.clone();
        for size in [
            Vec2::new(320.0, 452.0),
            Vec2::new(640.0, 892.0),
            Vec2::new(320.0, 452.0),
            Vec2::new(400.0, 892.0),
        ] {
            ui.size = size;
            ui.frame(Vec::new());
            assert_eq!(ui.passes, 2, "{title}: resize needs one layout correction");
            let close = ui.point("×");
            let content = ui.dialog_content_rect();
            assert!(Rect::from_min_size(Pos2::ZERO, size).contains(close));
            assert!(
                (close - Pos2::new(content.right() - 14.0, content.top() + 15.5)).length() < 1.1
            );
            assert_eq!(ui.points(title).len(), 1);
            if kind == 2 && size.x == 320.0 {
                assert!(
                    ui.point(title).y - close.y > 10.0,
                    "the narrow heading must wrap"
                );
            }
            for _ in 0..3 {
                ui.frame(Vec::new());
                assert_eq!(ui.passes, 1, "{title}: stable layouts must not repeat");
                assert!((ui.point("×") - close).length() < 1.0);
            }
        }
        assert_eq!(ui.app.model, model);
        ui.click(ui.point("×"));
        assert!(ui.app.dialog.is_none());
        assert_eq!(ui.app.model, model);
    }
}

#[test]
fn every_dialog_ignores_backdrop_and_escape_and_has_a_visible_close_button() {
    for width in [320.0, 400.0] {
        let vault = bnauth_core::VaultSnapshot::new_empty();
        let mut preview = import_draft();
        preview.prepare_preview(&vault, vault.groups[0].id).unwrap();
        let model = VaultModel::test_fixture();
        let dialogs = [
            (Dialog::Import(Box::new(import_draft())), "匯入驗證器"),
            (Dialog::Import(Box::new(preview)), "匯入驗證器"),
            (
                Dialog::Edit {
                    id: 1_001,
                    name: "編輯草稿".into(),
                    description: "保留備註".into(),
                    error: String::new(),
                },
                "編輯名稱與備註",
            ),
            (Dialog::details(&model, &model.entries[0]), "驗證器資料"),
            (Dialog::delete(vec![1_001]), "確定要刪除下列驗證器？"),
            (Dialog::groups(&model), "分組管理"),
            (Dialog::Info, "BNAuth"),
        ];
        for (dialog, title) in dialogs {
            let mut ui = GroupUi::new();
            ui.size = Vec2::new(width, 420.0);
            let kind = std::mem::discriminant(&dialog);
            let model = ui.app.model.clone();
            ui.open(dialog);
            assert_eq!(ui.points(title).len(), 1, "{title} at width {width}");
            assert!(Rect::from_min_size(Pos2::ZERO, ui.size).contains(ui.point("×")));
            let content = ui.dialog_content_rect();
            let close = ui.point("×");
            let expected = Pos2::new(content.right() - 14.0, content.top() + 14.0);
            assert!(
                (close - expected).length() <= 1.5,
                "{title} at width {width}: close center {close:?} must be at the content's top-right {expected:?}, content={content:?}"
            );
            let cancel_visible = match ui.app.dialog.as_ref() {
                Some(Dialog::Groups { .. }) => true,
                Some(Dialog::Import(form)) if form.preview.is_some() => {
                    !ui.points("確認").is_empty()
                }
                _ => false,
            };
            assert_eq!(
                !ui.points("取消").is_empty(),
                cancel_visible,
                "{title} at width {width}"
            );
            assert!(ui.points("返回").is_empty());
            ui.click(Pos2::new(5.0, 5.0));
            assert_eq!(
                std::mem::discriminant(ui.app.dialog.as_ref().unwrap()),
                kind
            );
            ui.key(egui::Key::Escape);
            ui.key(egui::Key::Escape);
            assert_eq!(
                std::mem::discriminant(ui.app.dialog.as_ref().unwrap()),
                kind
            );
            assert_eq!(ui.app.model, model);
            ui.size = Vec2::new(if width == 320.0 { 400.0 } else { 320.0 }, 452.0);
            ui.frame(Vec::new());
            assert_eq!(
                ui.passes, 2,
                "{title}: resize must finish in the same frame"
            );
            let content = ui.dialog_content_rect();
            let close = ui.point("×");
            assert!(Rect::from_min_size(Pos2::ZERO, ui.size).contains(close));
            assert!(
                (close - Pos2::new(content.right() - 14.0, content.top() + 15.5)).length() < 1.1
            );
            assert_eq!(ui.points(title).len(), 1);
            ui.click(ui.point("×"));
            assert!(ui.app.dialog.is_none(), "{title} at width {width}");
            assert_eq!(ui.app.model, model);
        }
    }
}

#[test]
fn import_draft_and_preview_survive_outside_input_and_require_confirmation_to_save() {
    let mut ui = GroupUi::new();
    let mut original = bnauth_core::VaultSnapshot::new_empty();
    let destination = uuid::Uuid::new_v4();
    original.groups.push(bnauth_core::Group {
        id: destination,
        name: "PTR".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    ui.use_memory_vault(original.clone());
    ui.app.runtime.frame_time = 59_000;
    ui.open(Dialog::Import(Box::new(import_draft())));
    let expected = import_draft();
    for previewing in [false, true] {
        if previewing {
            ui.click(ui.point("預覽匯入"));
            assert_eq!(ui.points("匯入驗證器").len(), 1);
            assert_eq!(ui.points("名稱 *").len(), 1);
        }
        let preview = ui.import().preview.clone();
        assert_eq!(preview.is_some(), previewing);
        ui.click(Pos2::new(5.0, 5.0));
        ui.key(egui::Key::Escape);
        let actual = ui.import();
        assert_eq!(actual.name, expected.name);
        assert_eq!(actual.description, expected.description);
        assert_eq!(actual.secret, expected.secret);
        assert_eq!(actual.serial, expected.serial);
        assert_eq!(actual.restore, expected.restore);
        assert_eq!(actual.group, expected.group);
        assert!(actual.preview == preview);
    }
    let preview = ui.import().preview.clone();
    ui.click(ui.point("名稱 *") + Vec2::new(0.0, 24.0));
    ui.key(egui::Key::End);
    ui.type_text("!");
    ui.click(ui.point("備註") + Vec2::new(0.0, 24.0));
    ui.key(egui::Key::End);
    ui.type_text("!");
    assert_eq!(ui.import().secret, expected.secret);
    assert_eq!(ui.import().restore, expected.restore);
    assert_eq!(ui.points("匯入驗證器").len(), 1);
    assert!(ui.import().preview == preview);
    assert_eq!(ui.points("9428 7082").len(), 1);

    ui.ctx
        .memory_mut(|memory| memory.request_focus(DialogEditor::ImportRestore.id()));
    let mut reached_group = false;
    let mut reached_cancel = false;
    let mut reached_confirmation = false;
    for _ in 0..6 {
        ui.key(egui::Key::Tab);
        let focused = ui
            .ctx
            .memory(|memory| memory.focused())
            .and_then(|id| ui.ctx.read_response(id))
            .expect("keyboard focus in the import dialog");
        if focused.rect.contains(ui.point("預設")) {
            reached_group = true;
        }
        if focused.rect.contains(ui.point("取消")) {
            assert!(reached_group);
            reached_cancel = true;
        }
        if focused.rect.contains(ui.point("確認")) {
            reached_confirmation = true;
            break;
        }
    }
    assert!(reached_group && reached_cancel && reached_confirmation);

    ui.click(ui.point("預設"));
    ui.click(ui.point("PTR"));
    assert!(ui.import().preview == preview);
    assert_eq!(ui.points("9428 7082").len(), 1);
    assert_import_preview_is_read_only(&mut ui, &original);
    ui.click(ui.point("確認"));
    assert!(ui.app.dialog.is_none());
    let runtime = &mut ui.app.runtime;
    drain_memory_app(&mut runtime.app);
    let saved = runtime.app.current.as_deref().unwrap().clone();
    assert_eq!(saved.authenticators.len(), 1);
    assert_eq!(saved.authenticators[0].group_id, destination);
    assert_eq!(saved.authenticators[0].name, format!("{}!", expected.name));
    assert_eq!(
        saved.authenticators[0].description,
        format!("{}!", expected.description)
    );
    assert_eq!(
        saved.authenticators[0].restore_code.as_deref(),
        Some(expected.restore.as_str())
    );
    // Only poll the memory application, never the desktop's network-triggering poll_runtime.
    assert!(runtime.sync_rx.is_none());
    ui.app.model.apply_snapshot(&saved);

    // A duplicate preview is read-only until the user accepts the reminder.
    ui.open(Dialog::Import(Box::new(import_draft())));
    ui.click(ui.point("預覽匯入"));
    assert_eq!(ui.points("「PTR」的「人工匯入草稿!」").len(), 1);
    assert_import_preview_is_read_only(&mut ui, &saved);
    ui.click(ui.point("取消"));
    assert!(ui.app.dialog.is_none());
    assert_import_preview_is_read_only(&mut ui, &saved);

    ui.open(Dialog::Import(Box::new(import_draft())));
    ui.click(ui.point("預覽匯入"));
    ui.click(ui.point("確認"));
    assert!(ui.app.dialog.is_none());
    let runtime = &mut ui.app.runtime;
    assert!(runtime.app.has_pending());
    assert_eq!(runtime.app.current.as_deref(), Some(&saved));
    drain_memory_app(&mut runtime.app);
    let updated = runtime.app.current.as_deref().unwrap();
    assert_eq!(updated.revision, saved.revision + 1);
    assert_eq!(updated.authenticators.len(), 2);
    assert_eq!(updated.authenticators[0], saved.authenticators[0]);
    assert_ne!(updated.authenticators[1].id, saved.authenticators[0].id);
    assert_eq!(
        updated.authenticators[1].secret,
        saved.authenticators[0].secret
    );
}

fn assert_import_preview_is_read_only(ui: &mut GroupUi, saved: &bnauth_core::VaultSnapshot) {
    let runtime = &ui.app.runtime;
    assert_eq!(runtime.app.current.as_deref(), Some(saved));
    assert_eq!(runtime.app.phase, bnauth_app::Phase::Ready);
    assert!(!runtime.app.has_pending());
    assert!(!runtime.app.saving);
    assert!(!runtime.app.cleaning);
    assert!(runtime.sync_rx.is_none());
    assert_eq!(ui.app.model.snapshot(), *saved);
}

#[test]
fn import_preview_code_uses_shared_time_across_forward_and_backward_steps_without_saving() {
    let mut ui = GroupUi::new();
    let saved = bnauth_core::VaultSnapshot::new_empty();
    ui.use_memory_vault(saved.clone());
    ui.app.runtime.frame_time = 59_000;
    ui.size = Vec2::new(400.0, 572.0);
    ui.open(Dialog::Import(Box::new(import_draft())));
    assert!(ui.points("9428 7082").is_empty());
    ui.click(ui.point("預覽匯入"));
    let destination_label = ui.point("匯入至");
    let dropdown = ui.point("預設");
    let cancel = ui.point("取消");
    let confirm = ui.point("確認");
    assert!(
        (destination_label.y - dropdown.y).abs() < 2.0,
        "{destination_label:?} {dropdown:?}"
    );
    assert!(
        (dropdown.y - confirm.y).abs() < 2.0,
        "{dropdown:?} {confirm:?}"
    );
    assert!(destination_label.x < dropdown.x && dropdown.x < cancel.x);
    assert!(cancel.x < confirm.x && confirm.x - cancel.x < 64.0);
    assert!((cancel.y - confirm.y).abs() < 2.0);

    // RFC6238's public SHA1 seed at 59 seconds; the harness/UI time and the
    // system clock deliberately do not match this shared application time.
    for (time, code, filled) in [
        (59_000, "9428 7082", true),
        (59_999, "9428 7082", true),
        (60_000, "3735 9152", false),
        (59_000, "9428 7082", true),
    ] {
        ui.app.runtime.frame_time = time;
        ui.frame(Vec::new());
        assert_eq!(ui.points(code).len(), 1, "preview code at {time} ms");
        let bar = ui
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect) if rect.fill == ACCENT && rect.rect.height() == 4.0 => {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("preview progress bar");
        assert!(bar.top() > ui.point(code).y);
        assert!(bar.bottom() < ui.point("確認").y);
        assert_eq!(bar.width() > 100.0, filled);
        assert_import_preview_is_read_only(&mut ui, &saved);
    }
    ui.app.runtime.frame_time = -1;
    ui.frame(Vec::new());
    assert_eq!(
        ui.points("E13：時間異常\n時間偏差過大，無法產生正確驗證碼。")
            .len(),
        1
    );
    assert!(ui.points("9428 7082").is_empty());
    assert!(ui.points("3735 9152").is_empty());
    assert_import_preview_is_read_only(&mut ui, &saved);
}

#[test]
fn import_preview_code_recomputes_after_editing_the_secret_in_place() {
    let mut ui = GroupUi::new();
    let saved = bnauth_core::VaultSnapshot::new_empty();
    ui.use_memory_vault(saved.clone());
    ui.app.runtime.frame_time = 59_000;
    ui.open(Dialog::Import(Box::new(import_draft())));
    ui.click(ui.point("預覽匯入"));
    assert_eq!(ui.points("9428 7082").len(), 1);
    let first_key = ui.import().preview.as_ref().unwrap().secret.clone();

    let replacement = "11".repeat(20);
    let expected = bnauth_core::totp(
        &bnauth_core::Secret::from_hex(&replacement).unwrap(),
        59_000,
    )
    .unwrap();
    assert_ne!(expected, "94287082");
    ui.click(ui.point("密鑰 *") + Vec2::new(0.0, 24.0));
    ui.frame(vec![egui::Event::Key {
        key: egui::Key::A,
        physical_key: Some(egui::Key::A),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    }]);
    ui.type_text(&replacement);
    assert!(ui.import().preview.is_none());
    assert!(ui.points("9428 7082").is_empty());
    assert!(ui.points("確認").is_empty());
    ui.click(ui.point("預覽匯入"));
    assert_ne!(ui.import().preview.as_ref().unwrap().secret, first_key);
    assert_eq!(
        ui.points(&format!("{} {}", &expected[..4], &expected[4..]))
            .len(),
        1
    );
    assert!(ui.points("9428 7082").is_empty());
    assert_import_preview_is_read_only(&mut ui, &saved);
}

#[test]
fn import_preview_shows_the_candidate_code_and_up_to_five_duplicate_names() {
    let draft = import_draft();
    let mut empty = bnauth_core::VaultSnapshot::new_empty();
    let group = uuid::Uuid::new_v4();
    empty.groups.push(bnauth_core::Group {
        id: group,
        name: "其他分組".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    let first = bnauth_core::prepare_import(
        &empty,
        "不同密鑰的驗證器",
        "",
        empty.groups[0].id,
        bnauth_core::parse_manual(&"11".repeat(20), "", "").unwrap(),
    )
    .unwrap()
    .candidate(&empty)
    .unwrap();
    let mut saved = bnauth_core::prepare_import(
        &first,
        "既有RFC驗證器",
        &draft.description,
        group,
        bnauth_core::parse_manual(&draft.secret, &draft.serial, &draft.restore).unwrap(),
    )
    .unwrap()
    .candidate(&first)
    .unwrap();
    for index in 1..=5 {
        let mut record = saved.authenticators[1].clone();
        record.id = uuid::Uuid::new_v4();
        record.name = format!("既有RFC驗證器 {index}");
        record.sort_order = index;
        saved.authenticators.push(record);
    }
    let mut ui = GroupUi::new();
    ui.use_memory_vault(saved.clone());
    ui.app.runtime.frame_time = 59_000;
    let wrong_code = ui.app.otp_cache.code(&saved.authenticators[0], 59_000);
    assert_ne!(wrong_code, "94287082");
    ui.open(Dialog::Import(Box::new(draft)));
    ui.click(ui.point("預覽匯入"));
    let preview = ui.import().preview.as_ref().unwrap();
    assert_eq!(
        preview
            .record(&ui.import().name, &ui.import().description)
            .group_id,
        saved.groups[0].id
    );
    assert_eq!(preview.matches.len(), 5);
    assert_eq!(ui.points("此驗證器密鑰與下列相同：").len(), 1);
    for record in &saved.authenticators[1..=5] {
        assert_eq!(
            ui.points(&format!("「其他分組」的「{}」", record.name))
                .len(),
            1
        );
    }
    assert!(ui.points("「其他分組」的「既有RFC驗證器 5」").is_empty());
    assert_eq!(ui.points("9428 7082").len(), 1);
    assert!(
        ui.points(&format!("{} {}", &wrong_code[..4], &wrong_code[4..]))
            .is_empty()
    );
    assert_import_preview_is_read_only(&mut ui, &saved);
}

#[test]
fn successful_edit_and_delete_actions_still_finish_their_dialogs() {
    let mut ui = GroupUi::new();
    let id = ui.app.model.entries[0].id;
    ui.open(Dialog::Edit {
        id,
        name: "套用後名稱".into(),
        description: "套用後備註".into(),
        error: String::new(),
    });
    ui.click(ui.point("套用"));
    assert!(ui.app.dialog.is_none());
    wait_runtime(&mut ui);
    let entry = ui.app.model.entry(id).unwrap();
    assert_eq!(entry.record.name, "套用後名稱");
    assert_eq!(entry.record.description, "套用後備註");
    ui.open(Dialog::delete(vec![id]));
    ui.click(ui.point("確認"));
    assert!(ui.app.dialog.is_none());
    wait_runtime(&mut ui);
    assert!(ui.app.model.entry(id).is_none());
    // Start the group workflow with the previous record edit and deletion saved.
    let vault = ui.app.model.snapshot();
    ui.use_memory_vault(vault);
    let entries = ui.app.model.entries.clone();
    let nonempty = ui.app.model.groups[1].id;
    let empty = ui.app.model.groups[4].id;
    ui.app.switch_group(empty);
    ui.open(Dialog::groups(&ui.app.model));
    ui.click(ui.action_point(nonempty, "刪除"));
    ui.assert_idle_groups();
    assert!(ui.group_draft().group_name(nonempty).is_some());
    ui.edit_group(empty);
    ui.key(egui::Key::Backspace);
    assert!(ui.rename().name.is_empty());
    ui.click(ui.action_point(empty, "刪除"));
    ui.assert_idle_groups();
    assert!(ui.group_draft().group_name(empty).is_none());
    assert!(ui.app.model.group_name(empty).is_some());
    assert_eq!(ui.app.group, empty);
    ui.click(ui.point("取消"));
    assert!(ui.app.dialog.is_none());
    assert!(!ui.app.runtime.app.has_pending());
    ui.open(Dialog::groups(&ui.app.model));
    ui.edit_group(empty);
    let duplicate = ui.app.model.group_name(nonempty).unwrap().to_owned();
    ui.type_text(&duplicate);
    assert!(ui.rename().error.duplicate);
    ui.click(ui.action_point(empty, "刪除"));
    ui.assert_idle_groups();
    assert!(ui.group_draft().group_name(empty).is_none());
    assert!(ui.app.model.group_name(empty).is_some());
    ui.click(ui.point("套用"));
    wait_runtime(&mut ui);
    assert!(ui.app.dialog.is_none());
    assert!(ui.app.model.group_name(empty).is_none());
    assert_eq!(ui.app.group, 0);
    assert_eq!(ui.app.model.entries, entries);
}
