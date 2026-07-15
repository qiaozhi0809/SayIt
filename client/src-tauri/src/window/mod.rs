use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::commands::system::write_log_line;
use crate::context::{capture_foreground_monitor, MonitorBounds};

const OVERLAY_DEFAULT_BASE_WIDTH: f64 = 360.0;
const OVERLAY_BASE_HEIGHT: f64 = 56.0;
const OVERLAY_FALLBACK_WIDTH: f64 = 520.0;
const OVERLAY_FALLBACK_HEIGHT: f64 = 224.0;
const OVERLAY_SCREEN_MARGIN: f64 = 8.0;
const ACK_FIRST_TIMEOUT_MS: u64 = 1_200;
const ACK_SECOND_TIMEOUT_MS: u64 = 700;
const RECOVERY_ACK_TIMEOUT_MS: u64 = 2_000;

/// 旧版轻量 ping/pong 仍保留，便于和历史日志对照。
static LAST_PONG_MS: AtomicI64 = AtomicI64::new(0);
static SHOW_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn record_overlay_pong(_seq: u64) {
    LAST_PONG_MS.store(now_ms(), Ordering::SeqCst);
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, PartialEq)]
enum OverlayLayout {
    Base,
    Fallback,
}

pub struct WindowState {
    overlay_layout: Mutex<OverlayLayout>,
    overlay_base_width: Mutex<f64>,
    overlay_monitor: Mutex<Option<MonitorBounds>>,
    overlay_lifecycle: Mutex<()>,
    latest_overlay_payload: Mutex<Option<Value>>,
    active_show_id: AtomicU64,
    active_generation: AtomicU64,
    show_started_at_ms: AtomicI64,
    last_ack_show_id: AtomicU64,
    last_ack_generation: AtomicU64,
    last_ack_at_ms: AtomicI64,
    recovery_started_show_id: AtomicU64,
}
impl WindowState {
    pub fn new() -> Self {
        Self {
            overlay_layout: Mutex::new(OverlayLayout::Base),
            overlay_base_width: Mutex::new(OVERLAY_DEFAULT_BASE_WIDTH),
            overlay_monitor: Mutex::new(None),
            overlay_lifecycle: Mutex::new(()),
            latest_overlay_payload: Mutex::new(None),
            active_show_id: AtomicU64::new(0),
            active_generation: AtomicU64::new(0),
            show_started_at_ms: AtomicI64::new(0),
            last_ack_show_id: AtomicU64::new(0),
            last_ack_generation: AtomicU64::new(0),
            last_ack_at_ms: AtomicI64::new(0),
            recovery_started_show_id: AtomicU64::new(0),
        }
    }

    /// 原子地保存状态、显示原生窗口并请求 Overlay 确认本次渲染。
    pub fn present_overlay(&self, app: &AppHandle, data: Value) -> u64 {
        self.apply_payload_layout(&data);
        self.capture_overlay_monitor();
        *self.latest_overlay_payload.lock().unwrap() = Some(data);

        let show_id = SHOW_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
        self.active_show_id.store(show_id, Ordering::SeqCst);
        self.active_generation.store(0, Ordering::SeqCst);
        self.show_started_at_ms.store(now_ms(), Ordering::SeqCst);
        self.last_ack_show_id.store(0, Ordering::SeqCst);
        self.last_ack_generation.store(0, Ordering::SeqCst);
        self.last_ack_at_ms.store(0, Ordering::SeqCst);
        self.recovery_started_show_id.store(0, Ordering::SeqCst);

        let state_name = self.latest_state_name();
        write_log_line(&format!(
            "[overlay-health] present show_id={} state={} generation=0",
            show_id, state_name,
        ));

        if self.ensure_visible(app, show_id) {
            self.emit_latest(app, true);
        }
        spawn_render_watchdog(app.clone(), show_id);
        show_id
    }

    /// 兼容旧调用点；新代码应使用 present_overlay，将显示和状态更新合成一次操作。
    pub fn show_overlay(&self, app: &AppHandle) -> u64 {
        let data = self.latest_overlay_payload.lock().unwrap().clone()
            .unwrap_or_else(|| json!({ "state": "waiting", "elapsedSec": 0 }));
        self.present_overlay(app, data)
    }

