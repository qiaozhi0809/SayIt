// WebSocket service for communicating with SayIt backend

import {
  addMsg,
  addAudioChunk,
  startSession,
  endSession,
  addRuntimeEvent,
  hasActiveSession,
} from './debugLog'
import { getWSUrl } from './runtimeConfig'
import { setConnectionStatus } from '../stores/connectionStatus'
import type { ActiveAppContext } from '../types/appContext'
import type { ClientRuntimeInfo } from '../types/appApi'

export type WSState = 'disconnected' | 'connecting' | 'connected' | 'error'

export interface ASRResult {
  text: string
  asrMs: number
  durationSec: number
}

export interface FinalResult {
  asrText: string
  llmText: string
  asrMs: number
  llmMs: number
  durationSec: number
  asrEngine?: string
  asrModel?: string
}

export interface AudioStats {
  avgRms: number
  peakRms: number
  peakAmplitude: number
  silenceRatio: number
  totalFrames: number
}

export interface WSCallbacks {
  onStateChange?: (state: WSState) => void
  onReady?: (data: { connectionId: string; asr: boolean; llm: boolean }) => void
  onASR?: (result: ASRResult) => void
  onFinal?: (result: FinalResult) => void
  onDone?: () => void
  onError?: (msg: string) => void
}

let ws: WebSocket | null = null
let callbacks: WSCallbacks = {}
let reconnectTimer: ReturnType<typeof setTimeout> | null = null
let intentionalClose = false
/** 连接建立的时间戳，用于日志里计算连接存活时长 */
let connectStartMs = 0
let openedAtMs = 0

/** 抓取一小段调用栈（去掉本函数与 Error 头两行），用于日志里定位「谁触发了连接/关闭」。 */
function shortCallerStack(): string {
  const raw = new Error().stack || ''
  return raw
    .split('\n')
    .slice(2, 5)
    .map((l) => l.trim().replace(/^at\s+/, ''))
    .join(' <- ')
}

// --- 重连退避 ---
let reconnectAttempts = 0
const RECONNECT_BASE_MS = 3000
const RECONNECT_MAX_MS = 30_000
// 服务端限流类关闭码（1013 服务器满、4029 该 IP 并发超限）：退避更久，避免重连风暴
const RECONNECT_LIMIT_MIN_MS = 15_000
const LIMIT_CLOSE_CODES = new Set([1013, 4029])

/** 计算下次重连延迟：指数退避 + ±20% 抖动；限流码时至少退避 RECONNECT_LIMIT_MIN_MS */
function computeReconnectDelayMs(closeCode?: number): number {
  const exp = Math.min(RECONNECT_BASE_MS * 2 ** reconnectAttempts, RECONNECT_MAX_MS)
  const base = closeCode !== undefined && LIMIT_CLOSE_CODES.has(closeCode)
    ? Math.max(exp, RECONNECT_LIMIT_MIN_MS)
    : exp
  const jitter = base * 0.2 * (Math.random() * 2 - 1)
  return Math.max(1000, Math.round(base + jitter))
}
let sessionStarted = false
let audioDropWarned = false

// --- Heartbeat ---
const HEARTBEAT_INTERVAL_MS = 30_000
const HEARTBEAT_TIMEOUT_MS = 10_000
let heartbeatTimer: ReturnType<typeof setInterval> | null = null
let pongTimer: ReturnType<typeof setTimeout> | null = null

function startHeartbeat() {
  stopHeartbeat()
  heartbeatTimer = setInterval(() => {
    if (ws?.readyState !== WebSocket.OPEN) return
    try {
      ws.send(JSON.stringify({ cmd: 'ping' }))
      // Start pong timeout
      pongTimer = setTimeout(() => {
        addRuntimeEvent('warn', 'websocket', '心跳超时：10s 未收到 pong，主动断开')
        // Force close so onclose triggers reconnect
        try { ws?.close(4000, 'heartbeat timeout') } catch { /* ignore */ }
      }, HEARTBEAT_TIMEOUT_MS)
    } catch {
      // send failed, connection is likely dead
      try { ws?.close(4000, 'heartbeat send failed') } catch { /* ignore */ }
    }
  }, HEARTBEAT_INTERVAL_MS)
}

