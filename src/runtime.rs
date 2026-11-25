use crate::otp_cache::PreviewCode;
use bnauth_app::{Application, CommitOutcome, Repository};
use bnauth_core::{
    ImportPreview, TimeSyncRecord, VaultSnapshot,
    clock::{ClockStatus, RuntimeClock, SyncSample},
};
use eframe::egui::{
    Color32, Context, FontSelection, Pos2, Rect, Style, Vec2, ViewportCommand,
    text::{LayoutJob, TextFormat},
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc::Receiver},
};
use zeroize::Zeroize;

// Full client size, including the custom title bar, in logical points.
pub const DEFAULT_WINDOW_WIDTH: u32 = 400;
pub const DEFAULT_WINDOW_HEIGHT: u32 = 860 + crate::window_chrome::TITLE_BAR_HEIGHT as u32;
pub const MIN_WINDOW_WIDTH: u32 = 400;
pub const MIN_WINDOW_HEIGHT: u32 = 540 + crate::window_chrome::TITLE_BAR_HEIGHT as u32;
pub const MAX_WINDOW_WIDTH: u32 = 2160;
pub const MAX_WINDOW_HEIGHT: u32 = 3840;

struct DiskRepository(bnauth_adapters::storage::Repository);
fn disk_outcome(saved: bnauth_adapters::storage::CommitOutcome) -> CommitOutcome {
    CommitOutcome {
        snapshot: saved.snapshot,
        warning: saved.warning,
    }
}

impl Repository for DiskRepository {
    fn state(&self) -> bnauth_app::RepositoryState {
        bnauth_app::RepositoryState {
            cleanup_required: self.0.cleanup_required(),
            blocked: self.0.is_blocked(),
            read_only: self.0.is_read_only(),
        }
    }

