// 千问实时语音识别 — qwen3-asr-flash-realtime
// WebSocket 流式：边录边发，OpenAI Realtime 风格协议

use super::types::AsrProviderConfig;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::header::{AUTHORIZATION, USER_AGENT};
use tungstenite::http::HeaderValue;

const MODEL: &str = "qwen3-asr-flash-realtime-2026-02-10";

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

static SESSION: once_cell::sync::Lazy<Arc<Mutex<Option<WsStream>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));

/// 打开千问实时 ASR 会话
///
/// hotwords：热词列表，通过 session.input_audio_transcription.corpus.text 上下文偏置（最多 10000 tokens）
#[tauri::command]
pub async fn qwen_stream_open(config: AsrProviderConfig, hotwords: Option<Vec<String>>) -> Result<(), String> {
    // 关闭旧会话
    {
        let mut session = SESSION.lock().await;
        if let Some(mut ws) = session.take() {
            let _ = ws.close(None).await;
        }
    }

    let url = format!("wss://dashscope.aliyuncs.com/api-ws/v1/realtime?model={}", MODEL);

    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("构建请求失败: {}", e))?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", config.api_key))
            .map_err(|e| format!("Authorization 请求头无效: {}", e))?,
    );
    request.headers_mut().insert(
        USER_AGENT,
        HeaderValue::from_static(concat!("SayIt/", env!("CARGO_PKG_VERSION"))),
    );

    // 让 tungstenite 从 URL 生成标准 WebSocket 握手头，只追加服务要求的鉴权头。
    // 不再手工重复构造 Host/Upgrade/Sec-WebSocket-*，也不发送当前协议不要求的 OpenAI-Beta。
    let (mut ws, response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("WebSocket 连接失败: {}", e))?;
    crate::commands::system::write_log_line(&format!(
        "[RUST] [qwen_stream] connected status={} model={}",
        response.status(), MODEL
    ));

    // 发送 session.update 配置
    let mut input_audio_transcription = serde_json::json!({
        "language": "zh"
    });
    // 热词上下文偏置（如果有）
    if let Some(words) = &hotwords {
        if let Some(ctx) = super::asr_qwen::build_hotword_context_text(words) {
            input_audio_transcription["corpus"] = serde_json::json!({ "text": ctx });
        }
    }

    let session_update = serde_json::json!({
        "event_id": "init_session",
        "type": "session.update",
        "session": {
            "modalities": ["text"],
            "input_audio_format": "pcm",
            "sample_rate": 16000,
            "input_audio_transcription": input_audio_transcription,
            "turn_detection": {
                "type": "server_vad",
                "threshold": 0.0,
                "silence_duration_ms": 400
            }
        }
    });

    ws.send(tungstenite::Message::Text(
        serde_json::to_string(&session_update).unwrap().into(),
    ))
    .await
    .map_err(|e| format!("发送 session.update 失败: {}", e))?;

    // 等待 session.updated 确认（服务端会先发送 session.created）。
    let mut session_updated = false;
    while let Some(msg) = ws.next().await {
        let msg = msg.map_err(|e| format!("接收确认失败: {}", e))?;
        if let tungstenite::Message::Text(text) = msg {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                let event_type = data.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if event_type == "session.updated" {
                    session_updated = true;
                    crate::commands::system::write_log_line(
                        "[RUST] [qwen_stream] session updated",
                    );
                    break;
                }
                if event_type == "error" {
                    let err_msg = data.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("未知错误");
                    return Err(format!("服务端错误: {}", err_msg));
                }
            }
        }
    }
    if !session_updated {
        return Err("WebSocket 在 session.updated 前已关闭".to_string());
    }

    let mut session = SESSION.lock().await;
    *session = Some(ws);

    Ok(())
}

/// 发送音频数据
#[tauri::command]
pub async fn qwen_stream_send(pcm_b64: String) -> Result<(), String> {
    let mut session = SESSION.lock().await;
    let ws = session.as_mut().ok_or("会话未建立")?;

    let event = serde_json::json!({
        "event_id": format!("audio_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()),
        "type": "input_audio_buffer.append",
        "audio": pcm_b64
    });

    ws.send(tungstenite::Message::Text(
        serde_json::to_string(&event).unwrap().into(),
    ))
    .await
    .map_err(|e| format!("发送音频失败: {}", e))?;

    Ok(())
}

/// 结束会话并等待最终结果
#[tauri::command]
pub async fn qwen_stream_finish() -> Result<String, String> {
    let mut session = SESSION.lock().await;
    let ws = session.as_mut().ok_or("会话未建立")?;

    // 发送 session.finish
    let finish_event = serde_json::json!({
        "event_id": "finish",
        "type": "session.finish"
    });

    ws.send(tungstenite::Message::Text(
        serde_json::to_string(&finish_event).unwrap().into(),
    ))
    .await
    .map_err(|e| format!("发送 finish 失败: {}", e))?;

    // 等待 session.finished，收集转录结果
    let mut final_text = String::new();
    let mut completed_count = 0usize;

    while let Some(msg) = ws.next().await {
        let msg = msg.map_err(|e| format!("接收结果失败: {}", e))?;
        if let tungstenite::Message::Text(text) = msg {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                let event_type = data.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match event_type {
                    "conversation.item.input_audio_transcription.completed" => {
                        if let Some(transcript) = data.get("transcript").and_then(|t| t.as_str()) {
                            if !transcript.is_empty() {
                                completed_count += 1;
                                if !final_text.is_empty() {
                                    final_text.push_str("，");
                                }
                                final_text.push_str(transcript);
                            }
                        }
                    }
                    "session.finished" => {
                        // 最终文本由 completed 事件逐段提供；session.finished 仅表示会话结束。
                        break;
                    }
                    "error" => {
                        let err_msg = data.get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("未知错误");
                        let _ = ws.close(None).await;
                        *session = None;
                        return Err(format!("识别错误: {}", err_msg));
                    }
                    _ => {}
                }
            }
        }
    }

    crate::commands::system::write_log_line(&format!(
        "[RUST] [qwen_stream] finished completedSegments={} utf8Len={} utf16Len={}",
        completed_count,
        final_text.len(),
        final_text.encode_utf16().count()
    ));
    let _ = ws.close(None).await;
    *session = None;

    Ok(final_text)
}

/// 关闭会话
#[tauri::command]
pub async fn qwen_stream_close() -> Result<(), String> {
    let mut session = SESSION.lock().await;
    if let Some(mut ws) = session.take() {
        let _ = ws.close(None).await;
    }
    Ok(())
}