    pub fn hide_overlay(&self, app: &AppHandle) {
        self.active_show_id.store(0, Ordering::SeqCst);
        *self.overlay_monitor.lock().unwrap() = None;

        let prev_layout = {
            let mut layout = self.overlay_layout.lock().unwrap();
            let previous = layout.clone();
            *layout = OverlayLayout::Base;
            previous
        };

        if let Some(overlay) = app.get_webview_window("overlay") {
            set_overlay_interactivity(&overlay, false);
            if let Err(error) = overlay.hide() {
                write_log_line(&format!(
                    "[overlay-diag] hide FAILED prev_layout={:?} hide_err={:?}",
                    prev_layout, error,
                ));
            }
        }
    }

    pub fn update_overlay_state(&self, app: &AppHandle, data: &Value) {
        self.apply_payload_layout(data);
        *self.latest_overlay_payload.lock().unwrap() = Some(data.clone());

        if let Some(overlay) = app.get_webview_window("overlay") {
            let is_fallback = self.overlay_layout.lock().unwrap().clone() == OverlayLayout::Fallback;
            self.apply_native_layout(app, &overlay, is_fallback);
            set_overlay_interactivity(&overlay, is_fallback);
            if is_fallback {
                let _ = overlay.show();
            }
            self.emit_latest(app, false);
        }
    }

    /// Overlay 页面注册完事件监听后调用。新建 WebView 时，第一次状态事件可能早于
    /// React listener，因此在 ready 时重放最近状态并重新请求确认。
    pub fn overlay_ready(&self, app: &AppHandle) {
        let show_id = self.active_show_id.load(Ordering::SeqCst);
        if show_id == 0 {
            if let Some(overlay) = app.get_webview_window("overlay") {
                let _ = overlay.hide();
            }
            return;
        }

        write_log_line(&format!(
            "[overlay-health] renderer ready show_id={} generation={}",
            show_id,
            self.active_generation.load(Ordering::SeqCst),
        ));
        if self.ensure_visible(app, show_id) {
            self.emit_latest(app, true);
        }
    }

    pub fn record_render_ack(&self, data: &Value) {
        let show_id = data.get("showId").and_then(Value::as_u64).unwrap_or(0);
        let generation = data.get("generation").and_then(Value::as_u64).unwrap_or(0);
        let healthy = data.get("healthy").and_then(Value::as_bool).unwrap_or(false);
        let active_show_id = self.active_show_id.load(Ordering::SeqCst);
        let active_generation = self.active_generation.load(Ordering::SeqCst);

        if show_id != active_show_id || generation != active_generation {
            write_log_line(&format!(
                "[overlay-health] stale ack show_id={} generation={} active_show_id={} active_generation={}",
                show_id, generation, active_show_id, active_generation,
            ));
            return;
        }

        if !healthy {
            write_log_line(&format!(
                "[overlay-health] unhealthy render ack show_id={} generation={} detail={}",
                show_id, generation, compact_json(data),
            ));
            return;
        }

        self.last_ack_show_id.store(show_id, Ordering::SeqCst);
        self.last_ack_generation.store(generation, Ordering::SeqCst);
        self.last_ack_at_ms.store(now_ms(), Ordering::SeqCst);
        write_log_line(&format!(
            "[overlay-health] render ack OK show_id={} generation={} latency_ms={} state={} content={}x{} visibility={}",
            show_id,
            generation,
            self.ack_latency_ms(),
            data.get("overlayState").and_then(Value::as_str).unwrap_or("unknown"),
            data.get("contentWidth").and_then(Value::as_f64).unwrap_or(0.0),
            data.get("contentHeight").and_then(Value::as_f64).unwrap_or(0.0),
            data.get("documentVisibility").and_then(Value::as_str).unwrap_or("unknown"),
        ));
    }

