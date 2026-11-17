use bnauth_app::{Application, CommitOutcome, Repository};
use bnauth_core::{
    ImportPreview, VaultSnapshot,
    clock::{ClockStatus, RuntimeClock, SyncSample},
};
use eframe::egui::{Context, Pos2, Rect, Vec2, ViewportCommand};
use std::{path::PathBuf, sync::mpsc::Receiver};
use zeroize::Zeroize;

// Full client size, including the custom title bar, in logical points.
pub const DEFAULT_WINDOW_WIDTH: u32 = 400;
pub const DEFAULT_WINDOW_HEIGHT: u32 = 860 + crate::window_chrome::TITLE_BAR_HEIGHT as u32;
pub const MIN_WINDOW_WIDTH: u32 = 320;
pub const MIN_WINDOW_HEIGHT: u32 = 420 + crate::window_chrome::TITLE_BAR_HEIGHT as u32;
pub const MAX_WINDOW_WIDTH: u32 = 2160;
pub const MAX_WINDOW_HEIGHT: u32 = 3840;

struct DiskRepository(bnauth_adapters::storage::Repository);
impl Repository for DiskRepository {
    fn state(&self) -> bnauth_app::RepositoryState {
        bnauth_app::RepositoryState {
            cleanup_required: self.0.cleanup_required(),
            blocked: self.0.is_blocked(),
        }
    }

    fn commit(&mut self, candidate: &bnauth_core::VaultSnapshot) -> Result<CommitOutcome, String> {
        self.0.commit(candidate).map(|r| CommitOutcome {
            snapshot: r.snapshot,
            warning: r.warning,
        })
    }
    fn retry_cleanup(&mut self) -> Result<(), String> {
        self.0.retry_cleanup()
    }
    fn restore(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        self.0.restore(candidate).map(|r| CommitOutcome {
            snapshot: r.snapshot,
            warning: r.warning,
        })
    }
    fn last_restore_backup(&self) -> Option<PathBuf> {
        self.0.restore_backup().map(|path| path.to_path_buf())
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct WindowGeometry {
    width: u32,
    height: u32,
    position: Pos2,
}

pub fn clock_status_tooltip(clock: &RuntimeClock, syncing: bool) -> String {
    let mut text = String::from("所有驗證器共用校時。\n");
    if syncing {
        text.push_str("正在校時…\n");
    }
    text.push_str(&clock.status_text());
    if let Some(error) = clock.last_error() {
        text.push_str("\n最近校時錯誤：");
        text.push_str(error);
    }
    text
}

#[derive(Default)]
pub struct ClockStatusNotices {
    previous: Option<ClockStatus>,
    reported_during_sync: bool,
}

impl ClockStatusNotices {
    /// A new explicit or import-triggered round must report its own failure.
    pub fn begin_sync(&mut self) {
        self.reported_during_sync = false;
    }

    /// Call after sampling clock.now_ms(), which can invalidate a previous anchor.
    /// Coalesce a rejected sample with a warning for the same discontinuity,
    /// including when the clock change was observed on an earlier frame.
    pub fn observe(
        &mut self,
        status: ClockStatus,
        specific_sync_rejection: Option<String>,
    ) -> Option<String> {
        let newly_discontinuous = status == ClockStatus::Discontinuous
            && self.previous != Some(ClockStatus::Discontinuous);
        self.previous = Some(status);
        if let Some(error) = specific_sync_rejection {
            if self.reported_during_sync {
                return None;
            }
            self.reported_during_sync = true;
            return Some(error);
        }
        if newly_discontinuous {
            self.reported_during_sync = true;
            Some("時間不連續，已改用本機時間；請手動校時。".to_owned())
        } else {
            None
        }
    }
}

/// Fit the fixed launch size inside the primary monitor's work area.
/// Coordinates and sizes here are egui points, not native physical pixels.
fn fixed_launch_geometry(work: Rect, decoration: Vec2) -> Option<WindowGeometry> {
    if !work.min.is_finite() || !work.max.is_finite() || !decoration.is_finite() {
        return None;
    }
    let work = Rect::from_min_max(
        Pos2::new(work.left().ceil(), work.top().ceil()),
        Pos2::new(work.right().floor(), work.bottom().floor()),
    );
    let decoration = decoration.max(Vec2::ZERO);
    let available = work.size() - decoration;
    if available.x < 1.0 || available.y < 1.0 {
        return None;
    }
    let width = DEFAULT_WINDOW_WIDTH.min(available.x.floor() as u32);
    let height = DEFAULT_WINDOW_HEIGHT.min(available.y.floor() as u32);
    let outer_size = Vec2::new(width as f32, height as f32) + decoration;
    let minimum = Pos2::new(work.left().ceil(), work.top().ceil());
    let maximum = Pos2::new(
        (work.right() - outer_size.x).floor(),
        (work.bottom() - outer_size.y).floor(),
    );
    if maximum.x < minimum.x || maximum.y < minimum.y {
        return None;
    }
    let origin = Pos2::new(work.left() + work.width() * 0.75, work.center().y) - outer_size * 0.5;
    Some(WindowGeometry {
        width,
        height,
        position: origin.clamp(minimum, maximum),
    })
}

fn decoration_size(viewport: &eframe::egui::ViewportInfo) -> Vec2 {
    viewport
        .outer_rect
        .zip(viewport.inner_rect)
        .map(|(outer, inner)| (outer.size() - inner.size()).max(Vec2::ZERO))
        // Native decorations are disabled; the client size includes our title bar.
        .unwrap_or(Vec2::ZERO)
}

/// Native bounds are stored in pixels, so refresh them only when the UI scale changes.
pub fn apply_window_size_limits(ctx: &Context) {
    let viewport = ctx.input(|input| input.viewport().clone());
    let maximum = Vec2::new(MAX_WINDOW_WIDTH as f32, MAX_WINDOW_HEIGHT as f32);
    let mut minimum = Vec2::new(MIN_WINDOW_WIDTH as f32, MIN_WINDOW_HEIGHT as f32);
    if let Some(outer) = viewport.outer_rect
        && let Some(work) = monitor_work_area(outer, ctx.pixels_per_point())
    {
        minimum = minimum.min((work.size() - decoration_size(&viewport)).max(Vec2::splat(1.0)));
    }
    ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(minimum));
    ctx.send_viewport_cmd(ViewportCommand::MaxInnerSize(maximum));
    if viewport.maximized != Some(true)
        && viewport.minimized != Some(true)
        && let Some(inner) = viewport
            .inner_rect
            .filter(|rect| rect.is_finite() && rect.is_positive())
        && (inner.width() > maximum.x || inner.height() > maximum.y)
    {
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(inner.size().min(maximum)));
    }
}

