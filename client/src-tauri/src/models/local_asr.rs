// 本地 ASR 推理 — 使用 sherpa-onnx 官方 Rust crate
// 模型加载后缓存在内存中，避免每次推理都重新初始化

use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use sherpa_onnx::{
    OfflineFireRedAsrCtcModelConfig,
    OfflineFireRedAsrModelConfig,
    OfflineFunASRNanoModelConfig,
    OfflineParaformerModelConfig,
    OfflineQwen3ASRModelConfig,
    OfflineRecognizer,
    OfflineRecognizerConfig,
    OfflineSenseVoiceModelConfig,
    OfflineWhisperModelConfig,
    SileroVadModelConfig,
    VadModelConfig,
    VoiceActivityDetector,
};

use super::downloader::model_dir;

#[derive(Debug, Clone, Serialize)]
pub struct LocalAsrResult {
    pub text: String,
    pub elapsed_ms: u64,
}

pub(crate) struct RecognizerCache {
    model_id: String,
    language: String,
    hotwords: String,
    recognizer: OfflineRecognizer,
}

// sherpa-onnx 的 OfflineRecognizer 内部使用 C 指针，但实际是线程安全的
// （sherpa-onnx C API 文档确认 recognizer 可跨线程使用）
unsafe impl Send for RecognizerCache {}

static CACHE: Mutex<Option<RecognizerCache>> = Mutex::new(None);

/// hotwords 在 recognizer 创建时写入 config（Qwen3），故作为缓存 key 的一部分：
/// 热词变化时重建 recognizer，否则复用。
fn ensure_loaded(model_id: &str, language: &str, hotwords: &str) -> Result<(), String> {
    let mut cache = CACHE.lock().map_err(|e| format!("锁获取失败: {}", e))?;

    if let Some(ref c) = *cache {
        if c.model_id == model_id && c.language == language && c.hotwords == hotwords {
            return Ok(());
        }
    }

    let dir = model_dir(model_id);
    if !dir.exists() {
        return Err(format!("模型 \"{}\" 尚未下载", model_id));
    }

    log::info!("Loading ASR model: {}", model_id);
    let start = Instant::now();

    let recognizer = create_recognizer(&dir, model_id, language, hotwords)?;

    log::info!(
        "ASR model loaded in {}ms: {} (lang={})",
        start.elapsed().as_millis(),
        model_id,
        language
    );
    *cache = Some(RecognizerCache {
        model_id: model_id.to_string(),
        language: language.to_string(),
        hotwords: hotwords.to_string(),
        recognizer,
    });
    Ok(())
}

/// 根据 model_id 前缀选择模型类型并创建 recognizer
fn create_recognizer(
    dir: &Path,
    model_id: &str,
    language: &str,
    hotwords: &str,
) -> Result<OfflineRecognizer, String> {
    if model_id.starts_with("whisper-") {
        create_whisper(dir, model_id, language)
    } else if model_id.starts_with("funasr-nano") {
        create_funasr_nano(dir, language)
    } else if model_id.starts_with("qwen3-asr") {
        create_qwen3_asr(dir, hotwords)
    } else if model_id.starts_with("paraformer") {
        create_paraformer(dir)
    } else if model_id.starts_with("fire-red-asr2-ctc") {
        create_fire_red_ctc(dir)
    } else if model_id.starts_with("fire-red-asr2-aed") {
        create_fire_red_aed(dir)
    } else {
        create_sensevoice(dir, model_id, language)
    }
}

fn create_sensevoice(
    dir: &Path,
    model_id: &str,
    language: &str,
) -> Result<OfflineRecognizer, String> {
    let model_path = dir.join("model.int8.onnx");
    let model_path = if model_path.exists() {
        model_path
    } else {
        let fp32 = dir.join("model.onnx");
        if fp32.exists() { fp32 } else { return Err("模型文件不存在".into()); }
    };
    let tokens_path = dir.join("tokens.txt");
    if !tokens_path.exists() {
        return Err("tokens.txt 不存在".into());
    }

    let use_itn = !model_id.contains("funasr-nano");

    let mut config = OfflineRecognizerConfig::default();
    config.model_config.num_threads = 4;
    config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
        model: Some(model_path.to_string_lossy().to_string()),
        language: Some(language.to_string()),
        use_itn,
    };
    config.model_config.tokens = Some(tokens_path.to_string_lossy().to_string());

    OfflineRecognizer::create(&config)
        .ok_or_else(|| "SenseVoice 初始化失败".to_string())
}