function stopHeartbeat() {
  if (heartbeatTimer) { clearInterval(heartbeatTimer); heartbeatTimer = null }
  if (pongTimer) { clearTimeout(pongTimer); pongTimer = null }
}

function handlePong() {
  if (pongTimer) { clearTimeout(pongTimer); pongTimer = null }
}

function updateGlobalState(state: WSState) {
  setConnectionStatus(state)
  callbacks.onStateChange?.(state)
}

// HMR cleanup: close WebSocket when module is hot-replaced
if ((import.meta as unknown as Record<string, unknown>).hot) {
  const hot = (import.meta as unknown as Record<string, unknown>).hot as { dispose: (cb: () => void) => void }
  hot.dispose(() => {
    console.log('[websocket] HMR dispose: closing WebSocket')
    intentionalClose = true
    stopHeartbeat()
    if (reconnectTimer) {
      clearTimeout(reconnectTimer)
      reconnectTimer = null
    }
    try { ws?.close() } catch { /* ignore */ }
    ws = null
    sessionStarted = false
  })
}

function endSessionIfNeeded() {
  if (sessionStarted || hasActiveSession()) {
    sessionStarted = false
    endSession()
  }
  audioDropWarned = false
}

export function connect(cbs: WSCallbacks): Promise<void> {
  callbacks = cbs

  if (ws?.readyState === WebSocket.OPEN) {
    // 已经连上，直接复用（connect 幂等）。这是每次开始录音的正常路径，不记日志避免刷屏；
    // 真正异常的连接场景由 开始连接/连接成功/主动关闭/关闭未就绪连接 等日志覆盖。
    return Promise.resolve()
  }

  if (ws) {
    // 上一条连接还在（可能仍在 CONNECTING，或刚 open 尚未标记就绪）。这里会主动关掉它再重连，
    // 这正是「连上几秒就被自己关掉、且没发 start」最可能的元凶——记录下来看看是谁触发的。
    const prevState = ws.readyState
    addRuntimeEvent('warn', 'websocket', 'connect() 关闭上一条未就绪连接并重连', {
      prevReadyState: prevState, // 0=CONNECTING 1=OPEN 2=CLOSING 3=CLOSED
      caller: shortCallerStack(),
    })
    intentionalClose = true
    try {
      ws.close()
    } catch {
      // ignore
    }
    ws = null
  }

  return new Promise((resolve, reject) => {
    intentionalClose = false
    updateGlobalState('connecting')
    const wsUrl = getWSUrl()
    connectStartMs = Date.now()
    addRuntimeEvent('info', 'websocket', '开始连接', { url: wsUrl, caller: shortCallerStack() })

    const socket = new WebSocket(wsUrl)
    socket.binaryType = 'arraybuffer'
    ws = socket

    const timeout = setTimeout(() => {
      if (socket.readyState !== WebSocket.OPEN) {
        try {
          socket.close()
        } catch {
          // ignore
        }
        updateGlobalState('error')
        addRuntimeEvent('error', 'websocket', '连接超时（10s）', { url: wsUrl })
        reject(new Error('WebSocket connection timeout'))
      }
    }, 10000)

    socket.onopen = () => {
      clearTimeout(timeout)
      openedAtMs = Date.now()
      updateGlobalState('connected')
      const wasReconnect = reconnectAttempts > 0
      addRuntimeEvent('info', 'websocket', wasReconnect ? `重连成功（第 ${reconnectAttempts} 次尝试）` : '连接成功', {
        elapsedMs: connectStartMs ? openedAtMs - connectStartMs : undefined,
      })
      reconnectAttempts = 0
      if (reconnectTimer) {
        clearTimeout(reconnectTimer)
        reconnectTimer = null
      }
      startHeartbeat()
      resolve()
    }

    socket.onmessage = (e) => {
      if (typeof e.data !== 'string') return
      try {
        const msg = JSON.parse(e.data)
        addMsg('received', msg.type || 'unknown', msg)

        switch (msg.type) {
          case 'ready':
            addRuntimeEvent('info', 'websocket', '收到 ready', {
              connectionId: msg.connection_id,
              asr: msg.asr,
              llm: msg.llm,
            })
            callbacks.onReady?.({
              connectionId: msg.connection_id,
              asr: msg.asr,
              llm: msg.llm,
            })
            break
          case 'pong':
            handlePong()
            break
          case 'asr':
            callbacks.onASR?.({
              text: msg.text,
              asrMs: msg.asr_ms,
              durationSec: Number(msg.duration_sec || 0),
            })
            break
          case 'final': {
            const durationFromAsrDebug = Number(msg?.asr_debug?.duration_sec || 0)
            callbacks.onFinal?.({
              asrText: msg.asr_text,
              llmText: msg.llm_text,
              asrMs: msg.asr_ms,
              llmMs: msg.llm_ms,
              durationSec: Number(msg.duration_sec || durationFromAsrDebug || 0),
              asrEngine: msg.asr_engine || undefined,
              asrModel: msg.asr_model || undefined,
            })
            break
          }
          case 'done':
            endSessionIfNeeded()
            callbacks.onDone?.()
            break
          case 'error':
            addRuntimeEvent('error', 'backend', String(msg.message || 'unknown backend error'), msg)
            endSessionIfNeeded()
            callbacks.onError?.(String(msg.message || 'unknown backend error'))
            break
        }
      } catch (err) {
        addRuntimeEvent('error', 'websocket', '消息解析失败', {
          error: String(err),
          raw: e.data,
        })
      }
    }

    socket.onerror = (ev) => {
      clearTimeout(timeout)
      updateGlobalState('error')
      addRuntimeEvent('error', 'websocket', '连接发生错误', {
        event: String(ev.type || 'error'),
      })
    }

    socket.onclose = (ev) => {
      clearTimeout(timeout)
      stopHeartbeat()
      ws = null
      updateGlobalState('disconnected')

      const aliveMs = openedAtMs ? Date.now() - openedAtMs : undefined
      const sentStart = sessionStarted
      endSessionIfNeeded()

      if (!intentionalClose) {
        const delay = computeReconnectDelayMs(ev.code)
        addRuntimeEvent('warn', 'websocket', `连接关闭 code=${ev.code} reason=${ev.reason || '-'}，${Math.round(delay / 1000)}s 后重连`, {
          code: ev.code,
          attempt: reconnectAttempts + 1,
          aliveMs,
          sentStart,
        })
        reconnectAttempts++
        reconnectTimer = setTimeout(() => connect(callbacks), delay)
      } else {
        // 主动关闭这一路以前是完全静默的（切换供应商/地址、connect() 替换旧连接、disconnect()）。
        // 现在也记一条，标明是客户端主动关的，便于区分「服务端踢」还是「客户端自己关」。
        addRuntimeEvent('warn', 'websocket', `主动关闭连接 code=${ev.code} reason=${ev.reason || '-'}`, {
          code: ev.code,
          aliveMs,
          sentStart,
          caller: shortCallerStack(),
        })
      }
      openedAtMs = 0
    }
  })
}

