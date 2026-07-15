/**
 * Tauri IPC Bridge — 所有前端代码通过这个模块与 Rust 后端通信。
 */

import { invoke } from '@tauri-apps/api/core'
import { listen, emit } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'

import type { UpdateStatus } from '../types/update'
import type { DiagnosticOccurrence, DiagnosticsPreview } from '../types/appApi'

// Re-export for convenience
export { invoke, listen, emit }

// ─── Window Controls ───

export function minimize() {
  getCurrentWindow().minimize()
}

export function maximize() {
  getCurrentWindow().toggleMaximize()
}

export function close() {
  getCurrentWindow().close()
}

// ─── Overlay ───

/**
 * Bug 003 diagnostic helper — log overlay IPC failures to runtime events
 * (which mirror to sayit.log via appendDebugLog).
 */
function logOverlayIpcError(op: string, err: unknown) {
  try {
    // Lazy import to avoid circular dep with debugLog
    void import('./debugLog').then(({ addRuntimeEvent }) => {
      addRuntimeEvent('error', 'overlay-ipc', `${op} failed`, { error: String(err) })
    })
  } catch {
    console.error(`[overlay-ipc] ${op} failed:`, err)
  }
}

export interface OverlayHealthSnapshot {
  showId: number
  activeShowId: number
  activeGeneration: number
  acked: boolean
  ackGeneration: number
  ackLatencyMs: number
  recoveryStarted: boolean
  recoverySucceeded: boolean
  window: {
    handleExists: boolean
    visible?: boolean | null
    intersectsPrimary?: boolean
    position?: { x: number; y: number } | null
    size?: { width: number; height: number } | null
  }
}

/** 原子地更新状态并显示悬浮窗，返回本次显示的关联 id。 */
export function presentOverlay(data: unknown) {
  return invoke<number>('present_overlay', { data }).catch((err) => {
    logOverlayIpcError('present_overlay', err)
    return 0
  })
}

/** 兼容旧调用；新显示流程应优先使用 presentOverlay。 */
export function showOverlay() {
  return invoke<number>('show_overlay').catch((err) => {
    logOverlayIpcError('show_overlay', err)
    return 0
  })
}

export function hideOverlay() {
  return invoke<void>('hide_overlay').catch((err) => {
    logOverlayIpcError('hide_overlay', err)
  })
}

export function updateOverlay(data: unknown) {
  return invoke<void>('update_overlay_state', { data }).catch((err) => {
    logOverlayIpcError('update_overlay_state', err)
  })
}

export function overlayReady() {
  return invoke<void>('overlay_ready')
}

export function overlayRenderAck(data: unknown) {
  return invoke<void>('overlay_render_ack', { data })
}

export function getOverlayHealth(showId: number) {
  return invoke<OverlayHealthSnapshot>('get_overlay_health', { showId })
}

// ─── Paste / Context ───

export function pasteText(text: string, hwnd?: string, focusHwnd?: string, restoreClipboard?: boolean) {
  return invoke<{
    ok: boolean
    strategy?: string
    reason?: string
    detail?: string
    attempts?: Array<{ strategy: string; ok: boolean; reason?: string; detail?: string }>
  }>('paste_text', { text, hwnd: hwnd || null, focusHwnd: focusHwnd || null, restoreClipboard: restoreClipboard ?? false })
}

export function getProbeResult() {
  return invoke<Record<string, unknown>>('get_probe_result')
}

export function getActiveAppContext() {
  return invoke<Record<string, unknown> | null>('get_active_app_context')
}

export function getClientRuntimeInfo() {
  return invoke<{
    userId: string
    userName: string
    deviceId: string
    hostname: string
    clientVersion: string
    platform: string
    osVersion: string
    localIp: string
    systemLocale: string
    cpuCores: number
    memoryMb: number
  }>('get_client_runtime_info')
}

export function copyText(text: string) {
  return invoke('copy_text', { text })
}

// ─── 系统输出静音（录音期间防回采）───

/** 记录当前默认输出设备静音状态并将其静音。返回是否已处理。 */
export function muteSystemOutput() {
  return invoke<boolean>('mute_system_output')
}

/** 恢复到 muteSystemOutput 之前记录的静音状态。 */
export function restoreSystemOutput() {
  return invoke<boolean>('restore_system_output')
}

export function appendDebugLog(payload: unknown) {
  invoke('append_debug_log', { payload })
}

// ─── Store ───

export function storeGet(key: string) {
  return invoke<unknown>('store_get', { key })
}

export function storeSet(key: string, value: unknown) {
  return invoke('store_set', { key, value })
}

export function storeDelete(key: string) {
  return invoke('store_delete', { key })
}

// ─── History ───

export function historyList(query?: {
  keyword?: string
  favoriteOnly?: boolean
  limit?: number
  offset?: number
}) {
  return invoke<unknown[]>('history_list', { query })
}

export function historyCount(query?: {
  keyword?: string
  favoriteOnly?: boolean
}) {
  return invoke<number>('history_count', { query })
}