fn create_whisper(
    dir: &Path,
    model_id: &str,
    language: &str,
) -> Result<OfflineRecognizer, String> {
    let size = model_id.split('-').last().unwrap_or("small");
    let encoder = dir.join(format!("{}-encoder.int8.onnx", size));
    let decoder = dir.join(format!("{}-decoder.int8.onnx", size));
    let tokens = dir.join(format!("{}-tokens.txt", size));

    if !encoder.exists() || !decoder.exists() {
        return Err(format!(
            "Whisper 模型文件不完整: 需要 {}-encoder/decoder.int8.onnx",
            size
        ));
    }

    let mut config = OfflineRecognizerConfig::default();
    config.model_config.num_threads = 4;
    config.model_config.whisper = OfflineWhisperModelConfig {
        encoder: Some(encoder.to_string_lossy().to_string()),
        decoder: Some(decoder.to_string_lossy().to_string()),
        language: Some(if language == "auto" { "".into() } else { language.to_string() }),
        ..Default::default()
    };
    config.model_config.tokens = Some(tokens.to_string_lossy().to_string());

    OfflineRecognizer::create(&config)
        .ok_or_else(|| "Whisper 初始化失败".to_string())
}

fn create_funasr_nano(
    dir: &Path,
    language: &str,
) -> Result<OfflineRecognizer, String> {
    let encoder_adaptor = dir.join("encoder_adaptor.int8.onnx");
    let llm = dir.join("llm.int8.onnx");
    let embedding = dir.join("embedding.int8.onnx");
    let tokenizer = dir.join("Qwen3-0.6B");

    if !encoder_adaptor.exists() || !llm.exists() || !embedding.exists() {
        return Err("FunASR Nano 模型文件不完整".into());
    }
    if !tokenizer.exists() {
        return Err("FunASR Nano tokenizer 目录不存在 (Qwen3-0.6B/)".into());
    }

    // 注意：sherpa-onnx Rust crate 的 Default 与 CLI 合理默认值不一致
    // （Default 的 temperature=1.0、prompt 为空会导致 speech-LLM 输出乱码），
    // 必须显式设置贪心采样 + 转写 prompt，与官方 CLI 一致。
    let mut config = OfflineRecognizerConfig::default();
    config.model_config.num_threads = 4;
    config.model_config.funasr_nano = OfflineFunASRNanoModelConfig {
        encoder_adaptor: Some(encoder_adaptor.to_string_lossy().to_string()),
        llm: Some(llm.to_string_lossy().to_string()),
        embedding: Some(embedding.to_string_lossy().to_string()),
        tokenizer: Some(tokenizer.to_string_lossy().to_string()),
        system_prompt: Some("You are a helpful assistant.".into()),
        user_prompt: Some("语音转写：".into()),
        max_new_tokens: 512,
        temperature: 1e-6,
        top_p: 0.8,
        seed: 42,
        itn: 1,
        language: Some(if language == "auto" { "".into() } else { language.to_string() }),
        hotwords: None,
    };

    OfflineRecognizer::create(&config)
        .ok_or_else(|| "FunASR Nano 初始化失败".to_string())
}

fn create_paraformer(dir: &Path) -> Result<OfflineRecognizer, String> {
    let model = dir.join("model.int8.onnx");
    let model = if model.exists() {
        model
    } else {
        let fp32 = dir.join("model.onnx");
        if fp32.exists() { fp32 } else { return Err("Paraformer 模型文件不存在 (model.int8.onnx)".into()); }
    };
    let tokens = dir.join("tokens.txt");
    if !tokens.exists() {
        return Err("tokens.txt 不存在".into());
    }

    let mut config = OfflineRecognizerConfig::default();
    config.model_config.num_threads = 4;
    config.model_config.paraformer = OfflineParaformerModelConfig {
        model: Some(model.to_string_lossy().to_string()),
    };
    config.model_config.tokens = Some(tokens.to_string_lossy().to_string());

    OfflineRecognizer::create(&config)
        .ok_or_else(|| "Paraformer 初始化失败".to_string())
}

