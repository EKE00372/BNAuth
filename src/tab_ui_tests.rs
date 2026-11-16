use super::*;

struct TabUi {
    app: BnAuthApp,
    ctx: egui::Context,
    width: f32,
    time: f64,
    shapes: Vec<egui::epaint::ClippedShape>,
    header_rect: Rect,
}

impl TabUi {
    fn new(width: f32, group_count: usize) -> Self {
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
        let mut style = egui::Style::default();
        style.animation_time = 0.0;
        style.scroll_animation = egui::style::ScrollAnimation::none();
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
        if group_count < model.groups.len() {
            model.groups.truncate(group_count);
            model
                .entries
                .retain(|entry| model.groups.iter().any(|group| group.id == entry.group_id));
        } else {
            while model.groups.len() < group_count {
                model
                    .add_group(&format!("測試{:02}", model.groups.len()))
                    .unwrap();
            }
        }
        if group_count > 1 {
            model
                .rename_group(1, &"這是很長的分組名稱".repeat(9))
                .unwrap();
        }
        let runtime = crate::test_support::runtime(model.snapshot(), 1_700_000_000_000);
        let mut ui = Self {
            app: BnAuthApp {
                window_chrome: crate::window_chrome::WindowChrome::default(),
                model,
                group: 0,
                query: String::new(),
                selected: BTreeSet::new(),
                dialog: None,
                notifications: crate::notifications::Notifications::default(),
                runtime,
                otp_cache: crate::otp_cache::OtpCache::default(),
                projection: EntryProjection::default(),
                drag_origin: None,
                dragging: None,
                drop: None,
                tab_reveal: Some(0),
                tab_scroll: None,
            },
            ctx,
            width,
            time: 0.0,
            shapes: Vec::new(),
            header_rect: Rect::NOTHING,
        };
        ui.settle(4);
        ui
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        self.time += 0.04;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(self.width, 860.0),
            )),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let ctx = self.ctx.clone();
        let mut output = ctx.run_ui(input, |ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
            self.header_rect = egui::Frame::NONE
                .show(ui, |ui| self.app.tabs(ui))
                .response
                .rect;
            self.app.search(ui);
            self.app.footer(ui);
        });
        // This harness inspects paint shapes without a GPU texture consumer.
        output.textures_delta.clear();
        self.shapes = output.shapes;
    }

    fn settle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.frame(Vec::new());
        }
    }

    fn point(&self, label: &str) -> Option<Pos2> {
        fn visit(shape: &egui::Shape, clip: Rect, label: &str) -> Option<Pos2> {
            match shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    let center = text.pos + text.galley.rect.center().to_vec2();
                    clip.contains(center).then_some(center)
                }
                egui::Shape::Vec(shapes) => {
                    shapes.iter().find_map(|shape| visit(shape, clip, label))
                }
                _ => None,
            }
        }
        self.shapes
            .iter()
            .find_map(|shape| visit(&shape.shape, shape.clip_rect, label))
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
        self.settle(3);
    }

    fn wheel(&mut self, vertical_delta: f32) {
        let point = Pos2::new(60.0, self.header_rect.center().y);
        self.frame(vec![
            egui::Event::PointerMoved(point),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, vertical_delta),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        self.settle(8);
    }
}

#[test]
fn long_names_and_twenty_groups_keep_the_header_inside_narrow_windows() {
    for width in [320.0, 400.0] {
        let mut ui = TabUi::new(width, 20);
        assert!(
            ui.header_rect.right() <= width + 0.5,
            "{:?}",
            ui.header_rect
        );
        assert!(ui.header_rect.height() <= 44.0, "{:?}", ui.header_rect);
        assert!(ui.point("←").is_some());
        assert!(ui.point("→").is_some());
        // The management button stays accessible at the right edge despite the long label.
        ui.click(Pos2::new(width - 18.0, ui.header_rect.center().y));
        assert!(matches!(ui.app.dialog, Some(Dialog::Groups { .. })));
    }
}

#[test]
fn arrows_reveal_the_last_tab_and_allow_switching_to_it() {
    let mut ui = TabUi::new(320.0, 20);
    assert!(ui.point("測試19").is_none());
    for _ in 0..25 {
        if ui.point("測試19").is_some() {
            break;
        }
        ui.click(ui.point("→").expect("Overflow must show a right arrow"));
    }
    assert_eq!(ui.app.group, 0, "Scrolling alone must not switch groups");
    ui.click(
        ui.point("測試19")
            .expect("The last group must be reachable"),
    );
    assert_eq!(ui.app.group, 19);
    assert!(ui.point("總共 29 個，本組 0 個").is_some());
}

#[test]
fn vertical_wheel_over_tabs_scrolls_horizontally() {
    let mut ui = TabUi::new(400.0, 20);
    assert!(ui.point("測試19").is_none());
    for _ in 0..15 {
        if ui.point("測試19").is_some() {
            break;
        }
        ui.wheel(-400.0);
    }
    assert!(
        ui.point("測試19").is_some(),
        "Vertical wheel must reach the last group"
    );
    assert_eq!(ui.app.group, 0);
}

#[test]
fn selecting_a_hidden_group_reveals_it_without_overriding_later_user_scroll() {
    let mut ui = TabUi::new(320.0, 20);
    ui.app.switch_group(19);
    ui.settle(5);
    assert!(ui.point("測試19").is_some());
    for _ in 0..15 {
        if ui.point("未分組").is_some() {
            break;
        }
        ui.wheel(400.0);
    }
    ui.settle(12);
    assert_eq!(ui.app.group, 19);
    assert!(ui.point("總共 29 個，本組 0 個").is_some());
    assert!(
        ui.point("未分組").is_some(),
        "User scrolling must not snap back to the active tab"
    );
    assert!(ui.point("測試19").is_none());
}

#[test]
fn arrows_disappear_when_all_tabs_fit() {
    let mut ui = TabUi::new(400.0, 1);
    assert!(ui.point("未分組").is_some());
    assert!(ui.point("←").is_none());
    assert!(ui.point("→").is_none());
    ui.wheel(-400.0);
    assert!(ui.point("未分組").is_some());
    assert_eq!(ui.app.group, 0);
}

#[test]
fn tab_labels_align_with_controls_and_the_underline_stays_above_search() {
    for width in [320.0, 400.0] {
        let ui = TabUi::new(width, 20);
        assert!(ui.point("8").is_none(), "Tabs must not show counts");
        assert!(ui.point("12").is_none(), "Tabs must not show counts");
        assert!(ui.point("總共 29 個，本組 8 個").is_some());
        let label = ui.point("未分組").unwrap();
        let left = ui.point("←").unwrap();
        let right = ui.point("→").unwrap();
        assert!((left.x - right.x).abs() < 0.1);
        assert!(left.y < right.y);
        let arrow = Pos2::new(left.x, (left.y + right.y) / 2.0);
        assert!(
            (label.y - arrow.y).abs() <= 3.1,
            "Tab and controls are misaligned: {label:?}, {arrow:?}"
        );
        let mut underlines = 0;
        for shape in &ui.shapes {
            if let egui::Shape::LineSegment { points, stroke } = &shape.shape
                && stroke.color == ACCENT
                && (points[0].y - points[1].y).abs() < 0.1
            {
                underlines += 1;
                assert!(
                    points.iter().all(|point| ui.header_rect.contains(*point)),
                    "Underline extends beyond the header: {points:?} vs {:?}",
                    ui.header_rect
                );
            }
        }
        assert_eq!(underlines, 1);
    }
}