export function disconnect() {
  addRuntimeEvent('info', 'websocket', 'disconnect() 被调用', {
    hadSocket: !!ws,
    readyState: ws?.readyState,
    caller: shortCallerStack(),
  })
  intentionalClose = true
  reconnectAttempts = 0
  stopHeartbeat()
  if (reconnectTimer) {
    clearTimeout(reconnectTimer)
    reconnectTimer = null
  }
  try {
    ws?.close()
  } catch {
    // ignore
  }
  ws = null
  endSessionIfNeeded()
}

export function sendStart(opts?: {
  systemPrompt?: string
  disableAi?: boolean
  clientMeta?: ClientRuntimeInfo | null
  appContext?: ActiveAppContext | null
  source?: string
  hotwords?: string[]
  language?: string
}): boolean {
  if (ws?.readyState !== WebSocket.OPEN) {
    addRuntimeEvent('error', 'websocket', '发送 start 失败：连接未就绪')
    return false
  }

  const msg: Record<string, unknown> = { cmd: 'start', source: opts?.source || 'live' }
  if (opts?.systemPrompt) msg.system_prompt = opts.systemPrompt
  if (opts?.disableAi) msg.disable_ai = true
  if (opts?.clientMeta) {
    msg.client_meta = {
      user_id: opts.clientMeta.userId,
      device_id: opts.clientMeta.deviceId,
      hostname: opts.clientMeta.hostname,
      client_version: opts.clientMeta.clientVersion,
      platform: opts.clientMeta.platform,
      os_version: opts.clientMeta.osVersion,
      local_ip: opts.clientMeta.localIp,
      system_locale: opts.clientMeta.systemLocale,
      cpu_cores: opts.clientMeta.cpuCores,
      memory_mb: opts.clientMeta.memoryMb,
    }
  }
  if (opts?.appContext) {
    msg.app_context = {
      process_name: opts.appContext.processName,
      exe_path: opts.appContext.exePath,
      window_class: opts.appContext.windowClass,
      focus_class: opts.appContext.focusClass,
      control_type: opts.appContext.controlType,
    }
  }
  if (opts?.hotwords && opts.hotwords.length > 0) {
    msg.hotwords = opts.hotwords
  }
  if (opts?.language) {
    msg.language = opts.language
  }

  try {
    startSession(opts)
    addMsg('sent', 'start', msg)
    ws.send(JSON.stringify(msg))
    sessionStarted = true
    audioDropWarned = false
    return true
  } catch (err) {
    addRuntimeEvent('error', 'websocket', '发送 start 失败', { error: String(err) })
    endSessionIfNeeded()
    return false
  }
}