    pub fn health_snapshot(&self, app: &AppHandle, show_id: u64) -> Value {
        let active_show_id = self.active_show_id.load(Ordering::SeqCst);
        let active_generation = self.active_generation.load(Ordering::SeqCst);
        let ack_show_id = self.last_ack_show_id.load(Ordering::SeqCst);
        let ack_generation = self.last_ack_generation.load(Ordering::SeqCst);
        let recovery_started = self.recovery_started_show_id.load(Ordering::SeqCst) == show_id;
        json!({
            "showId": show_id,
            "activeShowId": active_show_id,
            "activeGeneration": active_generation,
            "acked": ack_show_id == show_id && ack_generation == active_generation,
            "ackGeneration": ack_generation,
            "ackLatencyMs": if ack_show_id == show_id { self.ack_latency_ms() } else { -1 },
            "recoveryStarted": recovery_started,
            "recoverySucceeded": recovery_started && ack_show_id == show_id && ack_generation > 0,
            "window": overlay_window_snapshot(app),
        })
    }

    fn ack_latency_ms(&self) -> i64 {
        let ack_at = self.last_ack_at_ms.load(Ordering::SeqCst);
        let started_at = self.show_started_at_ms.load(Ordering::SeqCst);
        if ack_at <= 0 || started_at <= 0 { -1 } else { ack_at - started_at }
    }

    fn is_acked(&self, show_id: u64, generation: u64) -> bool {
        self.last_ack_show_id.load(Ordering::SeqCst) == show_id
            && self.last_ack_generation.load(Ordering::SeqCst) == generation
    }

    fn is_active(&self, show_id: u64, generation: u64) -> bool {
        self.active_show_id.load(Ordering::SeqCst) == show_id
            && self.active_generation.load(Ordering::SeqCst) == generation
    }

    fn capture_overlay_monitor(&self) {
        let monitor = capture_foreground_monitor();
        if let Some(value) = &monitor {
            write_log_line(&format!(
                "[overlay-position] target_monitor source={} work=({},{},{},{})",
                value.source, value.left, value.top, value.right, value.bottom,
            ));
        }
        *self.overlay_monitor.lock().unwrap() = monitor;
    }

    fn apply_payload_layout(&self, data: &Value) {
        if let Some(width) = data.get("baseWidth").and_then(Value::as_f64) {
            if (160.0..=600.0).contains(&width) {
                *self.overlay_base_width.lock().unwrap() = width;
            }
        }

        let next_layout = if data.get("state").and_then(Value::as_str) == Some("fallback") {
            OverlayLayout::Fallback
        } else {
            OverlayLayout::Base
        };
        *self.overlay_layout.lock().unwrap() = next_layout;
    }

