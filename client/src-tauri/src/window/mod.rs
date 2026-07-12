use serde_json::Value;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::commands::system::write_log_line;

const OVERLAY_DEFAULT_BASE_WIDTH: f64 = 360.0;
const OVERLAY_BASE_HEIGHT: f64 = 56.0;
const OVERLAY_FALLBACK_WIDTH: f64 = 520.0;
const OVERLAY_FALLBACK_HEIGHT: f64 = 224.0;

/// Time of last received pong from overlay (epoch millis). 0 = never.
/// Used to detect WebView2 unresponsiveness in `show_overlay`.
static LAST_PONG_MS: AtomicI64 = AtomicI64::new(0);
/// Monotonic id for ping/pong correlation
static PING_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn record_overlay_pong(_seq: u64) {
    let now = chrono::Utc::now().timestamp_millis();
    LAST_PONG_MS.store(now, Ordering::SeqCst);
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
}

impl WindowState {
    pub fn new() -> Self {
        Self {
            overlay_layout: Mutex::new(OverlayLayout::Base),
            overlay_base_width: Mutex::new(OVERLAY_DEFAULT_BASE_WIDTH),
        }
    }

    pub fn show_overlay(&self, app: &AppHandle) {
        let layout = self.overlay_layout.lock().unwrap().clone();
        let is_fallback = layout == OverlayLayout::Fallback;
        let base_w = *self.overlay_base_width.lock().unwrap();

        let bounds = calc_overlay_bounds(app, is_fallback, base_w);
        let (mon_w, mon_h, mon_scale) = monitor_info(app);

        if let Some(overlay) = app.get_webview_window("overlay") {
            // ── Diagnostic snapshot before any state change ──
            let was_visible = overlay.is_visible().unwrap_or(false);
            let outer_pos = overlay.outer_position()
                .map(|p| format!("{},{}", p.x, p.y))
                .unwrap_or_else(|_| "err".to_string());
            let outer_size = overlay.outer_size()
                .map(|s| format!("{}x{}", s.width, s.height))
                .unwrap_or_else(|_| "err".to_string());

            // Probe webview responsiveness — send a ping; pong arrival is logged async.
            let seq = PING_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
            let last_pong = LAST_PONG_MS.load(Ordering::SeqCst);
            let pong_age_ms = if last_pong == 0 { -1 } else { now_ms() - last_pong };

            let ping_emit_err = overlay.emit("overlay-ping", seq).err()
                .map(|e| format!("{:?}", e));

            let pos_err = overlay.set_position(tauri::Position::Logical(
                tauri::LogicalPosition::new(bounds.0, bounds.1)
            )).err().map(|e| format!("{:?}", e));
            let size_err = overlay.set_size(tauri::Size::Logical(
                tauri::LogicalSize::new(bounds.2, bounds.3)
            )).err().map(|e| format!("{:?}", e));
            let show_err = overlay.show().err().map(|e| format!("{:?}", e));
            let aot_err = overlay.set_always_on_top(true).err().map(|e| format!("{:?}", e));
            set_overlay_interactivity(&overlay, is_fallback);

            let any_err = pos_err.is_some() || size_err.is_some()
                || show_err.is_some() || aot_err.is_some();

            // 常规显示（无错误）不再记录日志，避免每次录音刷屏；
            // 仅在出错时记录完整诊断信息（含 ping/pong 存活探测），便于排查悬浮窗异常。
            if any_err {
                let now_visible = overlay.is_visible().unwrap_or(false);
                let now_outer_pos = overlay.outer_position()
                    .map(|p| format!("{},{}", p.x, p.y))
                    .unwrap_or_else(|_| "err".to_string());
                write_log_line(&format!(
                    "[overlay-diag] show FAILED layout={:?} fallback={} was_visible={} \
                     outer_pos={} outer_size={} target_bounds={:?} monitor={}x{}@{:.2} \
                     ping_seq={} last_pong_age_ms={} ping_emit_err={:?} \
                     now_visible={} now_outer_pos={} \
                     pos_err={:?} size_err={:?} show_err={:?} aot_err={:?}",
                    layout, is_fallback, was_visible,
                    outer_pos, outer_size, bounds, mon_w, mon_h, mon_scale,
                    seq, pong_age_ms, ping_emit_err,
                    now_visible, now_outer_pos,
                    pos_err, size_err, show_err, aot_err,
                ));
            }
        } else {
            write_log_line(&format!(
                "[overlay-diag] show begin handle=MISSING layout={:?} fallback={} \
                 target_bounds={:?} monitor={}x{}@{:.2} — recreating",
                layout, is_fallback, bounds, mon_w, mon_h, mon_scale,
            ));
            self.create_overlay(app, is_fallback, base_w);
        }
    }