export function sendStop(opts?: { pttHoldMs?: number; audioStats?: AudioStats }): boolean {
  if (!sessionStarted) {
    return false
  }

  if (ws?.readyState !== WebSocket.OPEN) {
    addRuntimeEvent('error', 'websocket', '发送 stop 失败：连接未就绪')
    endSessionIfNeeded()
    return false
  }

  try {
    const payload: Record<string, unknown> = { cmd: 'stop' }
    if (typeof opts?.pttHoldMs === 'number' && Number.isFinite(opts.pttHoldMs)) {
      payload.usage_meta = { ptt_hold_ms: Math.max(0, Math.round(opts.pttHoldMs)) }
    }
    if (opts?.audioStats) {
      payload.audio_stats = {
        avg_rms: opts.audioStats.avgRms,
        peak_rms: opts.audioStats.peakRms,
        peak_amplitude: opts.audioStats.peakAmplitude,
        silence_ratio: opts.audioStats.silenceRatio,
        total_frames: opts.audioStats.totalFrames,
      }
    }
    addMsg('sent', 'stop', payload)
    ws.send(JSON.stringify(payload))
    return true
  } catch (err) {
    addRuntimeEvent('error', 'websocket', '发送 stop 失败', { error: String(err) })
    endSessionIfNeeded()
    return false
  }
}

export function sendAudio(buffer: ArrayBuffer) {
  if (!sessionStarted) {
    return
  }

  if (ws?.readyState === WebSocket.OPEN) {
    audioDropWarned = false
    try {
      ws.send(buffer)
      addAudioChunk(buffer)
    } catch (err) {
      addRuntimeEvent('error', 'websocket', '发送音频失败', {
        error: String(err),
        bytes: buffer.byteLength,
      })
    }
    return
  }

  if (!audioDropWarned) {
    audioDropWarned = true
    addRuntimeEvent('warn', 'websocket', '音频已丢弃：连接断开', { bytes: buffer.byteLength })
  }
}

export function isConnected(): boolean {
  return ws?.readyState === WebSocket.OPEN
}
