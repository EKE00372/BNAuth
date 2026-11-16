//! Client-drawn window controls; native movement, sizing and closing stay in winit.
use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Id, Order, Pos2, Rect, ResizeDirection, Sense,
    Stroke, Vec2, ViewportCommand, WindowLevel,
};

pub const TITLE_BAR_HEIGHT: f32 = 32.0;
const BUTTON_WIDTH: f32 = 44.0;
const BORDER: f32 = 5.0;
const BACKGROUND: Color32 = Color32::from_rgb(32, 32, 32);
const FOREGROUND: Color32 = Color32::from_rgb(210, 210, 210);
const ACCENT: Color32 = Color32::from_rgb(71, 168, 245);

#[derive(Default)]
pub struct WindowChrome {
    pinned: bool,
    restore_requested: bool,
    last_pixels_per_point: Option<f32>,
    primary_origin: Option<Id>,
}

#[derive(Clone, Copy)]
enum Control {
    Pin,
    Minimize,
    Close,
}

impl WindowChrome {
    pub fn enforce_window_state(&mut self, ctx: &egui::Context) {
        if ctx.input(|input| {
            !input.focused
                || input.viewport().visible() == Some(false)
                || input.key_pressed(egui::Key::Escape)
                || input.pointer.button_released(egui::PointerButton::Primary)
        }) {
            self.primary_origin = None;
        }
        let pixels_per_point = ctx.pixels_per_point();
        if self.last_pixels_per_point != Some(pixels_per_point) {
            crate::runtime::apply_window_size_limits(ctx);
            self.last_pixels_per_point = Some(pixels_per_point);
        }
        match ctx.input(|input| input.viewport().maximized) {
            Some(true) if !self.restore_requested => {
                ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                self.restore_requested = true;
            }
            Some(false) => self.restore_requested = false,
            _ => {}
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let primary_press = ctx.input(|input| {
            if input.pointer.button_pressed(egui::PointerButton::Primary) {
                self.primary_origin = None;
            }
            if !input.focused
                || input.viewport().visible() == Some(false)
                || !input.pointer.button_down(egui::PointerButton::Primary)
                || input.key_pressed(egui::Key::Escape)
            {
                return None;
            }
            input.events.iter().rev().find_map(|event| match event {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    ..
                } => Some(*pos),
                _ => None,
            })
        });
        let window = ctx.content_rect();
        let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
        let spacing = ui.spacing().item_spacing.y;
        ui.spacing_mut().item_spacing.y = 0.0;
        let (title, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), TITLE_BAR_HEIGHT),
            Sense::hover(),
        );
        ui.spacing_mut().item_spacing.y = spacing;

        // A separate upper layer keeps window controls usable while an egui Modal
        // blocks the application. It covers only the title, never the form below.
        egui::Area::new(Id::unique("window_chrome"))
            .order(Order::Tooltip)
            .fixed_pos(title.min)
            .default_size(title.size())
            .movable(false)
            .constrain(false)
            .sense(Sense::hover())
            .show(&ctx, |ui| {
                ui.set_min_size(title.size());
                ui.set_max_size(title.size());
                ui.painter().rect_filled(title, 0.0, BACKGROUND);
                let controls = [Control::Pin, Control::Minimize, Control::Close];
                let controls_left = title.right() - controls.len() as f32 * BUTTON_WIDTH;
                let inside = if maximized {
                    window
                } else {
                    window.shrink(BORDER)
                };
                let drag_rect =
                    Rect::from_min_max(title.min, Pos2::new(controls_left, title.bottom()))
                        .intersect(inside);
                let drag = ui.interact(
                    drag_rect,
                    Id::unique("window_drag"),
                    Sense::click_and_drag(),
                );
                if self.native_drag_started(ui, &drag, primary_press) {
                    ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                }
                ui.painter().text(
                    Pos2::new(title.left() + 12.0, title.center().y),
                    Align2::LEFT_CENTER,
                    "BNAuth",
                    FontId::proportional(13.0),
                    FOREGROUND,
                );
                for (index, control) in controls.into_iter().enumerate() {
                    let rect = Rect::from_min_size(
                        Pos2::new(controls_left + index as f32 * BUTTON_WIDTH, title.top()),
                        Vec2::new(BUTTON_WIDTH, TITLE_BAR_HEIGHT),
                    );
                    let hint = match control {
                        Control::Pin if self.pinned => "取消置頂",
                        Control::Pin => "置頂",
                        Control::Minimize => "最小化",
                        Control::Close => "關閉",
                    };
                    let response = ui.interact(
                        rect.intersect(inside),
                        Id::unique(("window_control", index)),
                        Sense::click(),
                    );
                    response
                        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, hint));
                    let hovered = response.hovered() || response.is_pointer_button_down_on();
                    let fill = if hovered {
                        if matches!(control, Control::Close) {
                            Color32::from_rgb(196, 43, 28)
                        } else {
                            Color32::from_rgb(55, 55, 55)
                        }
                    } else {
                        BACKGROUND
                    };
                    ui.painter().rect_filled(rect, 0.0, fill);
                    paint_icon(ui.painter(), rect.center(), control, self.pinned);
                    if response.clicked() {
                        let command = match control {
                            Control::Pin => {
                                self.pinned = !self.pinned;
                                ViewportCommand::WindowLevel(if self.pinned {
                                    WindowLevel::AlwaysOnTop
                                } else {
                                    WindowLevel::Normal
                                })
                            }
                            Control::Minimize => ViewportCommand::Minimized(true),
                            Control::Close => ViewportCommand::Close,
                        };
                        ctx.send_viewport_cmd(command);
                    }
                    response.on_hover_text(hint);
                }
            });

        if !maximized {
            self.resize_edges(&ctx, window, primary_press);
        }
    }

    fn native_drag_started(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        primary_press: Option<Pos2>,
    ) -> bool {
        // The button held during an egui drag is not proof of its press origin.
        if let Some(pos) = primary_press
            && response.rect.contains(pos)
            && ui.clip_rect().contains(pos)
            && ui.ctx().layer_id_at(pos) == Some(ui.layer_id())
        {
            self.primary_origin = Some(response.id);
        }
        if self.primary_origin == Some(response.id)
            && response.drag_started_by(egui::PointerButton::Primary)
        {
            // Windows owns the gesture after handoff; no UI release is required.
            self.primary_origin = None;
            true
        } else {
            false
        }
    }

    fn resize_edges(&mut self, ctx: &egui::Context, window: Rect, primary_press: Option<Pos2>) {
        let x = [
            window.left(),
            window.left() + BORDER,
            window.right() - BORDER,
            window.right(),
        ];
        let y = [
            window.top(),
            window.top() + BORDER,
            window.bottom() - BORDER,
            window.bottom(),
        ];
        let edges = [
            (
                0,
                0,
                1,
                1,
                ResizeDirection::NorthWest,
                CursorIcon::ResizeNwSe,
            ),
            (
                1,
                0,
                2,
                1,
                ResizeDirection::North,
                CursorIcon::ResizeVertical,
            ),
            (
                2,
                0,
                3,
                1,
                ResizeDirection::NorthEast,
                CursorIcon::ResizeNeSw,
            ),
            (
                0,
                1,
                1,
                2,
                ResizeDirection::West,
                CursorIcon::ResizeHorizontal,
            ),
            (
                2,
                1,
                3,
                2,
                ResizeDirection::East,
                CursorIcon::ResizeHorizontal,
            ),
            (
                0,
                2,
                1,
                3,
                ResizeDirection::SouthWest,
                CursorIcon::ResizeNeSw,
            ),
            (
                1,
                2,
                2,
                3,
                ResizeDirection::South,
                CursorIcon::ResizeVertical,
            ),
            (
                2,
                2,
                3,
                3,
                ResizeDirection::SouthEast,
                CursorIcon::ResizeNwSe,
            ),
        ];
        for (index, (left, top, right, bottom, direction, cursor)) in edges.into_iter().enumerate()
        {
            let rect =
                Rect::from_min_max(Pos2::new(x[left], y[top]), Pos2::new(x[right], y[bottom]));
            // Eight narrow Areas preserve resizing over modals without swallowing
            // normal client-area input. Controls start inside this outer strip.
            egui::Area::new(Id::unique(("window_resize", index)))
                .order(Order::Tooltip)
                .fixed_pos(rect.min)
                .default_size(rect.size())
                .movable(false)
                .constrain(false)
                .sense(Sense::hover())
                .show(ctx, |ui| {
                    ui.set_min_size(rect.size());
                    ui.set_max_size(rect.size());
                    let response = ui.interact(
                        rect,
                        Id::unique(("window_resize_handle", index)),
                        Sense::drag(),
                    );
                    if response.hovered() || response.dragged() {
                        ctx.set_cursor_icon(cursor);
                    }
                    if self.native_drag_started(ui, &response, primary_press) {
                        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
                    }
                });
        }
    }
}