pub fn initialize_window_geometry(cc: &eframe::CreationContext<'_>) {
    // AppCreator runs while the native window is hidden. Viewport commands are
    // applied after the first ShowWindow, so set the native bounds directly here.
    let Some(window) = cc.winit_window() else {
        return;
    };
    let pixels_per_point = window.scale_factor() as f32 * cc.egui_ctx.zoom_factor();
    let inner = window.inner_size();
    let outer = window.outer_size();
    let decoration = Vec2::new(
        outer.width.saturating_sub(inner.width) as f32,
        outer.height.saturating_sub(inner.height) as f32,
    ) / pixels_per_point;
    let Some(work) = primary_monitor_work_area(pixels_per_point) else {
        return;
    };
    let Some(geometry) = fixed_launch_geometry(work, decoration) else {
        return;
    };
    let Ok(mut position) = window.outer_position() else {
        return;
    };
    // Lower the native minimum first if the work area cannot fit the usual one.
    let mut minimum = inner;
    minimum.width = (geometry.width.min(MIN_WINDOW_WIDTH) as f32 * pixels_per_point).round() as u32;
    minimum.height =
        (geometry.height.min(MIN_WINDOW_HEIGHT) as f32 * pixels_per_point).round() as u32;
    window.set_min_inner_size(Some(minimum));
    let mut size = inner;
    size.width = (geometry.width as f32 * pixels_per_point).round() as u32;
    size.height = (geometry.height as f32 * pixels_per_point).round() as u32;
    let _ = window.request_inner_size(size);
    position.x = (geometry.position.x * pixels_per_point).round() as i32;
    position.y = (geometry.position.y * pixels_per_point).round() as i32;
    window.set_outer_position(position);
}

#[cfg(any(windows, test))]
fn work_area_in_points(bounds: [i32; 4], pixels_per_point: f32) -> Option<Rect> {
    if !pixels_per_point.is_finite()
        || pixels_per_point <= 0.0
        || bounds[2] <= bounds[0]
        || bounds[3] <= bounds[1]
    {
        return None;
    }
    Some(Rect::from_min_max(
        Pos2::new(
            bounds[0] as f32 / pixels_per_point,
            bounds[1] as f32 / pixels_per_point,
        ),
        Pos2::new(
            bounds[2] as f32 / pixels_per_point,
            bounds[3] as f32 / pixels_per_point,
        ),
    ))
}

