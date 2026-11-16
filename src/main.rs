#![cfg_attr(not(test), windows_subsystem = "windows")]

mod app;
mod model;
mod notifications;
mod otp_cache;
mod runtime;
#[cfg(test)]
mod test_support;
mod window_chrome;
use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        centered: true,
        viewport: egui::ViewportBuilder::default()
            .with_title("BNAuth")
            .with_decorations(false)
            .with_maximize_button(false)
            .with_inner_size([
                runtime::DEFAULT_WINDOW_WIDTH as f32,
                runtime::DEFAULT_WINDOW_HEIGHT as f32,
            ])
            .with_clamp_size_to_monitor_size(true)
            .with_max_inner_size([
                runtime::MAX_WINDOW_WIDTH as f32,
                runtime::MAX_WINDOW_HEIGHT as f32,
            ])
            .with_min_inner_size([
                runtime::MIN_WINDOW_WIDTH as f32,
                runtime::MIN_WINDOW_HEIGHT as f32,
            ]),
        ..Default::default()
    };
    eframe::run_native(
        "BNAuth",
        options,
        Box::new(|cc| {
            runtime::initialize_window_geometry(cc);
            Ok(Box::new(app::BnAuthApp::new(cc)))
        }),
    )
}