fn paint_icon(painter: &egui::Painter, center: Pos2, control: Control, pinned: bool) {
    let stroke = Stroke::new(
        1.2,
        if matches!(control, Control::Pin) && pinned {
            ACCENT
        } else {
            FOREGROUND
        },
    );
    let p = |x, y| center + Vec2::new(x, y);
    match control {
        Control::Pin => {
            painter.add(egui::Shape::line(
                vec![
                    p(-4.0, -6.0),
                    p(4.0, -6.0),
                    p(3.0, -4.0),
                    p(3.0, 0.0),
                    p(6.0, 3.0),
                    p(-6.0, 3.0),
                    p(-3.0, 0.0),
                    p(-3.0, -4.0),
                    p(-4.0, -6.0),
                ],
                stroke,
            ));
            painter.line_segment([p(0.0, 3.0), p(0.0, 8.0)], stroke);
        }
        Control::Minimize => {
            painter.line_segment([p(-5.0, 0.0), p(5.0, 0.0)], stroke);
        }
        Control::Close => {
            painter.line_segment([p(-5.0, -5.0), p(5.0, 5.0)], stroke);
            painter.line_segment([p(-5.0, 5.0), p(5.0, -5.0)], stroke);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Harness {
        chrome: WindowChrome,
        ctx: egui::Context,
        time: f64,
        modal: bool,
        focused: bool,
        maximized: Option<bool>,
        size: Vec2,
    }

    impl Harness {
        fn new(modal: bool) -> Self {
            let ctx = egui::Context::default();
            let mut fonts = egui::FontDefinitions::empty();
            fonts.font_data.insert(
                "Nowar".into(),
                egui::FontData::from_static(include_bytes!(
                    "../assets/fonts/NowarSansTW-Regular.ttf"
                ))
                .into(),
            );
            fonts
                .families
                .insert(egui::FontFamily::Proportional, vec!["Nowar".into()]);
            ctx.set_fonts(fonts);
            let mut value = Self {
                chrome: WindowChrome::default(),
                ctx,
                time: 0.0,
                modal,
                focused: true,
                maximized: Some(false),
                size: Vec2::new(400.0, 860.0),
            };
            for _ in 0..4 {
                value.frame(Vec::new());
            }
            value
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<ViewportCommand> {
            self.time += 0.04;
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
                time: Some(self.time),
                focused: self.focused,
                events,
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .maximized = self.maximized;
            let ctx = self.ctx.clone();
            let mut output = ctx.run_ui(input, |ui| {
                self.chrome.enforce_window_state(ui.ctx());
                self.chrome.show(ui);
                ui.label("body");
                if self.modal {
                    egui::Modal::new(Id::unique("modal_test")).show(ui.ctx(), |ui| {
                        ui.label("modal");
                    });
                }
            });
            output.textures_delta.clear();
            output
                .viewport_output
                .remove(&egui::ViewportId::ROOT)
                .unwrap()
                .commands
        }

        fn click(&mut self, point: Pos2) -> Vec<ViewportCommand> {
            let mut commands = Vec::new();
            for pressed in [true, false] {
                commands.extend(self.frame(vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]));
            }
            commands
        }

        fn drag(&mut self, point: Pos2) -> Vec<ViewportCommand> {
            let mut commands = self.frame(vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            commands.extend(self.frame(vec![egui::Event::PointerMoved(point + Vec2::splat(15.0))]));
            commands
        }
    }

    #[test]
    fn three_caption_controls_stay_on_the_right_and_send_native_commands() {
        for modal in [false, true] {
            let mut ui = Harness::new(modal);
            for width in [320.0, 400.0, 2160.0, 320.0] {
                ui.size.x = width;
                for _ in 0..3 {
                    ui.frame(Vec::new());
                }
                for (offset, expected) in [
                    (
                        110.0,
                        ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop),
                    ),
                    (110.0, ViewportCommand::WindowLevel(WindowLevel::Normal)),
                    (66.0, ViewportCommand::Minimized(true)),
                    (22.0, ViewportCommand::Close),
                ] {
                    let commands = ui.click(Pos2::new(width - offset, 18.0));
                    assert!(
                        commands.contains(&expected),
                        "width={width}, modal={modal}: {commands:?}"
                    );
                    assert!(
                        !commands
                            .iter()
                            .any(|command| matches!(command, ViewportCommand::Maximized(_)))
                    );
                }
            }
        }
    }

    #[test]
    fn double_clicking_title_never_maximizes_the_window() {
        for modal in [false, true] {
            for point in [Pos2::new(90.0, 18.0), Pos2::new(246.0, 18.0)] {
                let mut ui = Harness::new(modal);
                let mut commands = ui.click(point);
                commands.extend(ui.click(point));
                assert!(
                    !commands
                        .iter()
                        .any(|command| matches!(command, ViewportCommand::Maximized(_)))
                );
            }
        }
    }

    #[test]
    fn title_drag_and_all_eight_edges_use_native_dragging() {
        assert!(
            Harness::new(false)
                .drag(Pos2::new(90.0, 18.0))
                .contains(&ViewportCommand::StartDrag)
        );
        for (point, direction) in [
            (Pos2::new(2.0, 2.0), ResizeDirection::NorthWest),
            (Pos2::new(200.0, 2.0), ResizeDirection::North),
            (Pos2::new(398.0, 2.0), ResizeDirection::NorthEast),
            (Pos2::new(2.0, 430.0), ResizeDirection::West),
            (Pos2::new(398.0, 430.0), ResizeDirection::East),
            (Pos2::new(2.0, 858.0), ResizeDirection::SouthWest),
            (Pos2::new(200.0, 858.0), ResizeDirection::South),
            (Pos2::new(398.0, 858.0), ResizeDirection::SouthEast),
        ] {
            let commands = Harness::new(true).drag(point);
            assert!(
                commands.contains(&ViewportCommand::BeginResize(direction)),
                "{direction:?}: {commands:?}"
            );
            assert!(!commands.contains(&ViewportCommand::Close));
        }
    }

    #[test]
    fn display_scale_changes_reapply_size_limits_once() {
        let mut ui = Harness::new(false);
        for scale in [1.25, 2.0, 1.0] {
            ui.ctx.set_pixels_per_point(scale);
            let commands = ui.frame(Vec::new());
            assert_eq!(ui.ctx.pixels_per_point(), scale);
            assert_eq!(
                commands
                    .iter()
                    .filter(|command| matches!(command, ViewportCommand::MaxInnerSize(_)))
                    .cloned()
                    .collect::<Vec<_>>(),
                vec![ViewportCommand::MaxInnerSize(Vec2::new(2160.0, 3840.0))]
            );
            assert_eq!(
                commands
                    .iter()
                    .filter(|command| matches!(command, ViewportCommand::MinInnerSize(_)))
                    .cloned()
                    .collect::<Vec<_>>(),
                vec![ViewportCommand::MinInnerSize(Vec2::new(320.0, 452.0))]
            );
            for _ in 0..3 {
                let commands = ui.frame(Vec::new());
                assert!(!commands.iter().any(|command| matches!(
                    command,
                    ViewportCommand::MaxInnerSize(_) | ViewportCommand::MinInnerSize(_)
                )));
            }
        }
    }

    #[test]
    fn native_size_limit_commands_cap_each_dimension_and_leave_valid_sizes_alone() {
        for (size, resized) in [
            (Vec2::new(400.0, 892.0), None),
            (Vec2::new(8192.0, 8192.0), Some(Vec2::new(2160.0, 3840.0))),
            (Vec2::new(900.0, 8192.0), Some(Vec2::new(900.0, 3840.0))),
            (Vec2::new(8192.0, 1000.0), Some(Vec2::new(2160.0, 1000.0))),
            (Vec2::new(2160.0, 3840.0), None),
        ] {
            let ctx = egui::Context::default();
            let mut chrome = WindowChrome::default();
            let rect = Rect::from_min_size(Pos2::ZERO, size);
            let mut input = egui::RawInput {
                screen_rect: Some(rect),
                ..Default::default()
            };
            let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
            viewport.inner_rect = Some(rect);
            viewport.maximized = Some(false);
            viewport.minimized = Some(false);
            let mut output = ctx.run_ui(input, |ui| chrome.enforce_window_state(ui.ctx()));
            output.textures_delta.clear();
            let commands = output
                .viewport_output
                .remove(&egui::ViewportId::ROOT)
                .unwrap()
                .commands;
            assert!(commands.contains(&ViewportCommand::MaxInnerSize(Vec2::new(2160.0, 3840.0))));
            assert!(commands.contains(&ViewportCommand::MinInnerSize(Vec2::new(320.0, 452.0))));
            assert_eq!(
                commands
                    .into_iter()
                    .filter(|command| matches!(command, ViewportCommand::InnerSize(_)))
                    .collect::<Vec<_>>(),
                resized
                    .map(ViewportCommand::InnerSize)
                    .into_iter()
                    .collect::<Vec<_>>(),
                "native size limit commands for {size:?}"
            );
        }
    }

    #[test]
    fn external_maximization_requests_restore_once_until_normal_is_confirmed() {
        for modal in [false, true] {
            let mut ui = Harness::new(modal);
            assert!(
                !ui.frame(Vec::new())
                    .iter()
                    .any(|command| matches!(command, ViewportCommand::Maximized(_)))
            );
            ui.maximized = Some(true);
            let commands = ui.frame(Vec::new());
            assert_eq!(
                commands
                    .iter()
                    .filter(|command| matches!(command, ViewportCommand::Maximized(false)))
                    .count(),
                1
            );
            assert!(!commands.contains(&ViewportCommand::Maximized(true)));
            for reported in [Some(true), Some(true), None, Some(true)] {
                ui.maximized = reported;
                assert!(
                    !ui.frame(Vec::new())
                        .iter()
                        .any(|command| matches!(command, ViewportCommand::Maximized(_)))
                );
            }
            ui.maximized = Some(false);
            assert!(
                !ui.frame(Vec::new())
                    .iter()
                    .any(|command| matches!(command, ViewportCommand::Maximized(_)))
            );
            ui.maximized = Some(true);
            let commands = ui.frame(Vec::new());
            assert_eq!(
                commands
                    .iter()
                    .filter(|command| matches!(command, ViewportCommand::Maximized(false)))
                    .count(),
                1
            );
            assert!(!commands.contains(&ViewportCommand::Maximized(true)));
        }
    }

    #[test]
    fn maximized_window_has_no_resize_handles() {
        let mut ui = Harness::new(false);
        ui.maximized = Some(true);
        for _ in 0..3 {
            ui.frame(Vec::new());
        }
        let commands = ui.drag(Pos2::new(398.0, 858.0));
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command, ViewportCommand::BeginResize(_)))
        );
    }
    fn chrome_button(pos: Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn native_gesture_commands(commands: &[ViewportCommand]) -> Vec<&ViewportCommand> {
        commands
            .iter()
            .filter(|command| {
                matches!(
                    command,
                    ViewportCommand::StartDrag | ViewportCommand::BeginResize(_)
                )
            })
            .collect()
    }

    fn chrome_drag_targets() -> [(Pos2, ViewportCommand); 9] {
        [
            (Pos2::new(90.0, 18.0), ViewportCommand::StartDrag),
            (
                Pos2::new(2.0, 2.0),
                ViewportCommand::BeginResize(ResizeDirection::NorthWest),
            ),
            (
                Pos2::new(200.0, 2.0),
                ViewportCommand::BeginResize(ResizeDirection::North),
            ),
            (
                Pos2::new(398.0, 2.0),
                ViewportCommand::BeginResize(ResizeDirection::NorthEast),
            ),
            (
                Pos2::new(2.0, 430.0),
                ViewportCommand::BeginResize(ResizeDirection::West),
            ),
            (
                Pos2::new(398.0, 430.0),
                ViewportCommand::BeginResize(ResizeDirection::East),
            ),
            (
                Pos2::new(2.0, 858.0),
                ViewportCommand::BeginResize(ResizeDirection::SouthWest),
            ),
            (
                Pos2::new(200.0, 858.0),
                ViewportCommand::BeginResize(ResizeDirection::South),
            ),
            (
                Pos2::new(398.0, 858.0),
                ViewportCommand::BeginResize(ResizeDirection::SouthEast),
            ),
        ]
    }

    #[test]
    fn blank_primary_then_secondary_never_moves_or_resizes_and_next_primary_works() {
        for (target, expected) in chrome_drag_targets() {
            let mut ui = Harness::new(false);
            // This client-area gap has no drag-sensitive widget and is outside the
            // title, resize handles and caption buttons.
            let blank = Pos2::new(200.0, 400.0);
            let mut commands = ui.frame(vec![egui::Event::PointerMoved(blank)]);
            commands.extend(ui.frame(vec![chrome_button(
                blank,
                egui::PointerButton::Primary,
                true,
            )]));
            commands.extend(ui.frame(vec![egui::Event::PointerMoved(target)]));
            commands.extend(ui.frame(vec![chrome_button(
                target,
                egui::PointerButton::Secondary,
                true,
            )]));
            let moved = target + Vec2::splat(15.0);
            commands.extend(ui.frame(vec![egui::Event::PointerMoved(moved)]));
            assert!(
                ui.ctx
                    .input(|input| { input.pointer.button_down(egui::PointerButton::Primary) })
            );
            assert!(
                native_gesture_commands(&commands).is_empty(),
                "wrong source for {expected:?}: {commands:?}"
            );
            for button in [egui::PointerButton::Secondary, egui::PointerButton::Primary] {
                commands.extend(ui.frame(vec![chrome_button(moved, button, false)]));
            }
            assert!(
                native_gesture_commands(&commands).is_empty(),
                "release submitted {expected:?}: {commands:?}"
            );
            assert!(ui.chrome.primary_origin.is_none());

            // Releasing a rejected gesture must leave the next genuine left-button
            // press free to initiate exactly one native operation in the same region.
            let commands = ui.drag(target);
            assert_eq!(native_gesture_commands(&commands), vec![&expected]);
            assert!(ui.chrome.primary_origin.is_none());
            let released = ui.frame(vec![chrome_button(
                moved,
                egui::PointerButton::Primary,
                false,
            )]);
            assert!(native_gesture_commands(&released).is_empty());
            assert!(ui.chrome.primary_origin.is_none());
        }
    }

    #[test]
    fn secondary_only_never_starts_title_drag_or_any_resize_handle() {
        for (target, expected) in chrome_drag_targets() {
            let mut ui = Harness::new(false);
            let mut commands = ui.frame(vec![egui::Event::PointerMoved(target)]);
            commands.extend(ui.frame(vec![chrome_button(
                target,
                egui::PointerButton::Secondary,
                true,
            )]));
            let moved = target + Vec2::splat(15.0);
            commands.extend(ui.frame(vec![egui::Event::PointerMoved(moved)]));
            commands.extend(ui.frame(vec![chrome_button(
                moved,
                egui::PointerButton::Secondary,
                false,
            )]));
            assert!(
                native_gesture_commands(&commands).is_empty(),
                "secondary started {expected:?}: {commands:?}"
            );
            assert!(ui.chrome.primary_origin.is_none());
        }
    }

    #[test]
    fn escape_cancels_pending_title_drag_and_next_primary_press_works() {
        let mut ui = Harness::new(false);
        let title = Pos2::new(90.0, 18.0);
        let mut commands = ui.frame(vec![egui::Event::PointerMoved(title)]);
        commands.extend(ui.frame(vec![chrome_button(
            title,
            egui::PointerButton::Primary,
            true,
        )]));
        assert!(native_gesture_commands(&commands).is_empty());
        assert!(ui.chrome.primary_origin.is_some());
        commands.extend(ui.frame(vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]));
        assert!(ui.chrome.primary_origin.is_none());
        let moved = title + Vec2::splat(15.0);
        commands.extend(ui.frame(vec![egui::Event::PointerMoved(moved)]));
        commands.extend(ui.frame(vec![chrome_button(
            moved,
            egui::PointerButton::Primary,
            false,
        )]));
        commands.extend(ui.frame(vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]));
        assert!(
            native_gesture_commands(&commands).is_empty(),
            "cancelled title drag sent a native command: {commands:?}"
        );
        assert!(ui.chrome.primary_origin.is_none());
        let commands = ui.drag(title);
        assert_eq!(
            native_gesture_commands(&commands),
            vec![&ViewportCommand::StartDrag]
        );
        let released = ui.frame(vec![chrome_button(
            moved,
            egui::PointerButton::Primary,
            false,
        )]);
        assert!(native_gesture_commands(&released).is_empty());
        assert!(ui.chrome.primary_origin.is_none());
    }
    #[test]
    fn losing_focus_cancels_pending_title_drag_and_next_primary_press_works() {
        let mut ui = Harness::new(false);
        let title = Pos2::new(90.0, 18.0);
        let mut commands = ui.frame(vec![egui::Event::PointerMoved(title)]);
        commands.extend(ui.frame(vec![chrome_button(
            title,
            egui::PointerButton::Primary,
            true,
        )]));
        assert!(native_gesture_commands(&commands).is_empty());
        assert!(ui.chrome.primary_origin.is_some());

        // egui-winit updates both RawInput.focused and the focus event. Keep the
        // button physically held here so only focus cancellation invalidates it.
        ui.focused = false;
        commands.extend(ui.frame(vec![egui::Event::WindowFocused(false)]));
        assert!(ui.chrome.primary_origin.is_none());
        ui.focused = true;
        commands.extend(ui.frame(vec![egui::Event::WindowFocused(true)]));
        let moved = title + Vec2::splat(15.0);
        commands.extend(ui.frame(vec![egui::Event::PointerMoved(moved)]));
        commands.extend(ui.frame(vec![chrome_button(
            moved,
            egui::PointerButton::Primary,
            false,
        )]));
        assert!(
            native_gesture_commands(&commands).is_empty(),
            "focus-cancelled title drag sent a native command: {commands:?}"
        );
        assert!(ui.chrome.primary_origin.is_none());

        let commands = ui.drag(title);
        assert_eq!(
            native_gesture_commands(&commands),
            vec![&ViewportCommand::StartDrag]
        );
        let released = ui.frame(vec![chrome_button(
            moved,
            egui::PointerButton::Primary,
            false,
        )]);
        assert!(native_gesture_commands(&released).is_empty());
        assert!(ui.chrome.primary_origin.is_none());
    }

    #[test]
    fn releasing_pending_title_press_clears_origin_and_next_primary_drag_works() {
        let mut ui = Harness::new(false);
        let title = Pos2::new(90.0, 18.0);
        let mut commands = ui.frame(vec![egui::Event::PointerMoved(title)]);
        commands.extend(ui.frame(vec![chrome_button(
            title,
            egui::PointerButton::Primary,
            true,
        )]));
        assert!(native_gesture_commands(&commands).is_empty());
        assert!(ui.chrome.primary_origin.is_some());

        // Release without moving far enough to start a native drag. The pending
        // source must still be cleared instead of waiting for an active handoff.
        commands.extend(ui.frame(vec![chrome_button(
            title,
            egui::PointerButton::Primary,
            false,
        )]));
        assert!(native_gesture_commands(&commands).is_empty());
        assert!(ui.chrome.primary_origin.is_none());

        let commands = ui.drag(title);
        assert_eq!(
            native_gesture_commands(&commands),
            vec![&ViewportCommand::StartDrag]
        );
        let released = ui.frame(vec![chrome_button(
            title + Vec2::splat(15.0),
            egui::PointerButton::Primary,
            false,
        )]);
        assert!(native_gesture_commands(&released).is_empty());
        assert!(ui.chrome.primary_origin.is_none());
    }
}