#[cfg(windows)]
fn monitor_work_area(rect: Rect, pixels_per_point: f32) -> Option<Rect> {
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromRect},
    };
    if !pixels_per_point.is_finite()
        || pixels_per_point <= 0.0
        || !rect.min.is_finite()
        || !rect.max.is_finite()
    {
        return None;
    }
    // egui-winit 0.37 sends OuterPosition in physical pixels using this same scale.
    let target = RECT {
        left: (rect.left() * pixels_per_point).floor() as i32,
        top: (rect.top() * pixels_per_point).floor() as i32,
        right: (rect.right() * pixels_per_point).ceil() as i32,
        bottom: (rect.bottom() * pixels_per_point).ceil() as i32,
    };
    // SAFETY: this synchronous User32 query does not retain the stack RECT.
    let monitor = unsafe { MonitorFromRect(&target, MONITOR_DEFAULTTONEAREST) };
    monitor_work_area_from_handle(monitor, pixels_per_point)
}

#[cfg(windows)]
fn primary_monitor_work_area(pixels_per_point: f32) -> Option<Rect> {
    use windows_sys::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint},
    };
    // The Windows primary monitor contains desktop origin (0, 0).
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    monitor_work_area_from_handle(monitor, pixels_per_point)
}

#[cfg(windows)]
fn monitor_work_area_from_handle(
    monitor: windows_sys::Win32::Graphics::Gdi::HMONITOR,
    pixels_per_point: f32,
) -> Option<Rect> {
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    if monitor.is_null() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: the borrowed HMONITOR and stack MONITORINFO are used synchronously.
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return None;
    }
    work_area_in_points(
        [
            info.rcWork.left,
            info.rcWork.top,
            info.rcWork.right,
            info.rcWork.bottom,
        ],
        pixels_per_point,
    )
}

#[cfg(not(windows))]
fn monitor_work_area(_rect: Rect, _pixels_per_point: f32) -> Option<Rect> {
    None
}

#[cfg(not(windows))]
fn primary_monitor_work_area(_pixels_per_point: f32) -> Option<Rect> {
    None
}

pub struct RuntimeState {
    pub app: Application,
    pub path: PathBuf,
    pub clock: RuntimeClock,
    pub frame_time: i64,
    pub sync_rx: Option<Receiver<Result<SyncSample, bnauth_adapters::time_sync::SyncFailure>>>,
    pub sync_manual: bool,
    pub export_rx: Option<Receiver<Result<Option<PathBuf>, String>>>,
    pub group_import_rx: Option<Receiver<Result<Option<GroupImportFile>, String>>>,
    pub pending_import_group: Option<uuid::Uuid>,
    pub full_restore_rx:
        Option<Receiver<Result<Option<crate::app::full_restore::RestoreFile>, String>>>,
    pub pending_full_restore: bool,
    pub pending_clock_record: Option<bnauth_core::TimeSyncRecord>,
    pub clock_save_inflight: Option<bnauth_core::TimeSyncRecord>,
    pub pending_clock_invalidation: bool,
    pub clock_invalidation_inflight: bool,
    pub invalidated_clock_records: Vec<bnauth_core::TimeSyncRecord>,
    pub pending_clock_manual: bool,
    pub clock_save_manual: bool,
    pub close_requested: bool,
    pub allow_close_with_residue: bool,
    pub clock_notices: ClockStatusNotices,
}

/// Storage values associated with the old clock before a load/restore ACK can
/// replace Application.current or remove the in-flight calibration marker.
pub(crate) struct ClockStorageContext {
    saved: Option<bnauth_core::TimeSyncRecord>,
    inflight: Option<bnauth_core::TimeSyncRecord>,
}

impl RuntimeState {
    pub(crate) fn clock_storage_context(&self) -> ClockStorageContext {
        ClockStorageContext {
            saved: self
                .app
                .current
                .as_ref()
                .and_then(|snapshot| snapshot.service_time_sync.battlenet.clone()),
            inflight: self.clock_save_inflight.clone(),
        }
    }

    /// A wall-clock step invalidates every calibration still able to become the
    /// saved value, not just the newest live sample. Keep these tombstones for
    /// this process so restoring an old backup cannot make them valid again.
    /// A fresh apply can first discover the old clock step while already leaving
    /// a valid new live sample; that sample and its pending save must survive.
    #[cfg(test)]
    pub fn note_clock_invalidation(&mut self, preserve_fresh_sample: bool) {
        let context = self.clock_storage_context();
        self.note_clock_invalidation_from(preserve_fresh_sample, &context);
    }