    fn commit(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        self.0.commit(candidate).map(disk_outcome)
    }
    fn commit_edit(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        self.0.commit_edit(candidate).map(disk_outcome)
    }
    fn retry_cleanup(&mut self) -> Result<(), String> {
        self.0.retry_cleanup()
    }
    fn defer_cleanup(&mut self) {
        self.0.defer_cleanup();
    }
    fn restore_from_preview(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        self.0.restore_from_preview(candidate).map(disk_outcome)
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct WindowGeometry {
    width: u32,
    height: u32,
    position: Pos2,
}

pub const SYNCING_NOTICE: &str = "正在校時";

pub fn clock_status_tooltip(
    clock: &RuntimeClock,
    syncing: bool,
    style: &Style,
    error_color: Color32,
) -> LayoutJob {
    let normal = TextFormat {
        font_id: FontSelection::Default.resolve(style),
        color: Color32::PLACEHOLDER,
        ..Default::default()
    };
    let mut text = LayoutJob::default();
    text.append("所有驗證器共用校時。\n", 0.0, normal.clone());
    if syncing {
        text.append(SYNCING_NOTICE, 0.0, normal.clone());
        text.append("\n", 0.0, normal.clone());
    }
    let color = if clock.status() == ClockStatus::Discontinuous || clock.last_error().is_some() {
        error_color
    } else {
        normal.color
    };
    text.append(&clock.status_text(), 0.0, TextFormat { color, ..normal });
    text
}

#[derive(Default)]
pub struct ClockStatusNotices {
    reported_during_sync: bool,
}

impl ClockStatusNotices {
    /// A new explicit or import-triggered round must report its own failure.
    pub fn begin_sync(&mut self) {
        self.reported_during_sync = false;
    }

    /// Coalesce a rejected sample with a warning for the same discontinuity,
    /// including when the clock change was observed on an earlier frame.
    pub fn should_report_discontinuity(
        &mut self,
        observed_jump: bool,
        sync_rejected: bool,
    ) -> bool {
        if observed_jump || (sync_rejected && !self.reported_during_sync) {
            self.reported_during_sync = true;
            true
        } else {
            false
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

pub enum ListEdit {
    EditEntry(usize),
    Copy(u64),
    Move(crate::model::GroupOrderEdit),
    Reorder(crate::model::GroupOrderEdit),
    Delete(crate::model::EntryDeletion),
}

pub enum PendingImport {
    FromKey,
    FromEmail,
    Group,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExportStage {
    ChoosePath,
    WriteFile,
}

pub struct ExportJob {
    pub stage: ExportStage,
    pub result: Receiver<Result<Option<PathBuf>, String>>,
}

/// Wake the event loop after the result is available, including a disconnected worker.
pub(crate) struct WorkerReply<T> {
    sender: Option<std::sync::mpsc::Sender<T>>,
    ctx: Context,
}

pub(crate) fn worker_channel<T>(ctx: &Context) -> (WorkerReply<T>, Receiver<T>) {
    let (sender, receiver) = std::sync::mpsc::channel();
    (
        WorkerReply {
            sender: Some(sender),
            ctx: ctx.clone(),
        },
        receiver,
    )
}

impl<T> WorkerReply<T> {
    pub(crate) fn send(mut self, result: T) {
        let _ = self.sender.take().unwrap().send(result);
    }
}

impl<T> Drop for WorkerReply<T> {
    fn drop(&mut self) {
        drop(self.sender.take());
        self.ctx.request_repaint();
    }
}

pub struct RuntimeState {
    pub ui_ctx: Context,
    pub app: Application,
    pub recovery: crate::app::email_recovery::Recovery,
    pub path: PathBuf,
    pub clock: RuntimeClock,
    pub frame_time: i64,
    pub sync_rx: Option<Receiver<Result<SyncSample, bnauth_adapters::time_sync::SyncFailure>>>,
    pub sync_manual: bool,
    #[cfg(test)]
    pub sync_requests: usize,
    pub export_job: Option<ExportJob>,
    pub export_cleanup: Arc<Mutex<bnauth_adapters::export_file::ExportCleanup>>,
    pub group_import_rx: Option<Receiver<Result<Option<GroupImportResponse>, String>>>,
    pub pending_list_edit: Option<ListEdit>,
    pub pending_groups: Option<Box<crate::model::GroupDraft>>,
    pub pending_import: Option<PendingImport>,
    pub full_restore_rx:
        Option<Receiver<Result<Option<crate::app::full_restore::RestoreFile>, String>>>,
    pub pending_full_restore: bool,
    pub pending_clock_record: Option<bnauth_core::TimeSyncRecord>,
    pub clock_save_inflight: Option<bnauth_core::TimeSyncRecord>,
    pub pending_clock_manual: bool,
    pub clock_save_manual: bool,
    pub pending_clock_merge: bool,
    pub close_requested: bool,
    pub save_after_cleanup: bool,
    pub clock_notices: ClockStatusNotices,
}

impl RuntimeState {
    pub fn commit_with_pending_clock(
        &mut self,
        candidate: VaultSnapshot,
        import: Option<PendingImport>,
    ) -> Result<(), String> {
        self.commit_prepared_with_pending_clock(import, || candidate)
    }

    pub fn commit_prepared_with_pending_clock(
        &mut self,
        import: Option<PendingImport>,
        take_candidate: impl FnOnce() -> VaultSnapshot,
    ) -> Result<(), String> {
        let record = self.pending_clock_record.clone();
        let mut queued = false;
        let result = self.app.commit_edit(import.is_some(), || {
            let mut candidate = take_candidate();
            if record.is_some() {
                candidate.service_time_sync.battlenet.clone_from(&record);
            }
            queued = true;
            candidate
        });
        if queued {
            self.pending_import = import;
            self.pending_clock_merge = true;
            self.clock_save_manual = record.is_some() && self.pending_clock_manual;
            self.clock_save_inflight = record;
        }
        result
    }

    pub fn retry_pending_save(&mut self) -> Result<(), String> {
        if self.app.repository_state.cleanup_required && self.app.has_pending() {
            self.app.retry_cleanup()?;
            self.save_after_cleanup = true;
            return Ok(());
        }
        let invalid_backup =
            self.pending_full_restore && Self::unusable_backup_clock(self.app.pending_time_sync());
        let merge_clock = self.pending_clock_merge || invalid_backup;
        let clock_only =
            !merge_clock && !self.pending_full_restore && self.clock_save_inflight.is_some();
        let record = if self.pending_full_restore && merge_clock {
            self.clock.last_success().cloned()
        } else if clock_only || merge_clock {
            self.pending_clock_record.clone()
        } else {
            None
        };

        if clock_only
            && self
                .app
                .current
                .as_ref()
                .is_some_and(|snapshot| snapshot.service_time_sync.battlenet == record)
        {
            self.app.discard();
            self.clear_pending_clock_save();
            return Ok(());
        }
        if !clock_only && record.is_none() && !invalid_backup {
            return self.app.retry();
        }

        self.app.retry_with_clock(record.clone())?;
        self.pending_clock_merge = merge_clock;
        self.clock_save_manual = record.is_some() && self.pending_clock_manual;
        self.clock_save_inflight = record;
        Ok(())
    }

    pub(crate) fn clear_pending_clock_save(&mut self) {
        self.pending_clock_record = None;
        self.clock_save_inflight = None;
        self.pending_clock_manual = false;
        self.clock_save_manual = false;
    }

    pub(crate) fn unusable_backup_clock(record: Option<&TimeSyncRecord>) -> bool {
        record.is_some_and(|record| {
            bnauth_core::clock::saved_time_ms(record, bnauth_core::now_ms()).is_none()
        })
    }

    pub fn file_job_in_progress(&self) -> bool {
        self.export_job.is_some()
            || self.group_import_rx.is_some()
            || self.full_restore_rx.is_some()
    }

    pub fn close_blocked(&self) -> bool {
        self.app.saving
            || self.app.cleaning
            || self.recovery.in_progress()
            || self.file_job_in_progress()
            || (self.pending_clock_record.is_some() && self.app.repository_state.allows_writes())
            || self.app.has_pending()
            || (self.app.warning.is_some() && !self.app.repository_state.blocked)
    }

    pub fn new(ui_ctx: Context) -> Self {
        let location = std::env::current_exe()
            .map_err(|_| "無法取得程式所在目錄。".to_owned())
            .and_then(|exe| {
                exe.parent()
                    .map(|p| p.join("BNAuth.bnauth"))
                    .ok_or("無法取得資料檔位置。".to_owned())
            });
        let path = location.clone().unwrap_or_default();
        let wake = ui_ctx.clone();
        let app = Application::start_with_notify(
            Box::new(move || {
                let path = location.clone()?;
                let (repository, snapshot) = bnauth_adapters::storage::Repository::open(path)?;
                Ok((Box::new(DiskRepository(repository)), snapshot))
            }),
            move || wake.request_repaint(),
        );
        Self {
            ui_ctx,
            app,
            recovery: crate::app::email_recovery::Recovery::default(),
            path,
            clock: RuntimeClock::new(),
            frame_time: bnauth_core::now_ms(),
            sync_rx: None,
            sync_manual: false,
            #[cfg(test)]
            sync_requests: 0,
            export_job: None,
            export_cleanup: Default::default(),
            group_import_rx: None,
            pending_list_edit: None,
            pending_groups: None,
            pending_import: None,
            full_restore_rx: None,
            pending_full_restore: false,
            pending_clock_record: None,
            clock_save_inflight: None,
            pending_clock_manual: false,
            clock_save_manual: false,
            pending_clock_merge: false,
            close_requested: false,
            save_after_cleanup: false,
            clock_notices: ClockStatusNotices::default(),
        }
    }
}
pub struct GroupImportFile {
    pub path: PathBuf,
    pub source_group: String,
    pub source_count: usize,
    pub preview: Option<Box<bnauth_core::GroupImportPreview>>,
    pub preview_code: PreviewCode,
}

pub enum GroupImportResponse {
    Selected {
        path: PathBuf,
        source: Box<bnauth_core::ValidatedBackup>,
    },
    Prepared(Box<bnauth_core::GroupImportPreview>),
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
    pub preview_code: PreviewCode,
    pub reveal_preview: bool,
    pub serial_is_embedded: bool,
    pub packed_key: Option<Result<bnauth_core::ManualMaterial, String>>,
    pub invalid_name: bool,
    pub invalid_secret: bool,
    pub invalid_serial: bool,
    pub invalid_restore: bool,
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
            preview_code: PreviewCode::default(),
            reveal_preview: false,
            serial_is_embedded: false,
            packed_key: None,
            invalid_name: false,
            invalid_secret: false,
            invalid_serial: false,
            invalid_restore: false,
        }
    }

    pub fn take_preview(&mut self) -> Option<ImportPreview> {
        self.preview_code = PreviewCode::default();
        self.reveal_preview = false;
        self.preview.take()
    }

    pub fn prepare_preview(
        &mut self,
        vault: &VaultSnapshot,
        group: impl Into<bnauth_core::ImportGroup>,
    ) -> Result<(), String> {
        // Drop a stale candidate before parsing so an error can never confirm it.
        drop(self.take_preview());
        let serial = if self.serial_is_embedded {
            Ok(None)
        } else {
            bnauth_core::submit_serial_input(&mut self.serial, false)
        };
        let restore = bnauth_core::submit_restore_input(&mut self.restore, false);
        self.invalid_serial = serial.is_err();
        self.invalid_restore = restore.is_err();
        let key = self
            .packed_key
            .clone()
            .unwrap_or_else(|| bnauth_core::ManualMaterial::from_key(&self.secret));
        self.invalid_secret = key.is_err();
        let serial = serial?;
        restore?;
        let material = key?.with_fields(serial, &self.restore)?;
        self.preview = Some(
            bnauth_core::prepare_import(vault, &self.name, &self.description, group, material)?
                .preview,
        );
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
}
