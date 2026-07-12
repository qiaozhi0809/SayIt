/**
 * 应用启动时自动检查、下载、安装更新
 * 在 App.tsx 中调用，不依赖任何页面挂载
 *
 * 全局单例状态：关于页等其它 UI 通过 onAutoUpdateChange 订阅同一份状态，
 * 并复用 checkNow/downloadNow/installNow 手动触发，不再各自维护一套独立的
 * 检查/下载/安装逻辑 —— 避免应用启动时的自动检查与用户打开关于页的检查同时
 * 触发，造成重复下载/安装。
 */

import { listen } from '@tauri-apps/api/event'
import { checkVersionUpdate, type VersionInfo } from './updateChecker'
import { getSetting } from '@/services/store'
import * as bridge from '@/services/bridge'

/** 更新状态，供 UI 层订阅 */
export type AutoUpdatePhase = 'idle' | 'checking' | 'checked' | 'downloading' | 'downloaded' | 'installing'
export interface AutoUpdateState {
  phase: AutoUpdatePhase
  version?: string
  versionInfo?: VersionInfo | null
  checkedAt?: number | null
  downloadedFilePath?: string | null
  error?: string | null
  /** 下载进度百分比（0-100），来自 Rust 端 update-download-progress 事件的真实字节进度 */
  downloadPercent?: number
}

let currentState: AutoUpdateState = { phase: 'idle', versionInfo: null, checkedAt: null }
const listeners: Set<(state: AutoUpdateState) => void> = new Set()
/** 正在进行中的检查/下载/安装 Promise，用于防止并发触发（启动自动检查 + 用户手动点击撞在一起） */
let inFlight: Promise<void> | null = null

// 订阅 Rust 端下载真实进度事件，驱动更新弹窗的进度条
void listen<{ downloadedBytes: number; totalBytes: number; percent: number; status: string; error: string | null }>(
  'update-download-progress',
  (event) => {
    const { percent } = event.payload
    setState({ downloadPercent: percent })
  },
)

function setState(patch: Partial<AutoUpdateState>) {
  currentState = { ...currentState, ...patch }
  listeners.forEach((cb) => cb(currentState))
}

export function getAutoUpdateState() {
  return currentState
}

export function onAutoUpdateChange(cb: (state: AutoUpdateState) => void) {
  listeners.add(cb)
  return () => { listeners.delete(cb) }
}

/** 手动触发一次检查更新（关于页“检查更新”按钮使用），与启动自动检查共用同一状态与并发锁 */
export async function checkNow(): Promise<VersionInfo> {
  if (inFlight) {
    await inFlight
    return currentState.versionInfo || { hasUpdate: false, currentVersion: __APP_VERSION__, latestVersion: null, downloadUrl: null, releaseDate: null, error: null }
  }
  const task = (async () => {
    setState({ phase: 'checking', error: null })
    const currentVersion = __APP_VERSION__
    const info = await checkVersionUpdate(currentVersion)
    setState({ phase: 'checked', versionInfo: info, checkedAt: Date.now() })
  })()
  inFlight = task.finally(() => { inFlight = null })
  await inFlight
  return currentState.versionInfo!
}

/** 手动触发下载（关于页“下载更新”按钮使用） */
export async function downloadNow(): Promise<void> {
  const url = currentState.versionInfo?.downloadUrl
  if (!url) return
  if (inFlight) { await inFlight; return }
  const task = (async () => {
    setState({ phase: 'downloading', version: currentState.versionInfo?.latestVersion || undefined, error: null, downloadPercent: 0 })
    try {
      const filePath = await bridge.downloadUpdate(url)
      setState({ phase: 'downloaded', downloadedFilePath: filePath, downloadPercent: 100 })
    } catch (err) {
      setState({ phase: 'checked', error: String(err) })
    }
  })()
  inFlight = task.finally(() => { inFlight = null })
  await inFlight
}

/** 手动触发安装（关于页“立即安装”按钮使用） */
export async function installNow(): Promise<void> {
  const filePath = currentState.downloadedFilePath
  if (!filePath) return
  setState({ phase: 'installing' })
  try {
    await bridge.installDownloadedUpdate(filePath)
  } catch (err) {
    setState({ phase: 'downloaded', error: String(err) })
  }
}

/** 应用启动时自动检查一次，发现新版本自动下载并安装（若设置开启） */
export async function runAutoUpdate() {
  const enabled = await getSetting('autoCheckUpdate', true)
  if (!enabled) return
  if (inFlight) return

  const task = (async () => {
    setState({ phase: 'checking', error: null })
    const currentVersion = __APP_VERSION__
    const info = await checkVersionUpdate(currentVersion)
    setState({ phase: 'checked', versionInfo: info, checkedAt: Date.now() })

    if (!info?.hasUpdate || !info.downloadUrl) {
      setState({ phase: 'idle' })
      return
    }

    setState({ phase: 'downloading', version: info.latestVersion || undefined, downloadPercent: 0 })
    try {
      const filePath = await bridge.downloadUpdate(info.downloadUrl)
      setState({ phase: 'downloaded', downloadedFilePath: filePath, downloadPercent: 100 })

      // 通知用户即将安装，给 3 秒缓冲
      setState({ phase: 'installing', version: info.latestVersion || undefined })
      await new Promise((r) => setTimeout(r, 3000))

      await bridge.installDownloadedUpdate(filePath)
    } catch (err) {
      setState({ phase: 'checked', error: String(err) })
    }
  })()
  inFlight = task.finally(() => { inFlight = null })
  await inFlight
}