fn create_qwen3_asr(dir: &Path, hotwords: &str) -> Result<OfflineRecognizer, String> {
    let conv_frontend = dir.join("conv_frontend.onnx");
    let encoder = dir.join("encoder.int8.onnx");
    let decoder = dir.join("decoder.int8.onnx");
    let tokenizer = dir.join("tokenizer");

    if !conv_frontend.exists() || !encoder.exists() || !decoder.exists() {
        return Err("Qwen3-ASR 模型文件不完整 (需要 conv_frontend.onnx / encoder.int8.onnx / decoder.int8.onnx)".into());
    }
    if !tokenizer.exists() {
        return Err("Qwen3-ASR tokenizer 目录不存在".into());
    }

    let mut config = OfflineRecognizerConfig::default();
    // Qwen3 是自回归 speech-LLM，大矩阵乘法可吃更多核；比轻量模型多给线程
    config.model_config.num_threads = asr_num_threads();
    config.model_config.qwen3_asr = OfflineQwen3ASRModelConfig {
        conv_frontend: Some(conv_frontend.to_string_lossy().to_string()),
        encoder: Some(encoder.to_string_lossy().to_string()),
        decoder: Some(decoder.to_string_lossy().to_string()),
        tokenizer: Some(tokenizer.to_string_lossy().to_string()),
        // 热词：逗号分隔，提升专有名词/术语识别（Qwen3 在创建时写入 config）
        hotwords: if hotwords.is_empty() { None } else { Some(hotwords.to_string()) },
        max_new_tokens: 256,
        ..Default::default()
    };

    OfflineRecognizer::create(&config)
        .ok_or_else(|| "Qwen3-ASR 初始化失败".to_string())
}

fn create_fire_red_ctc(dir: &Path) -> Result<OfflineRecognizer, String> {
    let model = dir.join("model.int8.onnx");
    if !model.exists() {
        return Err("FireRedASR2-CTC 模型文件不存在 (model.int8.onnx)".into());
    }
    let tokens = dir.join("tokens.txt");
    if !tokens.exists() {
        return Err("tokens.txt 不存在".into());
    }

    let mut config = OfflineRecognizerConfig::default();
    config.model_config.num_threads = 4;
    config.model_config.fire_red_asr_ctc = OfflineFireRedAsrCtcModelConfig {
        model: Some(model.to_string_lossy().to_string()),
    };
    config.model_config.tokens = Some(tokens.to_string_lossy().to_string());

    OfflineRecognizer::create(&config)
        .ok_or_else(|| "FireRedASR2-CTC 初始化失败".to_string())
}

fn create_fire_red_aed(dir: &Path) -> Result<OfflineRecognizer, String> {
    let encoder = dir.join("encoder.int8.onnx");
    let decoder = dir.join("decoder.int8.onnx");
    if !encoder.exists() || !decoder.exists() {
        return Err("FireRedASR2-AED 模型文件不完整 (需要 encoder.int8.onnx + decoder.int8.onnx)".into());
    }
    let tokens = dir.join("tokens.txt");
    if !tokens.exists() {
        return Err("tokens.txt 不存在".into());
    }

    let mut config = OfflineRecognizerConfig::default();
    config.model_config.num_threads = 4;
    config.model_config.fire_red_asr = OfflineFireRedAsrModelConfig {
        encoder: Some(encoder.to_string_lossy().to_string()),
        decoder: Some(decoder.to_string_lossy().to_string()),
    };
    config.model_config.tokens = Some(tokens.to_string_lossy().to_string());

    OfflineRecognizer::create(&config)
        .ok_or_else(|| "FireRedASR2-AED 初始化失败".to_string())
}

/// ASR 推理线程数：按机器逻辑核数动态取值，下限 2、上限 8。
/// 上限 8 是经验最优：混合架构 CPU（大核+小核）上线程超过 ~8 后，
/// 弱小核参与反而拖慢、线程调度开销上升，实测 8 优于 12/16。
fn asr_num_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get().clamp(2, 8) as i32)
        .unwrap_or(4)
}

/// 清洗 Qwen3-ASR 输出：剥离模型自带的模板前缀（如 "language Chinese<asr_text>"）。
/// 这是 Qwen3-ASR chat 模板未被 ONNX 导出剥离导致的已知现象（见 sherpa-onnx PR #3399）。
fn clean_asr_text(text: &str) -> String {
    let mut t = text;
    // 取最后一个 <asr_text> 之后的内容（前面是 "language XXX" 之类的前缀）
    if let Some(idx) = t.rfind("<asr_text>") {
        t = &t[idx + "<asr_text>".len()..];
    }
    t.trim().to_string()
}