    pub(crate) fn note_clock_invalidation_from(
        &mut self,
        preserve_fresh_sample: bool,
        context: &ClockStorageContext,
    ) {
        let records = [
            context.saved.clone(),
            if preserve_fresh_sample {
                None
            } else {
                self.clock.last_success().cloned()
            },
            context.inflight.clone(),
            if preserve_fresh_sample {
                None
            } else {
                self.pending_clock_record.clone()
            },
        ];
        for record in records.into_iter().flatten() {
            if !self.invalidated_clock_records.contains(&record) {
                self.invalidated_clock_records.push(record);
            }
        }
        if !preserve_fresh_sample {
            self.pending_clock_record = None;
            if self.app.phase == bnauth_app::Phase::Ready {
                self.pending_clock_invalidation = true;
            }
            self.pending_clock_manual = false;
            self.clock_save_manual = false;
        }
    }

    pub fn close_blocked(&self) -> bool {
        self.app.saving
            || self.app.cleaning
            || self.export_rx.is_some()
            || self.group_import_rx.is_some()
            || self.full_restore_rx.is_some()
            || ((self.pending_clock_record.is_some() || self.pending_clock_invalidation)
                && !self.allow_close_with_residue)
            || self.app.has_pending()
            || ((self.app.warning.is_some() || self.app.repository_state.needs_attention())
                && !self.allow_close_with_residue)
    }

    pub fn new() -> Self {
        let location = std::env::current_exe()
            .map_err(|_| "無法取得程式所在目錄。".to_owned())
            .and_then(|exe| {
                exe.parent()
                    .map(|p| p.join("BNAuth.bnauth"))
                    .ok_or("無法取得資料檔位置。".to_owned())
            });
        let path = location.clone().unwrap_or_default();
        let app = Application::start(Box::new(move || {
            let path = location.clone()?;
            let (repository, snapshot) = bnauth_adapters::storage::Repository::open(path)?;
            Ok((Box::new(DiskRepository(repository)), snapshot))
        }));
        Self {
            app,
            path,
            clock: RuntimeClock::new(),
            frame_time: bnauth_core::now_ms(),
            sync_rx: None,
            sync_manual: false,
            export_rx: None,
            group_import_rx: None,
            pending_import_group: None,
            full_restore_rx: None,
            pending_full_restore: false,
            pending_clock_record: None,
            clock_save_inflight: None,
            pending_clock_invalidation: false,
            clock_invalidation_inflight: false,
            invalidated_clock_records: Vec::new(),
            pending_clock_manual: false,
            clock_save_manual: false,
            close_requested: false,
            allow_close_with_residue: false,
            clock_notices: ClockStatusNotices::default(),
        }
    }
}
pub struct GroupImportFile {
    pub path: PathBuf,
    pub group_name: String,
    pub source_count: usize,
    pub preview: Result<bnauth_core::GroupImportPreview, String>,
}

#[derive(Default)]
pub struct GroupImportForm {
    pub loaded: Option<GroupImportFile>,
    pub error: String,
}

pub struct ImportForm {
    pub name: String,
    pub description: String,
    pub secret: String,
    pub serial: String,
    pub restore: String,
    pub group: u64,
    pub error: String,
    pub preview: Option<ImportPreview>,
    pub inferred_serial: Option<String>,
}
impl ImportForm {
    pub fn new(group: u64) -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            secret: String::new(),
            serial: String::new(),
            restore: String::new(),
            group,
            error: String::new(),
            preview: None,
            inferred_serial: None,
        }
    }

    pub fn prepare_preview(
        &mut self,
        vault: &VaultSnapshot,
        group: uuid::Uuid,
        now_ms: i64,
    ) -> Result<(), String> {
        // Drop a stale candidate before parsing so an error can never confirm it.
        self.preview = None;
        let material = bnauth_core::parse_manual(&self.secret, &self.serial, &self.restore)?;
        if self.name.trim().is_empty()
            && let Some(serial) = &material.serial
        {
            self.name = bnauth_core::display_serial(serial);
        }
        self.preview = Some(bnauth_core::prepare_import(
            vault,
            &self.name,
            &self.description,
            group,
            material,
            now_ms,
        )?);
        self.error.clear();
        Ok(())
    }
}
impl Drop for ImportForm {
    fn drop(&mut self) {
        self.secret.zeroize();
        self.restore.zeroize();
    }
}

#[cfg(test)]
#[path = "runtime_state_tests.rs"]
mod state_tests;