export function historyAdd(record: unknown) {
  return invoke('history_add', { record })
}

export function historyUpdate(id: string, patch: Record<string, unknown>) {
  return invoke('history_update', { id, patch })
}

export function historyDelete(id: string) {
  return invoke('history_delete', { id })
}

export function historySetFavorite(id: string, favorite: boolean) {
  return invoke('history_set_favorite', { id, favorite })
}

// ─── Export ───

export function saveTextExport(payload: {
  defaultPath: string
  content: string
  filters?: Array<{ name: string; extensions: string[] }>
}) {
  return invoke<string | null>('save_text_export', { payload })
}

export function saveExportBundle(payload: {
  defaultPath: string
  files: Array<{ name: string; content: string }>
}) {
  return invoke<string | null>('save_export_bundle', { payload })
}

// ─── Shortcuts ───

/** 前端内部广播：快捷键设置已变化，供页面（如首页提示）实时刷新显示。 */
export const SHORTCUTS_CHANGED_EVENT = 'sayit:shortcuts-changed'

export function notifyShortcutsChanged() {
  invoke('shortcuts_changed')
  // 同时在前端广播，让依赖快捷键显示的页面无需切换路由即可刷新
  if (typeof window !== 'undefined') {
    window.dispatchEvent(new CustomEvent(SHORTCUTS_CHANGED_EVENT))
  }
}

export function testShortcut(accelerator: string) {
  return invoke<boolean>('test_shortcut', { accelerator })
}

// ─── System ───

export function getAutoLaunch() {
  return invoke<boolean>('get_auto_launch')
}

export function setAutoLaunch(enable: boolean) {
  return invoke('set_auto_launch', { enable })
}

export function getUpdateStatus() {
  return invoke<UpdateStatus>('get_update_status')
}

export function checkForUpdates() {
  return invoke<UpdateStatus>('check_for_updates')
}

export function installDownloadedUpdate(filePath: string) {
  return invoke('install_downloaded_update', { filePath })
}

export function downloadUpdate(url: string) {
  return invoke<string>('download_update', { url })
}

export function setPTTLabConfig(data: unknown) {
  console.log('[bridge] setPTTLabConfig called', data)
  invoke('set_ptt_lab_config', { data }).catch((err) => {
    console.error('[bridge] setPTTLabConfig failed:', err)
  })
}

// ─── Audio Files ───

export function saveAudioFile(id: string, wavBase64: string) {
  return invoke<string>('save_audio_file', { id, wavBase64 })
}

export function savePcmAsWav(id: string, pcmBase64: string, sampleRate?: number) {
  return invoke<string>('save_pcm_as_wav', { id, pcmBase64, sampleRate: sampleRate ?? null })
}

export function readAudioFile(filePath: string) {
  return invoke<string | null>('read_audio_file', { filePath })
}

export function deleteAudioFile(filePath: string) {
  return invoke('delete_audio_file', { filePath })
}

// ─── Diagnostics ───

export function collectSettings() {
  return invoke<Record<string, unknown>>('collect_settings')
}

export function getDiagnosticsPreview(data: {
  settings: Record<string, unknown>
  issueOccurrence: DiagnosticOccurrence
}) {
  return invoke<DiagnosticsPreview>('get_diagnostics_preview', { data })
}

export function createDiagnosticsZip(data: unknown) {
  return invoke<string>('create_diagnostics_zip', { data })
}

export function readDiagnosticsZip(path: string) {
  return invoke<number[] | null>('read_diagnostics_zip', { path })
}

export function copyDiagnosticsZip(source: string, destination: string) {
  return invoke<void>('copy_diagnostics_zip', { source, destination })
}

export function readLogFile(logType: string) {
  return invoke<string | null>('read_log_file', { logType })
}

export function openLogFolder() {
  return invoke('open_log_folder')
}

// ─── Event Listeners ───

export function onOverlayState(cb: (data: unknown) => void) {
  const unlisten = listen<unknown>('overlay-state', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onActiveAppContext(cb: (data: unknown) => void) {
  const unlisten = listen<unknown>('active-app-context', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onPTTDown(cb: (data?: unknown) => void) {
  const unlisten = listen<unknown>('ptt-down', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onPTTUp(cb: (data?: unknown) => void) {
  const unlisten = listen<unknown>('ptt-up', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onPTTToggle(cb: (data?: unknown) => void) {
  const unlisten = listen<unknown>('ptt-toggle', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onPTTTimeoutWarning(cb: (data?: unknown) => void) {
  const unlisten = listen<unknown>('ptt-timeout-warning', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onToggleHandsFree(cb: (data?: unknown) => void) {
  const unlisten = listen<unknown>('toggle-hands-free', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onPTTLabEvent(cb: (data?: unknown) => void) {
  const unlisten = listen<unknown>('ptt-lab-event', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onUpdateStatus(cb: (status: UpdateStatus) => void) {
  const unlisten = listen<UpdateStatus>('update-status', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}