const SR: usize = 16000;
/// Qwen3 单次推理的音频上限（秒）。超过则用 VAD 切分，避免超长上下文导致内存暴涨/崩溃。
/// ≤此值单次直出（更快、无 VAD 开销）；30s 内 0.6B 单次处理稳定不崩。
const QWEN3_MAX_SEC: usize = 30;
/// 切分后每段目标上限（秒）。与服务器端 MAX_CHUNK 一致，配合 VAD 得到 5~15s 段落。
const SEG_MAX_SEC: usize = 15;

/// 拼接分段文本：上一段若非以标点结尾则补空格，减少相邻词粘连。
fn join_segment(out: &mut String, text: &str) {
    let t = text.trim();
    if t.is_empty() {
        return;
    }
    if !out.is_empty() && !out.ends_with(|c: char| "，。！？、,.!?".contains(c)) {
        out.push(' ');
    }
    out.push_str(t);
}

/// 用 Silero VAD 在静音处切分为语音段（丢弃静音），失败返回 None。
fn vad_segments(samples: &[f32], vad_path: &Path) -> Option<Vec<Vec<f32>>> {
    let mut cfg = VadModelConfig::default();
    cfg.sample_rate = SR as i32;
    cfg.num_threads = 1;
    cfg.silero_vad = SileroVadModelConfig {
        model: Some(vad_path.to_string_lossy().to_string()),
        threshold: 0.5,
        min_silence_duration: 0.5,
        min_speech_duration: 0.25,
        window_size: 512,
        max_speech_duration: SEG_MAX_SEC as f32, // 单段硬上限，连续语音也会被切
    };
    let vad = VoiceActivityDetector::create(&cfg, 30.0)?;

    let window = 512usize;
    let mut i = 0usize;
    let mut segs: Vec<Vec<f32>> = Vec::new();
    while i < samples.len() {
        let end = (i + window).min(samples.len());
        vad.accept_waveform(&samples[i..end]);
        i = end;
        while let Some(seg) = vad.front() {
            segs.push(seg.samples().to_vec());
            vad.pop();
        }
    }
    vad.flush();
    while let Some(seg) = vad.front() {
        segs.push(seg.samples().to_vec());
        vad.pop();
    }
    if segs.is_empty() { None } else { Some(segs) }
}