    pub fn hide_overlay(&self, app: &AppHandle) {
        // Reset to base layout when hiding
        let prev_layout = {
            let mut g = self.overlay_layout.lock().unwrap();
            let prev = g.clone();
            *g = OverlayLayout::Base;
            prev
        };

        if let Some(overlay) = app.get_webview_window("overlay") {
            set_overlay_interactivity(&overlay, false);
            let hide_err = overlay.hide().err().map(|e| format!("{:?}", e));
            // 常规隐藏（无错误）不再记录日志，避免每次录音都刷屏；只在隐藏出错时记录。
            if let Some(err) = hide_err {
                write_log_line(&format!(
                    "[overlay-diag] hide FAILED prev_layout={:?} hide_err={}",
                    prev_layout, err,
                ));
            }
        }
        // overlay 句柄不存在时无需隐藏，也无需记录（属正常情况：从未显示过或已销毁）。
    }

    pub fn update_overlay_state(&self, app: &AppHandle, data: &Value) {
        let state_str = data.get("state").and_then(|s| s.as_str()).unwrap_or("");

        // Update base width if provided
        if let Some(bw) = data.get("baseWidth").and_then(|v| v.as_f64()) {
            if bw >= 160.0 && bw <= 600.0 {
                *self.overlay_base_width.lock().unwrap() = bw;
            }
        }

        let next_layout = if state_str == "fallback" {
            OverlayLayout::Fallback
        } else {
            OverlayLayout::Base
        };

        let is_fallback = next_layout == OverlayLayout::Fallback;
        let base_w = *self.overlay_base_width.lock().unwrap();

        {
            let mut current = self.overlay_layout.lock().unwrap();
            if *current != next_layout {
                *current = next_layout;
                // Resize overlay
                if let Some(overlay) = app.get_webview_window("overlay") {
                    let bounds = calc_overlay_bounds(app, is_fallback, base_w);
                    let _ = overlay.set_position(tauri::Position::Logical(tauri::LogicalPosition::new(bounds.0, bounds.1)));
                    let _ = overlay.set_size(tauri::Size::Logical(tauri::LogicalSize::new(bounds.2, bounds.3)));
                    let _ = overlay.set_always_on_top(true);
                }
            }
        }

        if let Some(overlay) = app.get_webview_window("overlay") {
            set_overlay_interactivity(&overlay, is_fallback);

            if is_fallback {
                let _ = overlay.show();
            }

            let _ = overlay.emit("overlay-state", data);
        }
    }

    fn create_overlay(&self, app: &AppHandle, is_fallback: bool, base_w: f64) {
        let bounds = calc_overlay_bounds(app, is_fallback, base_w);

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
                set_overlay_interactivity(&overlay, is_fallback);
                let show_err = overlay.show().err().map(|e| format!("{:?}", e));
                let now_visible = overlay.is_visible().unwrap_or(false);
                write_log_line(&format!(
                    "[overlay-diag] create_overlay OK is_fallback={} bounds={:?} \
                     show_err={:?} now_visible={}",
                    is_fallback, bounds, show_err, now_visible,
                ));
            }
            Err(e) => {
                eprintln!("[overlay] create_overlay: FAILED: {}", e);
                log::error!("Failed to create overlay window: {}", e);
                write_log_line(&format!(
                    "[overlay-diag] create_overlay FAILED is_fallback={} bounds={:?} err={:?}",
                    is_fallback, bounds, e,
                ));
            }
        }
    }
}

fn set_overlay_interactivity(overlay: &tauri::WebviewWindow, interactive: bool) {
    let _ = overlay.set_ignore_cursor_events(!interactive);
}

/// Returns (logical_width, logical_height, scale_factor) of primary monitor.
fn monitor_info(app: &AppHandle) -> (f64, f64, f64) {
    if let Some(monitor) = app.primary_monitor().ok().flatten() {
        let size = monitor.size();
        let scale = monitor.scale_factor();
        (size.width as f64 / scale, size.height as f64 / scale, scale)
    } else {
        (1920.0, 1080.0, 1.0)
    }
}

/// Returns (x, y, width, height)
fn calc_overlay_bounds(app: &AppHandle, is_fallback: bool, base_w: f64) -> (f64, f64, f64, f64) {
    let (desired_w, desired_h) = if is_fallback {
        (OVERLAY_FALLBACK_WIDTH, OVERLAY_FALLBACK_HEIGHT)
    } else {
        (base_w, OVERLAY_BASE_HEIGHT)
    };

    // Try to get primary monitor size
    let (screen_w, screen_h) = if let Some(monitor) = app.primary_monitor().ok().flatten() {
        let size = monitor.size();
        let scale = monitor.scale_factor();
        (size.width as f64 / scale, size.height as f64 / scale)
    } else {
        (1920.0, 1080.0) // fallback
    };

    let w = desired_w.min(screen_w - 40.0).max(160.0);
    let h = desired_h.min(screen_h - 40.0).max(56.0);
    let x = ((screen_w - w) / 2.0).round();
    let y = (screen_h - h - 72.0).max(8.0);

    (x, y, w, h)
}