#[cfg(test)]
#[path = "clock_notice_tests.rs"]
mod clock_notice_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_fits(geometry: WindowGeometry, work: Rect, decoration: Vec2) {
        let outer = Rect::from_min_size(
            geometry.position,
            Vec2::new(geometry.width as f32, geometry.height as f32) + decoration,
        );
        assert!(work.contains_rect(outer), "outer={outer:?}, work={work:?}");
    }

    #[test]
    fn every_launch_centers_the_default_size_in_the_primary_work_area_right_half() {
        for (bounds, expected) in [
            ([0.0, 0.0, 1920.0, 1040.0], Pos2::new(1240.0, 74.0)),
            ([48.0, 40.0, 1968.0, 1080.0], Pos2::new(1288.0, 114.0)),
            ([-1920.0, 0.0, 0.0, 1040.0], Pos2::new(-680.0, 74.0)),
            ([-1700.0, -400.0, 220.0, 640.0], Pos2::new(-460.0, -326.0)),
        ] {
            let work = Rect::from_min_max(
                Pos2::new(bounds[0], bounds[1]),
                Pos2::new(bounds[2], bounds[3]),
            );
            let geometry = fixed_launch_geometry(work, Vec2::ZERO).unwrap();
            assert_eq!((geometry.width, geometry.height), (400, 892));
            assert_eq!(geometry.position, expected);
            assert_fits(geometry, work, Vec2::ZERO);
        }
    }

    #[test]
    fn launch_center_uses_the_primary_work_area_at_fractional_and_integer_dpi() {
        for pixels_per_point in [1.0, 1.25, 1.5, 1.75, 2.0, 3.0] {
            let work = work_area_in_points([0, 0, 7680, 4320], pixels_per_point).unwrap();
            let geometry = fixed_launch_geometry(work, Vec2::ZERO).unwrap();
            assert_eq!((geometry.width, geometry.height), (400, 892));
            let center = (geometry.position
                + Vec2::new(geometry.width as f32, geometry.height as f32) * 0.5)
                * pixels_per_point;
            assert!((center.x - 5760.0).abs() <= pixels_per_point);
            assert!((center.y - 2160.0).abs() <= pixels_per_point);
            assert_fits(geometry, work, Vec2::ZERO);
        }
    }

    #[test]
    fn frameless_launch_does_not_invent_unknown_native_decoration() {
        assert_eq!(
            decoration_size(&eframe::egui::ViewportInfo::default()),
            Vec2::ZERO
        );
    }

    #[test]
    fn launch_fit_keeps_the_complete_native_window_inside_the_work_area() {
        let decoration = Vec2::new(16.0, 40.0);
        for (work_size, expected, position) in [
            (Vec2::new(360.0, 800.0), (344, 760), Pos2::new(0.0, 48.0)),
            (Vec2::new(360.0, 1040.0), (344, 892), Pos2::new(0.0, 102.0)),
            (
                Vec2::new(1920.0, 800.0),
                (400, 760),
                Pos2::new(1232.0, 48.0),
            ),
        ] {
            let work = Rect::from_min_size(Pos2::new(0.0, 48.0), work_size);
            let fitted = fixed_launch_geometry(work, decoration).unwrap();
            assert_eq!((fitted.width, fitted.height), expected);
            assert_eq!(fitted.position, position);
            assert_fits(fitted, work, decoration);
        }
    }

    #[test]
    fn launch_fit_accounts_for_native_decoration_and_work_area_conversion() {
        let work = work_area_in_points([3840, 0, 7680, 2080], 2.0).unwrap();
        assert_eq!(work.min, Pos2::new(1920.0, 0.0));
        let decoration = Vec2::new(8.0, 20.0);
        let fitted = fixed_launch_geometry(work, decoration).unwrap();
        assert_eq!(fitted.position, Pos2::new(3156.0, 64.0));
        assert_fits(fitted, work, decoration);
        assert!(work_area_in_points([0, 0, 1920, 1080], 0.0).is_none());
        assert!(work_area_in_points([10, 0, 0, 1080], 1.0).is_none());
    }

    #[test]
    fn small_fractional_work_area_can_lower_normal_minimum_without_hiding_chrome() {
        let work = work_area_in_points([-1919, 37, -1520, 480], 1.5).unwrap();
        let decoration = Vec2::new(10.0, 26.0);
        let fitted = fixed_launch_geometry(work, decoration).unwrap();
        assert!(fitted.width < 320 && fitted.height < 420);
        assert_fits(fitted, work, decoration);
    }
}