/// 将相邻语音段合并到 ≤ max_samples，减少推理次数（5~15s 段落）。
fn merge_segments(segs: Vec<Vec<f32>>, max_samples: usize) -> Vec<Vec<f32>> {
    let mut out: Vec<Vec<f32>> = Vec::new();
    let mut cur: Vec<f32> = Vec::new();
    for s in segs {
        if s.len() > max_samples {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.push(s);
        } else if !cur.is_empty() && cur.len() + s.len() > max_samples {
            out.push(std::mem::replace(&mut cur, s));
        } else {
            cur.extend_from_slice(&s);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 固定窗口切分（VAD 不可用时的兜底），保证长音频不崩。
fn transcribe_fixed_window(samples: &[f32]) -> Result<String, String> {
    let chunk = SEG_MAX_SEC * SR;
    let mut out = String::new();
    let mut start = 0usize;
    while start < samples.len() {
        let end = (start + chunk).min(samples.len());
        let text = transcribe_with_cache(&samples[start..end])?;
        join_segment(&mut out, &text);
        start = end;
    }
    Ok(out)
}

/// 长音频转写：仅对自回归 speech-LLM（Qwen3）且 >QWEN3_MAX_SEC 的音频切分。
/// 优先 VAD 静音切分（5~15s），VAD 不可用时退回固定窗口。其他模型单次处理。
fn transcribe_long(model_id: &str, samples: &[f32], vad_path: Option<&Path>) -> Result<String, String> {
    let is_llm = model_id.starts_with("qwen3-asr");
    if !is_llm || samples.len() <= QWEN3_MAX_SEC * SR {
        return transcribe_with_cache(samples);
    }

    let vad_segs = vad_path
        .filter(|p| p.exists())
        .and_then(|p| vad_segments(samples, p));

    match vad_segs {
        Some(segs) => {
            let merged = merge_segments(segs, SEG_MAX_SEC * SR);
            let mut out = String::new();
            for seg in &merged {
                let text = transcribe_with_cache(seg)?;
                join_segment(&mut out, &text);
            }
            Ok(out)
        }
        None => transcribe_fixed_window(samples),
    }
}

/// 使用缓存的 recognizer 进行转录
fn transcribe_with_cache(samples: &[f32]) -> Result<String, String> {
    let cache = CACHE.lock().map_err(|e| format!("锁获取失败: {}", e))?;
    let entry = cache.as_ref().ok_or("模型未加载")?;

    let stream = entry.recognizer.create_stream();
    stream.accept_waveform(16000, samples);
    entry.recognizer.decode(&stream);
    let result = stream
        .get_result()
        .ok_or_else(|| "获取识别结果失败".to_string())?;

    Ok(clean_asr_text(&result.text))
}

#[tauri::command]
pub async fn preload_local_model(model_id: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        ensure_loaded(&model_id, "auto", "")?;
        Ok(format!("模型已加载 ({}ms)", start.elapsed().as_millis()))
    })
    .await
    .map_err(|e| format!("预加载异常: {}", e))?
}

/// 释放本地 ASR 模型占用的内存。切换到云 API / 服务器模式时调用，
/// 否则 sherpa-onnx recognizer（几百 MB ~ 数 GB）会一直占用到应用退出。
#[tauri::command]
pub async fn unload_local_model() -> Result<(), String> {
    tokio::task::spawn_blocking(|| {
        let mut cache = CACHE.lock().map_err(|e| format!("锁获取失败: {}", e))?;
        if let Some(entry) = cache.take() {
            log::info!("Unloading ASR model: {}", entry.model_id);
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("释放异常: {}", e))?
}

/// 解析 Silero VAD 模型路径：优先打包资源，兜底模型目录。
fn resolve_vad_model(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    use tauri::Manager;
    for cand in ["resources/silero_vad.onnx", "silero_vad.onnx"] {
        if let Ok(p) = app.path().resolve(cand, tauri::path::BaseDirectory::Resource) {
            if p.exists() {
                return Some(p);
            }
        }
    }
    let alt = super::downloader::models_dir().join("silero_vad.onnx");
    if alt.exists() {
        return Some(alt);
    }
    None
}

#[tauri::command]
pub async fn local_transcribe(
    app: tauri::AppHandle,
    audio_b64: String,
    model_id: String,
    language: Option<String>,
    hotwords: Option<Vec<String>>,
) -> Result<LocalAsrResult, String> {
    let pcm_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &audio_b64,
    )
    .map_err(|e| format!("base64 解码失败: {}", e))?;

    if pcm_bytes.len() < 2 {
        return Ok(LocalAsrResult { text: String::new(), elapsed_ms: 0 });
    }

    let samples: Vec<f32> = pcm_bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect();

    let vad_path = resolve_vad_model(&app);

    // 仅 Qwen3 使用热词；其它模型热词 key 置空，避免热词变化触发无谓重建
    let hotwords_key = if model_id.starts_with("qwen3-asr") {
        hotwords.map(|h| h.join(",")).unwrap_or_default()
    } else {
        String::new()
    };

    tokio::task::spawn_blocking(move || {
        let lang = language.as_deref().unwrap_or("auto");
        ensure_loaded(&model_id, lang, &hotwords_key)?;

        let start = Instant::now();
        let text = transcribe_long(&model_id, &samples, vad_path.as_deref())?;

        Ok(LocalAsrResult {
            text,
            elapsed_ms: start.elapsed().as_millis() as u64,
        })
    })
    .await
    .map_err(|e| format!("推理异常: {}", e))?
}

// ── 供 benchmark / test_audio 模块使用的公开接口 ──

pub fn ensure_loaded_pub(model_id: &str, language: &str) -> Result<(), String> {
    ensure_loaded(model_id, language, "")
}

pub fn get_cache_lock() -> Result<std::sync::MutexGuard<'static, Option<RecognizerCache>>, String> {
    CACHE.lock().map_err(|e| format!("锁获取失败: {}", e))
}

impl RecognizerCache {
    pub fn transcribe(&self, _sample_rate: u32, samples: &[f32]) -> String {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(16000, samples);
        self.recognizer.decode(&stream);
        stream
            .get_result()
            .map(|r| clean_asr_text(&r.text))
            .unwrap_or_default()
    }
}