    fn latest_state_name(&self) -> String {
        self.latest_overlay_payload.lock().unwrap().as_ref()
            .and_then(|payload| payload.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string()
    }

    fn payload_for_emit(&self, probe: bool) -> Option<Value> {
        let mut payload = self.latest_overlay_payload.lock().unwrap().clone()?;
        let show_id = self.active_show_id.load(Ordering::SeqCst);
        let generation = self.active_generation.load(Ordering::SeqCst);
        let object = payload.as_object_mut()?;
        object.insert("_overlayShowId".to_string(), json!(show_id));
        object.insert("_overlayGeneration".to_string(), json!(generation));
        object.insert("_overlayProbe".to_string(), json!(probe));
        Some(payload)
    }

    fn emit_latest(&self, app: &AppHandle, probe: bool) {
        let Some(overlay) = app.get_webview_window("overlay") else { return; };
        let Some(payload) = self.payload_for_emit(probe) else { return; };
        if let Err(error) = overlay.emit("overlay-state", payload) {
            write_log_line(&format!(
                "[overlay-health] emit FAILED show_id={} generation={} error={:?}",
                self.active_show_id.load(Ordering::SeqCst),
                self.active_generation.load(Ordering::SeqCst),
                error,
            ));
        }
    }

    fn ensure_visible(&self, app: &AppHandle, show_id: u64) -> bool {
        // WebviewWindowBuilder 的 label 注册与 Manager 句柄可见性不是原子操作。
        // 多个 present 并发时若都看到 handle=MISSING，会争抢同一 "overlay" label。
        let _lifecycle_guard = self.overlay_lifecycle.lock().unwrap();
        let is_fallback = self.overlay_layout.lock().unwrap().clone() == OverlayLayout::Fallback;
        let base_width = *self.overlay_base_width.lock().unwrap();

        let Some(overlay) = app.get_webview_window("overlay") else {
            write_log_line(&format!(
                "[overlay-health] handle missing show_id={} — creating",
                show_id,
            ));
            self.create_overlay(app, is_fallback, base_width);
            return false;
        };

        let position_error = self.apply_native_layout(app, &overlay, is_fallback);
        let show_error = overlay.show().err().map(|error| format!("{:?}", error));
        let top_error = overlay.set_always_on_top(true).err().map(|error| format!("{:?}", error));
        set_overlay_interactivity(&overlay, is_fallback);

        let snapshot = overlay_window_snapshot(app);
        if position_error.is_some() || show_error.is_some() || top_error.is_some() {
            write_log_line(&format!(
                "[overlay-health] native show FAILED show_id={} position_error={:?} show_error={:?} top_error={:?} snapshot={}",
                show_id, position_error, show_error, top_error, compact_json(&snapshot),
            ));
        } else {
            write_log_line(&format!(
                "[overlay-health] native show OK show_id={} snapshot={}",
                show_id, compact_json(&snapshot),
            ));
        }
        true
    }

    fn apply_native_layout(
        &self,
        app: &AppHandle,
        overlay: &tauri::WebviewWindow,
        is_fallback: bool,
    ) -> Option<String> {
        let base_width = *self.overlay_base_width.lock().unwrap();
        let target_monitor = self.overlay_monitor.lock().unwrap().clone();
        if let Some(bounds) = target_monitor
            .as_ref()
            .and_then(|value| calc_monitor_overlay_bounds(app, value, is_fallback, base_width))
        {
            let position_error = overlay.set_position(tauri::Position::Physical(
                tauri::PhysicalPosition::new(bounds.0, bounds.1),
            )).err();
            let size_error = overlay.set_size(tauri::Size::Physical(
                tauri::PhysicalSize::new(bounds.2, bounds.3),
            )).err();
            let _ = overlay.set_always_on_top(true);

            return match (position_error, size_error) {
                (None, None) => None,
                (position, size) => Some(format!("position={:?} size={:?}", position, size)),
            };
        }

        let bounds = calc_overlay_bounds(app, is_fallback, base_width);
        let position_error = overlay.set_position(tauri::Position::Logical(
            tauri::LogicalPosition::new(bounds.0, bounds.1),
        )).err();
        let size_error = overlay.set_size(tauri::Size::Logical(
            tauri::LogicalSize::new(bounds.2, bounds.3),
        )).err();
        let _ = overlay.set_always_on_top(true);

        match (position_error, size_error) {
            (None, None) => None,
            (position, size) => Some(format!("position={:?} size={:?}", position, size)),
        }
    }

    fn recover_overlay(&self, app: &AppHandle, show_id: u64) {
        let _lifecycle_guard = self.overlay_lifecycle.lock().unwrap();
        // 等锁期间可能已经 hide 或开始了下一次显示，必须重新校验。
        if !self.is_active(show_id, 0) {
            return;
        }

        self.active_generation.store(1, Ordering::SeqCst);
        self.last_ack_show_id.store(0, Ordering::SeqCst);
        self.last_ack_generation.store(0, Ordering::SeqCst);
        self.recovery_started_show_id.store(show_id, Ordering::SeqCst);
        write_log_line(&format!(
            "[overlay-health] recovery begin show_id={} snapshot={}",
            show_id, compact_json(&overlay_window_snapshot(app)),
        ));

        if let Some(overlay) = app.get_webview_window("overlay") {
            if let Err(error) = overlay.destroy() {
                write_log_line(&format!(
                    "[overlay-health] destroy FAILED show_id={} error={:?}",
                    show_id, error,
                ));
            }
        }

        for _ in 0..20 {
            if app.get_webview_window("overlay").is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }

        if app.get_webview_window("overlay").is_some() {
            write_log_line(&format!(
                "[overlay-health] recovery FAILED show_id={} reason=stale_handle",
                show_id,
            ));
            return;
        }

        let is_fallback = self.overlay_layout.lock().unwrap().clone() == OverlayLayout::Fallback;
        let base_width = *self.overlay_base_width.lock().unwrap();
        self.create_overlay(app, is_fallback, base_width);
    }

    fn create_overlay(&self, app: &AppHandle, is_fallback: bool, base_width: f64) {
        let bounds = calc_overlay_bounds(app, is_fallback, base_width);
        let builder = WebviewWindowBuilder::new(
            app,
            "overlay",
            WebviewUrl::App("overlay.html".into()),
        )
        .title("SayIt Overlay")
        .inner_size(bounds.2, bounds.3)
        .position(bounds.0, bounds.1)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .visible(false);

        match builder.build() {
            Ok(overlay) => {
                let position_error = self.apply_native_layout(app, &overlay, is_fallback);
                set_overlay_interactivity(&overlay, is_fallback);
                let show_error = overlay.show().err().map(|error| format!("{:?}", error));
                write_log_line(&format!(
                    "[overlay-health] create OK show_id={} generation={} initial_bounds={:?} position_error={:?} show_error={:?} visible={}",
                    self.active_show_id.load(Ordering::SeqCst),
                    self.active_generation.load(Ordering::SeqCst),
                    bounds,
                    position_error,
                    show_error,
                    overlay.is_visible().unwrap_or(false),
                ));
            }
            Err(error) => {
                write_log_line(&format!(
                    "[overlay-health] create FAILED show_id={} generation={} bounds={:?} error={:?}",
                    self.active_show_id.load(Ordering::SeqCst),
                    self.active_generation.load(Ordering::SeqCst),
                    bounds,
                    error,
                ));
            }
        }
    }
}
fn spawn_render_watchdog(app: AppHandle, show_id: u64) {
    let _ = thread::Builder::new()
        .name(format!("overlay-watchdog-{}", show_id))
        .spawn(move || {
            thread::sleep(Duration::from_millis(ACK_FIRST_TIMEOUT_MS));
            {
                let state = app.state::<WindowState>();
                if !state.is_active(show_id, 0) || state.is_acked(show_id, 0) {
                    return;
                }
                write_log_line(&format!(
                    "[overlay-health] ack timeout phase=1 show_id={} snapshot={}",
                    show_id, compact_json(&overlay_window_snapshot(&app)),
                ));
                state.emit_latest(&app, true);
            }

            thread::sleep(Duration::from_millis(ACK_SECOND_TIMEOUT_MS));
            {
                let state = app.state::<WindowState>();
                if !state.is_active(show_id, 0) || state.is_acked(show_id, 0) {
                    return;
                }
                write_log_line(&format!(
                    "[overlay-health] ack timeout phase=2 show_id={} — confirmed unhealthy",
                    show_id,
                ));
                state.recover_overlay(&app, show_id);
            }

            thread::sleep(Duration::from_millis(RECOVERY_ACK_TIMEOUT_MS));
            let state = app.state::<WindowState>();
            if !state.is_active(show_id, 1) {
                return;
            }
            if state.is_acked(show_id, 1) {
                write_log_line(&format!(
                    "[overlay-health] recovery OK show_id={} latency_ms={} snapshot={}",
                    show_id,
                    state.ack_latency_ms(),
                    compact_json(&overlay_window_snapshot(&app)),
                ));
            } else {
                write_log_line(&format!(
                    "[overlay-health] recovery FAILED show_id={} reason=no_render_ack snapshot={}",
                    show_id,
                    compact_json(&overlay_window_snapshot(&app)),
                ));
            }
        });
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

fn overlay_window_snapshot(app: &AppHandle) -> Value {
    let Some(overlay) = app.get_webview_window("overlay") else {
        return json!({ "handleExists": false });
    };

    let visible = overlay.is_visible().ok();
    let position = overlay.outer_position().ok();
    let size = overlay.outer_size().ok();
    let monitors = app.available_monitors().unwrap_or_default();
    let primary_monitor = app.primary_monitor().ok().flatten();

    let intersects_any = match (&position, &size) {
        (Some(position), Some(size)) => monitors.iter().any(|monitor| {
            let monitor_position = monitor.position();
            let monitor_size = monitor.size();
            let window_right = position.x as i64 + size.width as i64;
            let window_bottom = position.y as i64 + size.height as i64;
            let monitor_right = monitor_position.x as i64 + monitor_size.width as i64;
            let monitor_bottom = monitor_position.y as i64 + monitor_size.height as i64;
            window_right > monitor_position.x as i64
                && (position.x as i64) < monitor_right
                && window_bottom > monitor_position.y as i64
                && (position.y as i64) < monitor_bottom
        }),
        _ => false,
    };

    let mut snapshot = Map::new();
    snapshot.insert("handleExists".to_string(), json!(true));
    snapshot.insert("visible".to_string(), json!(visible));
    snapshot.insert("intersectsAnyMonitor".to_string(), json!(intersects_any));
    snapshot.insert(
        "position".to_string(),
        position.map(|value| json!({ "x": value.x, "y": value.y })).unwrap_or(Value::Null),
    );
    snapshot.insert(
        "size".to_string(),
        size.map(|value| json!({ "width": value.width, "height": value.height })).unwrap_or(Value::Null),
    );
    if let Some(monitor) = primary_monitor {
        let position = monitor.position();
        let size = monitor.size();
        snapshot.insert("primaryMonitor".to_string(), json!({
            "x": position.x,
            "y": position.y,
            "width": size.width,
            "height": size.height,
            "scaleFactor": monitor.scale_factor(),
        }));
    }
    Value::Object(snapshot)
}

fn set_overlay_interactivity(overlay: &tauri::WebviewWindow, interactive: bool) {
    let _ = overlay.set_ignore_cursor_events(!interactive);
}

fn monitor_info(app: &AppHandle) -> (f64, f64, f64) {
    if let Some(monitor) = app.primary_monitor().ok().flatten() {
        let size = monitor.size();
        let scale = monitor.scale_factor();
        (size.width as f64 / scale, size.height as f64 / scale, scale)
    } else {
        (1920.0, 1080.0, 1.0)
    }
}

fn calc_overlay_bounds(app: &AppHandle, is_fallback: bool, base_width: f64) -> (f64, f64, f64, f64) {
    let (desired_width, desired_height) = if is_fallback {
        (OVERLAY_FALLBACK_WIDTH, OVERLAY_FALLBACK_HEIGHT)
    } else {
        (base_width, OVERLAY_BASE_HEIGHT)
    };
    let (screen_width, screen_height, _) = monitor_info(app);
    let width = desired_width.min(screen_width - 40.0).max(160.0);
    let height = desired_height.min(screen_height - 40.0).max(56.0);
    let x = ((screen_width - width) / 2.0).round();
    let y = (screen_height - height - 72.0).max(8.0);
    (x, y, width, height)
}

/// 在录音开始时的前台窗口所在显示器底部居中放置。
fn calc_monitor_overlay_bounds(
    app: &AppHandle,
    target: &MonitorBounds,
    is_fallback: bool,
    base_width: f64,
) -> Option<(i32, i32, u32, u32)> {
    let center_x = target.left as i64 + (target.right as i64 - target.left as i64) / 2;
    let center_y = target.top as i64 + (target.bottom as i64 - target.top as i64) / 2;
    let monitors = app.available_monitors().ok()?;
    let monitor = monitors.iter().find(|monitor| {
        let position = monitor.position();
        let size = monitor.size();
        center_x >= position.x as i64
            && center_x < position.x as i64 + size.width as i64
            && center_y >= position.y as i64
            && center_y < position.y as i64 + size.height as i64
    })?;

    let scale = monitor.scale_factor();
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }

    let work_width = (target.right as i64 - target.left as i64).max(1);
    let work_height = (target.bottom as i64 - target.top as i64).max(1);
    let margin = (OVERLAY_SCREEN_MARGIN * scale).round().max(1.0) as i64;
    let bottom_gap = (72.0 * scale).round().max(margin as f64) as i64;
    let available_width = (work_width - margin * 2).max(1);
    let available_height = (work_height - margin * 2).max(1);
    let (desired_width, desired_height) = if is_fallback {
        (OVERLAY_FALLBACK_WIDTH, OVERLAY_FALLBACK_HEIGHT)
    } else {
        (base_width, OVERLAY_BASE_HEIGHT)
    };
    let width = ((desired_width * scale).round().max(1.0) as i64).min(available_width);
    let height = ((desired_height * scale).round().max(1.0) as i64).min(available_height);
    let x = target.left as i64 + ((work_width - width) / 2);
    let min_y = target.top as i64 + margin;
    let max_y = target.bottom as i64 - margin - height;
    let y = (target.bottom as i64 - bottom_gap - height).clamp(min_y, max_y);

    Some((x as i32, y as i32, width as u32, height as u32))
}
