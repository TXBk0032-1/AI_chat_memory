use async_trait::async_trait;
use candle_core::{D, DType, Device, IndexOp, Module, Tensor};
use candle_nn::{Activation, Embedding, Linear, VarBuilder, linear_b as linear, ops::softmax};
use hf_hub::api::tokio::{ApiBuilder, Progress};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokenizers::Tokenizer;
use tokio::sync::Mutex;

use super::{BackendIdentity, EmbeddingBackend, ensure_dimensions};
use crate::{
    error::{AppError, Result},
    models::{
        EmbeddingBackendKind, EmbeddingHealth, LocalEmbeddingDType, LocalEmbeddingDevice,
        LocalEmbeddingSettings, ModelDownloadProgress,
    },
};

const DEFAULT_QUERY_INSTRUCTION: &str = "Instruct: Given a chat history search query, retrieve relevant conversation passages that answer the query\nQuery: ";
const MODEL_FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];
const MAX_SEQUENCE_LEN: usize = 2048;
/// Pull a wider pending window so we can pack similar-length chunks.
pub const LOCAL_INDEX_CANDIDATE_LIMIT: i64 = 512;
/// Absolute hard cap on items in one local embed call.
pub const LOCAL_INDEX_MAX_BATCH_ITEMS: usize = 64;
/// Fallback padded-token budget used by tests / generic callers.
pub const LOCAL_INDEX_TOKEN_BUDGET: usize = 12288;

pub type DownloadProgressCallback = Arc<dyn Fn(ModelDownloadProgress) + Send + Sync>;

pub struct LocalHarrierBackend {
    model_id: String,
    model_dir: PathBuf,
    dimensions: usize,
    preferred_device: LocalEmbeddingDevice,
    preferred_dtype: LocalEmbeddingDType,
    /// Serialize first-time weight loading across warm-up / indexer / query.
    load_gate: Mutex<()>,
    /// Single loaded model replica (CUDA preferred, CPU fallback).
    state: Arc<std::sync::Mutex<Option<LoadedModel>>>,
    runtime_device: Arc<std::sync::Mutex<String>>,
    runtime_dtype: Arc<std::sync::Mutex<String>>,
}

struct LoadedModel {
    tokenizer: Tokenizer,
    model: HarrierModel,
    device: Device,
    device_label: String,
    dtype_label: String,
}

impl LocalHarrierBackend {
    pub async fn open(
        model_id: String,
        model_dir: PathBuf,
        settings: &LocalEmbeddingSettings,
    ) -> Result<Self> {
        tokio::fs::create_dir_all(&model_dir)
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        // Keep startup cheap: only validate directory here and load weights on first use.
        Ok(Self {
            model_id,
            model_dir,
            dimensions: 640,
            preferred_device: settings.device.clone(),
            preferred_dtype: settings.dtype.clone(),
            load_gate: Mutex::new(()),
            state: Arc::new(std::sync::Mutex::new(None)),
            runtime_device: Arc::new(std::sync::Mutex::new("unloaded".into())),
            runtime_dtype: Arc::new(std::sync::Mutex::new("unloaded".into())),
        })
    }

    pub fn is_loaded(&self) -> bool {
        self.state
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }

    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    pub fn runtime_device_label(&self) -> String {
        self.runtime_device
            .lock()
            .map(|value| value.clone())
            .unwrap_or_else(|_| "unknown".into())
    }

    pub fn runtime_dtype_label(&self) -> String {
        self.runtime_dtype
            .lock()
            .map(|value| value.clone())
            .unwrap_or_else(|_| "unknown".into())
    }

    pub async fn ensure_model_files_with_progress(
        &self,
        on_progress: Option<DownloadProgressCallback>,
    ) -> Result<()> {
        if model_files_present(&self.model_dir) {
            if let Some(on_progress) = &on_progress {
                on_progress(ModelDownloadProgress {
                    stage: "done".into(),
                    file: None,
                    file_index: MODEL_FILES.len(),
                    file_count: MODEL_FILES.len(),
                    downloaded_bytes: 0,
                    total_bytes: None,
                    fraction: 1.0,
                    message: "本地模型已就绪".into(),
                });
            }
            return Ok(());
        }
        download_model(&self.model_id, &self.model_dir, on_progress).await
    }

    pub async fn import_model_dir(&self, source: &Path) -> Result<()> {
        if !source.is_dir() {
            return Err(AppError::Configuration(
                "本地模型路径必须是包含 config.json / tokenizer.json / model.safetensors 的目录"
                    .into(),
            ));
        }
        for file in MODEL_FILES {
            let from = source.join(file);
            if !from.exists() {
                return Err(AppError::Configuration(format!("导入目录缺少 {file}")));
            }
            let to = self.model_dir.join(file);
            tokio::fs::copy(&from, &to)
                .await
                .map_err(|error| AppError::Configuration(error.to_string()))?;
        }
        let pooling = source.join("1_Pooling").join("config.json");
        if pooling.exists() {
            let dest_dir = self.model_dir.join("1_Pooling");
            tokio::fs::create_dir_all(&dest_dir).await.ok();
            let _ = tokio::fs::copy(pooling, dest_dir.join("config.json")).await;
        }
        if let Ok(mut guard) = self.state.lock() {
            *guard = None;
        }
        self.ensure_loaded().await?;
        Ok(())
    }

    async fn ensure_loaded(&self) -> Result<()> {
        if self.is_loaded() {
            return Ok(());
        }
        let _gate = self.load_gate.lock().await;
        if self.is_loaded() {
            return Ok(());
        }
        if !model_files_present(&self.model_dir) {
            return Err(AppError::Configuration(
                "本地 embedding 模型尚未准备好，请先下载或导入模型".into(),
            ));
        }
        let model_dir = self.model_dir.clone();
        let preferred_device = self.preferred_device.clone();
        let preferred_dtype = self.preferred_dtype.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            load_model(&model_dir, preferred_device, preferred_dtype)
        })
        .await
        .map_err(|error| AppError::Configuration(format!("加载本地模型任务失败: {error}")))?
        .map_err(|error| AppError::Configuration(error.to_string()))?;
        if let Ok(mut device) = self.runtime_device.lock() {
            *device = loaded.device_label.clone();
        }
        if let Ok(mut dtype) = self.runtime_dtype.lock() {
            *dtype = loaded.dtype_label.clone();
        }
        let mut guard = self
            .state
            .lock()
            .map_err(|_| AppError::Configuration("local model state lock poisoned".into()))?;
        if guard.is_none() {
            tracing::info!(
                device = %loaded.device_label,
                dtype = %loaded.dtype_label,
                "local embedding model ready"
            );
            *guard = Some(loaded);
        }
        Ok(())
    }

    async fn embed(&self, texts: &[String], is_query: bool) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_loaded().await?;
        let state = Arc::clone(&self.state);
        let texts = texts.to_vec();
        let dimensions = self.dimensions;
        let vectors = tokio::task::spawn_blocking(move || {
            let mut guard = state
                .lock()
                .map_err(|_| AppError::Configuration("local model state lock poisoned".into()))?;
            let loaded = guard
                .as_mut()
                .ok_or_else(|| AppError::Configuration("local model not loaded".into()))?;
            embed_batch_resilient(loaded, &texts, is_query, dimensions)
        })
        .await
        .map_err(|error| {
            AppError::Configuration(format!("本地 embedding 推理任务失败: {error}"))
        })??;
        Ok(vectors)
    }
}

#[async_trait]
impl EmbeddingBackend for LocalHarrierBackend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            backend: EmbeddingBackendKind::Local,
            backend_id: "local".into(),
            model_id: self.model_id.clone(),
            dimensions: self.dimensions,
        }
    }

    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts, false).await
    }

    async fn embed_queries(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts, true).await
    }

    async fn healthcheck(&self) -> Result<EmbeddingHealth> {
        // Avoid loading the 500MB+ model during startup/status polls.
        if !model_files_present(&self.model_dir) {
            return Ok(EmbeddingHealth {
                ok: false,
                backend: EmbeddingBackendKind::Local,
                model_id: self.model_id.clone(),
                dimensions: Some(self.dimensions),
                message: "本地模型未下载或未导入".into(),
            });
        }
        let device = self.runtime_device_label();
        let dtype = self.runtime_dtype_label();
        Ok(EmbeddingHealth {
            ok: true,
            backend: EmbeddingBackendKind::Local,
            model_id: self.model_id.clone(),
            dimensions: Some(self.dimensions),
            message: if self.is_loaded() {
                format!("本地模型已加载（{device}/{dtype}）")
            } else {
                format!(
                    "本地模型文件已就绪（偏好 {}/{})",
                    device_pref_label(&self.preferred_device),
                    dtype_pref_label(&self.preferred_dtype)
                )
            },
        })
    }

    fn is_ready(&self) -> bool {
        self.is_loaded()
    }

    fn runtime_device(&self) -> Option<String> {
        Some(self.runtime_device_label())
    }

    fn runtime_dtype(&self) -> Option<String> {
        Some(self.runtime_dtype_label())
    }
}

pub fn model_files_present(dir: &Path) -> bool {
    MODEL_FILES.iter().all(|file| dir.join(file).exists())
}

async fn download_model(
    model_id: &str,
    model_dir: &Path,
    on_progress: Option<DownloadProgressCallback>,
) -> Result<()> {
    tokio::fs::create_dir_all(model_dir)
        .await
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    if let Some(on_progress) = &on_progress {
        on_progress(ModelDownloadProgress {
            stage: "starting".into(),
            file: None,
            file_index: 0,
            file_count: MODEL_FILES.len(),
            downloaded_bytes: 0,
            total_bytes: None,
            fraction: 0.0,
            message: format!("开始从 Hugging Face 下载 {model_id}"),
        });
    }

    let api = ApiBuilder::new()
        .with_progress(false)
        .build()
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    let repo = api.model(model_id.to_string());
    let file_count = MODEL_FILES.len();

    for (file_index, file) in MODEL_FILES.iter().enumerate() {
        if let Some(on_progress) = &on_progress {
            on_progress(ModelDownloadProgress {
                stage: "file".into(),
                file: Some((*file).into()),
                file_index,
                file_count,
                downloaded_bytes: 0,
                total_bytes: None,
                fraction: file_index as f32 / file_count as f32,
                message: format!("正在下载 {file} ({}/{file_count})", file_index + 1),
            });
        }

        let progress = CallbackProgress {
            on_progress: on_progress.clone(),
            file: (*file).into(),
            file_index,
            file_count,
            downloaded_bytes: 0,
            total_bytes: None,
        };
        let path = repo
            .download_with_progress(file, progress)
            .await
            .map_err(|error| AppError::Configuration(format!("download {file} failed: {error}")))?;

        if let Some(on_progress) = &on_progress {
            on_progress(ModelDownloadProgress {
                stage: "copying".into(),
                file: Some((*file).into()),
                file_index,
                file_count,
                downloaded_bytes: 0,
                total_bytes: None,
                fraction: (file_index as f32 + 0.95) / file_count as f32,
                message: format!("正在写入本地缓存 {file}"),
            });
        }
        let destination = model_dir.join(file);
        tokio::fs::copy(&path, &destination)
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
    }

    if let Some(on_progress) = &on_progress {
        on_progress(ModelDownloadProgress {
            stage: "done".into(),
            file: None,
            file_index: file_count,
            file_count,
            downloaded_bytes: 0,
            total_bytes: None,
            fraction: 1.0,
            message: format!("模型已保存到 {}", model_dir.display()),
        });
    }
    Ok(())
}

#[derive(Clone)]
struct CallbackProgress {
    on_progress: Option<DownloadProgressCallback>,
    file: String,
    file_index: usize,
    file_count: usize,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
}

impl CallbackProgress {
    fn emit(&self, stage: &str, message: String) {
        let Some(on_progress) = &self.on_progress else {
            return;
        };
        let file_fraction = match self.total_bytes {
            Some(total) if total > 0 => {
                (self.downloaded_bytes as f32 / total as f32).clamp(0.0, 1.0)
            }
            _ => 0.0,
        };
        let fraction = (self.file_index as f32 + file_fraction) / self.file_count.max(1) as f32;
        on_progress(ModelDownloadProgress {
            stage: stage.into(),
            file: Some(self.file.clone()),
            file_index: self.file_index,
            file_count: self.file_count,
            downloaded_bytes: self.downloaded_bytes,
            total_bytes: self.total_bytes,
            fraction: fraction.clamp(0.0, 0.999),
            message,
        });
    }
}

impl Progress for CallbackProgress {
    async fn init(&mut self, size: usize, _filename: &str) {
        self.total_bytes = if size == 0 { None } else { Some(size as u64) };
        self.downloaded_bytes = 0;
        self.emit(
            "file",
            format!(
                "开始下载 {} ({}/{})",
                self.file,
                self.file_index + 1,
                self.file_count
            ),
        );
    }

    async fn update(&mut self, size: usize) {
        self.downloaded_bytes = self.downloaded_bytes.saturating_add(size as u64);
        let total = self
            .total_bytes
            .map(format_bytes)
            .unwrap_or_else(|| "?".into());
        self.emit(
            "file",
            format!(
                "下载 {}：{} / {}",
                self.file,
                format_bytes(self.downloaded_bytes),
                total
            ),
        );
    }

    async fn finish(&mut self) {
        if let Some(total) = self.total_bytes {
            self.downloaded_bytes = total;
        }
        self.emit("file", format!("{} 下载完成", self.file));
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.2} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.0} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

fn device_pref_label(device: &LocalEmbeddingDevice) -> &'static str {
    match device {
        LocalEmbeddingDevice::Auto => "auto",
        LocalEmbeddingDevice::Cuda => "cuda",
        LocalEmbeddingDevice::Cpu => "cpu",
    }
}

fn dtype_pref_label(dtype: &LocalEmbeddingDType) -> &'static str {
    match dtype {
        LocalEmbeddingDType::Auto => "auto",
        LocalEmbeddingDType::F16 => "f16",
        LocalEmbeddingDType::F32 => "f32",
    }
}

fn select_device(preferred: &LocalEmbeddingDevice) -> Result<(Device, String)> {
    match preferred {
        LocalEmbeddingDevice::Cpu => Ok((Device::Cpu, "CPU".into())),
        LocalEmbeddingDevice::Cuda => {
            let device = Device::new_cuda(0).map_err(|error| {
                AppError::Configuration(format!("无法初始化 CUDA 设备: {error}"))
            })?;
            Ok((device, "CUDA:0".into()))
        }
        LocalEmbeddingDevice::Auto => match Device::new_cuda(0) {
            Ok(device) => Ok((device, "CUDA:0".into())),
            Err(error) => {
                tracing::warn!(%error, "CUDA unavailable; falling back to CPU for local embeddings");
                Ok((Device::Cpu, "CPU".into()))
            }
        },
    }
}

fn select_dtype(preferred: &LocalEmbeddingDType, device: &Device) -> (DType, String) {
    let wants_f16 = match preferred {
        LocalEmbeddingDType::F16 => true,
        // CUDA F16 currently tends to produce NaNs in this harrier port; prefer F32 by default.
        LocalEmbeddingDType::F32 | LocalEmbeddingDType::Auto => false,
    };
    let _ = device;
    if wants_f16 {
        (DType::F16, "F16".into())
    } else {
        (DType::F32, "F32".into())
    }
}

fn load_model(
    model_dir: &Path,
    preferred_device: LocalEmbeddingDevice,
    preferred_dtype: LocalEmbeddingDType,
) -> Result<LoadedModel> {
    let (device, device_label) = select_device(&preferred_device)?;
    let (dtype, dtype_label) = select_dtype(&preferred_dtype, &device);
    let config_text = std::fs::read_to_string(model_dir.join("config.json"))
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    let config: HarrierConfig = serde_json::from_str(&config_text)
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    let tokenizer = Tokenizer::from_file(model_dir.join("tokenizer.json"))
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    let try_load = |device: &Device, dtype: DType| -> Result<HarrierModel> {
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[model_dir.join("model.safetensors")],
                dtype,
                device,
            )
            .map_err(candle_err)?
        };
        HarrierModel::load(&config, vb).map_err(candle_err)
    };

    let mut candidates: Vec<(Device, String, DType, String)> = Vec::new();
    candidates.push((
        device.clone(),
        device_label.clone(),
        dtype,
        dtype_label.clone(),
    ));
    if !matches!(device, Device::Cpu) && matches!(dtype, DType::F16) {
        candidates.push((
            device.clone(),
            device_label.clone(),
            DType::F32,
            "F32".into(),
        ));
    }
    if !matches!(device, Device::Cpu) {
        candidates.push((Device::Cpu, "CPU".into(), DType::F32, "F32".into()));
    }

    let mut last_error: Option<AppError> = None;
    for (candidate_device, candidate_device_label, candidate_dtype, candidate_dtype_label) in
        candidates
    {
        match try_load(&candidate_device, candidate_dtype) {
            Ok(model) => {
                let mut loaded = LoadedModel {
                    tokenizer: tokenizer.clone(),
                    model,
                    device: candidate_device,
                    device_label: candidate_device_label.clone(),
                    dtype_label: candidate_dtype_label.clone(),
                };
                match warmup_model(&mut loaded) {
                    Ok(()) => {
                        if candidate_device_label != device_label
                            || candidate_dtype_label != dtype_label
                        {
                            tracing::warn!(
                                requested_device = %device_label,
                                requested_dtype = %dtype_label,
                                actual_device = %candidate_device_label,
                                actual_dtype = %candidate_dtype_label,
                                "local embedding fell back after load/warmup"
                            );
                        }
                        return Ok(loaded);
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            device = %candidate_device_label,
                            dtype = %candidate_dtype_label,
                            "local embedding warmup failed; trying next device/dtype"
                        );
                        last_error = Some(error);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    device = %candidate_device_label,
                    dtype = %candidate_dtype_label,
                    "local embedding load failed; trying next device/dtype"
                );
                last_error = Some(error);
            }
        }
    }

    Err(last_error
        .unwrap_or_else(|| AppError::Configuration("failed to load local embedding model".into())))
}

fn warmup_model(loaded: &mut LoadedModel) -> Result<()> {
    // Force a tiny forward pass so CUDA PTX/runtime mismatches and NaN-producing
    // kernels fail during load, where we can still fall back to a safer device/dtype.
    let vectors = embed_batch(loaded, &["warmup".into()], false, 640)?;
    if vectors.len() != 1 || vectors[0].len() != 640 {
        return Err(AppError::Configuration(
            "local embedding warmup returned unexpected shape".into(),
        ));
    }
    let norm = vectors[0].iter().map(|v| v * v).sum::<f32>().sqrt();
    if !norm.is_finite() || (norm - 1.0).abs() > 5e-2 {
        return Err(AppError::Configuration(format!(
            "local embedding warmup produced invalid vector norm={norm}"
        )));
    }
    Ok(())
}

/// Cheap token estimate for batch packing.
/// Chat archives are Chinese-heavy, so char count is a stable upper-ish proxy.
pub fn estimate_token_count(text: &str) -> usize {
    text.chars().count().clamp(1, MAX_SEQUENCE_LEN)
}

fn length_band(tokens: usize) -> u8 {
    match tokens {
        0..=96 => 0,
        97..=192 => 1,
        193..=320 => 2,
        321..=512 => 3,
        _ => 4,
    }
}

/// Short texts should fill the GPU; long texts stay restrained because
/// attention cost grows roughly with max_len^2.
/// Returns (token_budget, max_items, preferred_min_items).
fn band_limits(band: u8) -> (usize, usize, usize) {
    match band {
        0 => (12_288, 64, 24), // short: force large packs when available
        1 => (11_264, 40, 16),
        2 => (9_216, 24, 8),
        3 => (8_192, 16, 4),
        _ => (7_168, 10, 1), // long: restrained
    }
}

fn pack_window_in_band(
    order: &[usize],
    estimated_tokens: &[usize],
    band_start: usize,
    band_end: usize,
    budget: usize,
    max_items: usize,
    preferred_min: usize,
) -> (usize, usize, f64) {
    let mut best_start = band_start;
    let mut best_end = band_start + 1;
    let mut best_score = f64::NEG_INFINITY;

    for start in band_start..band_end {
        let mut max_len = estimated_tokens[order[start]];
        let mut sum_tokens = 0usize;
        let max_end = (start + max_items).min(band_end);
        for end in (start + 1)..=max_end {
            let tokens = estimated_tokens[order[end - 1]];
            max_len = max_len.max(tokens);
            sum_tokens += tokens;
            let items = end - start;
            let padded = max_len.saturating_mul(items);
            if padded > budget && items > 1 {
                break;
            }
            let pad_ratio = 1.0 - (sum_tokens as f64 / padded.max(1) as f64);
            let fullness = (items as f64 / preferred_min.max(1) as f64).clamp(0.05, 2.0);
            // Reward more items and lower pad; lightly punish longer sequences.
            let score = (items as f64).powf(1.4) * (1.0 - pad_ratio).powf(1.15) * fullness
                / (max_len as f64).max(1.0).powf(0.9);
            if score > best_score {
                best_score = score;
                best_start = start;
                best_end = end;
            }
        }
    }

    // Expand under-filled windows when the band still has room.
    let chosen_items = best_end - best_start;
    let band_len = band_end - band_start;
    if chosen_items < preferred_min && band_len > chosen_items {
        let mut max_len = 0usize;
        let mut end = best_start;
        while end < band_end && (end - best_start) < max_items {
            let tokens = estimated_tokens[order[end]];
            let next_max = max_len.max(tokens);
            let next_items = end - best_start + 1;
            let padded = next_max.saturating_mul(next_items);
            if padded > budget && next_items > 1 {
                break;
            }
            max_len = next_max;
            end += 1;
        }
        if end > best_end {
            best_end = end;
            // Recompute a simple score for the expanded window.
            let mut max_len = 0usize;
            let mut sum_tokens = 0usize;
            for idx in &order[best_start..best_end] {
                let tokens = estimated_tokens[*idx];
                max_len = max_len.max(tokens);
                sum_tokens += tokens;
            }
            let items = best_end - best_start;
            let padded = max_len.saturating_mul(items).max(1);
            let pad_ratio = 1.0 - (sum_tokens as f64 / padded as f64);
            let fullness = (items as f64 / preferred_min.max(1) as f64).clamp(0.05, 2.0);
            best_score = (items as f64).powf(1.4) * (1.0 - pad_ratio).powf(1.15) * fullness
                / (max_len as f64).max(1.0).powf(0.9);
        }
    }

    (best_start, best_end, best_score)
}

/// Pick a dense, similar-length subset under a padded-token budget.
/// Returns indices into the candidate slice (stable ascending order).
pub fn plan_local_index_batch(estimated_tokens: &[usize]) -> Vec<usize> {
    let n = estimated_tokens.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![0];
    }

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&idx| estimated_tokens[idx]);

    // Restrict packing to one length band at a time so short/long never mix.
    let mut band_ranges: Vec<(usize, usize, u8)> = Vec::new();
    let mut range_start = 0usize;
    let mut current_band = length_band(estimated_tokens[order[0]]);
    for i in 1..n {
        let band = length_band(estimated_tokens[order[i]]);
        if band != current_band {
            band_ranges.push((range_start, i, current_band));
            range_start = i;
            current_band = band;
        }
    }
    band_ranges.push((range_start, n, current_band));

    // Prefer short band only when it can form a real pack; otherwise choose the densest band.
    let mut selected: Option<(usize, usize, f64)> = None;
    if let Some(&(band_start, band_end, band)) = band_ranges.iter().find(|(_, _, b)| *b == 0) {
        let (budget, max_items, preferred_min) = band_limits(band);
        let band_len = band_end - band_start;
        if band_len >= preferred_min {
            let packed = pack_window_in_band(
                &order,
                estimated_tokens,
                band_start,
                band_end,
                budget,
                max_items.min(LOCAL_INDEX_MAX_BATCH_ITEMS),
                preferred_min,
            );
            if packed.1 - packed.0 >= preferred_min {
                selected = Some(packed);
            }
        }
    }

    if selected.is_none() {
        let mut best: Option<(usize, usize, f64)> = None;
        for &(band_start, band_end, band) in &band_ranges {
            let (budget, max_items, preferred_min) = band_limits(band);
            let packed = pack_window_in_band(
                &order,
                estimated_tokens,
                band_start,
                band_end,
                budget,
                max_items.min(LOCAL_INDEX_MAX_BATCH_ITEMS),
                preferred_min,
            );
            let items = packed.1 - packed.0;
            // Skip sparse tiny packs if a denser band exists later, unless this is the only band.
            if items < 4 && band_ranges.len() > 1 && band <= 1 {
                // still consider, but with lower score already from pack_window
            }
            match best {
                None => best = Some(packed),
                Some((_, _, best_score)) if packed.2 > best_score => best = Some(packed),
                _ => {}
            }
        }
        selected = best;
    }

    let (best_start, best_end, _) = selected.unwrap_or((0, 1, 0.0));
    let mut chosen: Vec<usize> = order[best_start..best_end].to_vec();
    chosen.sort_unstable();
    chosen
}

fn embed_batch(
    loaded: &mut LoadedModel,
    texts: &[String],
    is_query: bool,
    dimensions: usize,
) -> Result<Vec<Vec<f32>>> {
    let total_started = std::time::Instant::now();
    let tokenize_started = std::time::Instant::now();
    let mut token_batches = Vec::with_capacity(texts.len());
    let mut lengths = Vec::with_capacity(texts.len());
    let mut max_len = 1usize;
    let mut total_tokens = 0usize;
    for text in texts {
        let prepared = if is_query {
            format!("{DEFAULT_QUERY_INSTRUCTION}{text}")
        } else {
            text.clone()
        };
        let encoding = loaded
            .tokenizer
            .encode(prepared, true)
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        let mut ids = encoding.get_ids().to_vec();
        if ids.is_empty() {
            return Err(AppError::Configuration(
                "tokenizer produced empty input".into(),
            ));
        }
        if ids.len() > MAX_SEQUENCE_LEN {
            ids.truncate(MAX_SEQUENCE_LEN);
        }
        max_len = max_len.max(ids.len());
        total_tokens += ids.len();
        lengths.push(ids.len());
        token_batches.push(ids);
    }
    let tokenize_ms = tokenize_started.elapsed().as_millis();

    let pack_started = std::time::Instant::now();
    let mut flat = Vec::with_capacity(token_batches.len() * max_len);
    for ids in &token_batches {
        flat.extend_from_slice(ids);
        if ids.len() < max_len {
            flat.extend(std::iter::repeat_n(0u32, max_len - ids.len()));
        }
    }
    let input = Tensor::from_vec(flat, (token_batches.len(), max_len), &loaded.device)
        .map_err(candle_err)?;
    let pack_ms = pack_started.elapsed().as_millis();

    let forward_started = std::time::Instant::now();
    let embeddings = loaded
        .model
        .embed_tokens_batch(&input, &lengths)
        .map_err(candle_err)?;
    // CUDA kernels are async; synchronize so forward_ms reflects real GPU work.
    loaded.device.synchronize().map_err(candle_err)?;
    let forward_ms = forward_started.elapsed().as_millis();

    let host_started = std::time::Instant::now();
    let embeddings = embeddings.to_dtype(DType::F32).map_err(candle_err)?;
    let vectors = embeddings.to_vec2::<f32>().map_err(candle_err)?;
    ensure_dimensions(&vectors, dimensions)?;
    ensure_finite_vectors(&vectors)?;
    let host_ms = host_started.elapsed().as_millis();

    let batch_size = token_batches.len();
    let avg_tokens = if batch_size == 0 {
        0.0
    } else {
        total_tokens as f64 / batch_size as f64
    };
    let pad_ratio = if max_len == 0 || batch_size == 0 {
        0.0
    } else {
        1.0 - (total_tokens as f64 / (batch_size as f64 * max_len as f64))
    };
    let total_ms = total_started.elapsed().as_millis();
    let tokens_per_sec = if total_ms == 0 {
        total_tokens as f64
    } else {
        (total_tokens as f64) * 1000.0 / (total_ms as f64)
    };
    let chunks_per_sec = if total_ms == 0 {
        batch_size as f64
    } else {
        (batch_size as f64) * 1000.0 / (total_ms as f64)
    };
    tracing::info!(
        batch_size,
        max_len,
        total_tokens,
        avg_tokens,
        pad_ratio,
        tokenize_ms,
        pack_ms,
        forward_ms,
        host_ms,
        total_ms,
        tokens_per_sec,
        chunks_per_sec,
        device = %loaded.device_label,
        dtype = %loaded.dtype_label,
        is_query,
        "local embedding batch profile"
    );
    Ok(vectors)
}

fn embed_batch_resilient(
    loaded: &mut LoadedModel,
    texts: &[String],
    is_query: bool,
    dimensions: usize,
) -> Result<Vec<Vec<f32>>> {
    match embed_batch(loaded, texts, is_query, dimensions) {
        Ok(vectors) => Ok(vectors),
        Err(error) if texts.len() > 1 => {
            tracing::warn!(
                %error,
                batch_size = texts.len(),
                "local embedding batch failed; splitting and retrying"
            );
            let mid = texts.len() / 2;
            let mut left = embed_batch_resilient(loaded, &texts[..mid], is_query, dimensions)?;
            let right = embed_batch_resilient(loaded, &texts[mid..], is_query, dimensions)?;
            left.extend(right);
            Ok(left)
        }
        Err(error) => Err(error),
    }
}

fn candle_err(error: impl ToString) -> AppError {
    AppError::Configuration(error.to_string())
}

fn ensure_finite_vectors(vectors: &[Vec<f32>]) -> Result<()> {
    for (row_idx, vector) in vectors.iter().enumerate() {
        if vector.iter().any(|value| !value.is_finite()) {
            return Err(AppError::Configuration(format!(
                "embedding vector {row_idx} contains non-finite values"
            )));
        }
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if !norm.is_finite() || norm < 1e-6 {
            return Err(AppError::Configuration(format!(
                "embedding vector {row_idx} has invalid L2 norm {norm}"
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Deserialize)]
struct HarrierConfig {
    hidden_size: usize,
    intermediate_size: usize,
    num_hidden_layers: usize,
    num_attention_heads: usize,
    num_key_value_heads: usize,
    head_dim: usize,
    rms_norm_eps: f64,
    vocab_size: usize,
    rope_theta: f64,
    #[serde(default = "default_rope_local")]
    rope_local_base_freq: f64,
    #[serde(default = "default_query_scalar")]
    query_pre_attn_scalar: usize,
    #[serde(default = "default_sliding_pattern")]
    sliding_window_pattern: usize,
    #[serde(default = "default_sliding_window")]
    sliding_window: usize,
    #[serde(default = "default_max_pos")]
    max_position_embeddings: usize,
    #[serde(default)]
    attention_bias: bool,
}

fn default_query_scalar() -> usize {
    256
}
fn default_sliding_window() -> usize {
    512
}
fn default_sliding_pattern() -> usize {
    1
}
fn default_rope_local() -> f64 {
    10_000.0
}
fn default_max_pos() -> usize {
    32_768
}

struct HarrierModel {
    embed_tokens: Embedding,
    layers: Vec<DecoderLayer>,
    norm: RmsNorm,
    hidden_size: usize,
}

impl HarrierModel {
    fn load(cfg: &HarrierConfig, vb: VarBuilder) -> candle_core::Result<Self> {
        // Published harrier weights are stored without a leading "model." prefix.
        let vb_m = if vb.contains_tensor("model.embed_tokens.weight") {
            vb.pp("model")
        } else {
            vb
        };
        let embed_tokens =
            candle_nn::embedding(cfg.vocab_size, cfg.hidden_size, vb_m.pp("embed_tokens"))?;
        let mut layers = Vec::with_capacity(cfg.num_hidden_layers);
        let vb_l = vb_m.pp("layers");
        for layer_idx in 0..cfg.num_hidden_layers {
            // pattern=1 means every layer is full attention for this checkpoint.
            let sliding = (layer_idx + 1) % cfg.sliding_window_pattern.max(1) > 0;
            layers.push(DecoderLayer::load(
                cfg,
                vb_l.pp(layer_idx),
                sliding.then_some(cfg.sliding_window),
            )?);
        }
        let norm = RmsNorm::new(cfg.hidden_size, cfg.rms_norm_eps, vb_m.pp("norm"))?;
        Ok(Self {
            embed_tokens,
            layers,
            norm,
            hidden_size: cfg.hidden_size,
        })
    }

    fn embed_tokens_batch(
        &self,
        input_ids: &Tensor,
        lengths: &[usize],
    ) -> candle_core::Result<Tensor> {
        let (b_size, seq_len) = input_ids.dims2()?;
        if lengths.len() != b_size {
            candle_core::bail!(
                "batch length mismatch: ids={b_size}, lengths={}",
                lengths.len()
            );
        }
        let mut xs = self.embed_tokens.forward(input_ids)?;
        xs = (xs * (self.hidden_size as f64).sqrt())?;
        let attention_mask = causal_mask(b_size, seq_len, lengths, xs.device(), xs.dtype())?;
        for layer in &self.layers {
            xs = layer.forward(&xs, Some(&attention_mask))?;
        }
        let xs = self.norm.forward(&xs)?;
        // last valid token pooling for padded batches
        let mut pooled_rows = Vec::with_capacity(b_size);
        for (batch_idx, length) in lengths.iter().enumerate() {
            let token_idx = length.saturating_sub(1).min(seq_len.saturating_sub(1));
            pooled_rows.push(xs.i((batch_idx, token_idx, ..))?);
        }
        let pooled = Tensor::stack(&pooled_rows, 0)?;
        let norm = pooled
            .sqr()?
            .sum_keepdim(D::Minus1)?
            .sqrt()?
            .clamp(1e-12, f64::INFINITY)?;
        pooled.broadcast_div(&norm)
    }
}

#[derive(Debug, Clone)]
struct RmsNorm {
    weight: Tensor,
    eps: f64,
}

impl RmsNorm {
    fn new(dim: usize, eps: f64, vb: VarBuilder) -> candle_core::Result<Self> {
        let weight = vb.get(dim, "weight")?;
        Ok(Self { weight, eps })
    }
}

impl Module for RmsNorm {
    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let x_dtype = x.dtype();
        let internal_dtype = match x_dtype {
            DType::F16 | DType::BF16 => DType::F32,
            d => d,
        };
        let hidden_size = x.dim(D::Minus1)?;
        let x = x.to_dtype(internal_dtype)?;
        let norm_x = (x.sqr()?.sum_keepdim(D::Minus1)? / hidden_size as f64)?;
        let x_normed = x.broadcast_div(&(norm_x + self.eps)?.sqrt()?)?;
        // Gemma-style RMSNorm stores weight-1.
        x_normed
            .to_dtype(x_dtype)?
            .broadcast_mul(&(&self.weight + 1.0)?)
    }
}

struct DecoderLayer {
    self_attn: Attention,
    mlp: Mlp,
    input_layernorm: RmsNorm,
    post_attention_layernorm: RmsNorm,
    pre_feedforward_layernorm: RmsNorm,
    post_feedforward_layernorm: RmsNorm,
}

impl DecoderLayer {
    fn load(
        cfg: &HarrierConfig,
        vb: VarBuilder,
        sliding_window: Option<usize>,
    ) -> candle_core::Result<Self> {
        Ok(Self {
            self_attn: Attention::load(cfg, vb.pp("self_attn"), sliding_window)?,
            mlp: Mlp::load(cfg, vb.pp("mlp"))?,
            input_layernorm: RmsNorm::new(
                cfg.hidden_size,
                cfg.rms_norm_eps,
                vb.pp("input_layernorm"),
            )?,
            post_attention_layernorm: RmsNorm::new(
                cfg.hidden_size,
                cfg.rms_norm_eps,
                vb.pp("post_attention_layernorm"),
            )?,
            pre_feedforward_layernorm: RmsNorm::new(
                cfg.hidden_size,
                cfg.rms_norm_eps,
                vb.pp("pre_feedforward_layernorm"),
            )?,
            post_feedforward_layernorm: RmsNorm::new(
                cfg.hidden_size,
                cfg.rms_norm_eps,
                vb.pp("post_feedforward_layernorm"),
            )?,
        })
    }

    fn forward(&self, xs: &Tensor, attention_mask: Option<&Tensor>) -> candle_core::Result<Tensor> {
        let residual = xs;
        let xs = self.input_layernorm.forward(xs)?;
        let xs = self.self_attn.forward(&xs, attention_mask)?;
        let xs = self.post_attention_layernorm.forward(&xs)?;
        let xs = (xs + residual)?;
        let residual = &xs;
        let xs = self.pre_feedforward_layernorm.forward(&xs)?;
        let xs = self.mlp.forward(&xs)?;
        let xs = self.post_feedforward_layernorm.forward(&xs)?;
        residual + xs
    }
}

struct Attention {
    q_proj: Linear,
    k_proj: Linear,
    v_proj: Linear,
    o_proj: Linear,
    q_norm: RmsNorm,
    k_norm: RmsNorm,
    num_heads: usize,
    num_kv_heads: usize,
    head_dim: usize,
    scale: f64,
    rotary_emb: RotaryEmbedding,
}

impl Attention {
    fn load(
        cfg: &HarrierConfig,
        vb: VarBuilder,
        sliding_window: Option<usize>,
    ) -> candle_core::Result<Self> {
        Ok(Self {
            q_proj: linear(
                cfg.hidden_size,
                cfg.num_attention_heads * cfg.head_dim,
                cfg.attention_bias,
                vb.pp("q_proj"),
            )?,
            k_proj: linear(
                cfg.hidden_size,
                cfg.num_key_value_heads * cfg.head_dim,
                cfg.attention_bias,
                vb.pp("k_proj"),
            )?,
            v_proj: linear(
                cfg.hidden_size,
                cfg.num_key_value_heads * cfg.head_dim,
                cfg.attention_bias,
                vb.pp("v_proj"),
            )?,
            o_proj: linear(
                cfg.num_attention_heads * cfg.head_dim,
                cfg.hidden_size,
                cfg.attention_bias,
                vb.pp("o_proj"),
            )?,
            q_norm: RmsNorm::new(cfg.head_dim, cfg.rms_norm_eps, vb.pp("q_norm"))?,
            k_norm: RmsNorm::new(cfg.head_dim, cfg.rms_norm_eps, vb.pp("k_norm"))?,
            num_heads: cfg.num_attention_heads,
            num_kv_heads: cfg.num_key_value_heads,
            head_dim: cfg.head_dim,
            // Gemma3 uses query_pre_attn_scalar for attention scaling.
            scale: 1.0 / (cfg.query_pre_attn_scalar as f64).sqrt(),
            rotary_emb: RotaryEmbedding::new(vb.dtype(), cfg, vb.device(), sliding_window)?,
        })
    }

    fn forward(&self, xs: &Tensor, attention_mask: Option<&Tensor>) -> candle_core::Result<Tensor> {
        let (b_sz, q_len, _) = xs.dims3()?;
        let query_states = self
            .q_proj
            .forward(xs)?
            .reshape((b_sz, q_len, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;
        let key_states = self
            .k_proj
            .forward(xs)?
            .reshape((b_sz, q_len, self.num_kv_heads, self.head_dim))?
            .transpose(1, 2)?;
        let value_states = self
            .v_proj
            .forward(xs)?
            .reshape((b_sz, q_len, self.num_kv_heads, self.head_dim))?
            .transpose(1, 2)?;

        let query_states = self.q_norm.forward(&query_states)?;
        let key_states = self.k_norm.forward(&key_states)?;
        let (query_states, key_states) = self
            .rotary_emb
            .apply_rotary_emb_qkv(&query_states, &key_states)?;

        let key_states = repeat_kv(key_states, self.num_heads / self.num_kv_heads.max(1))?;
        let value_states = repeat_kv(value_states, self.num_heads / self.num_kv_heads.max(1))?;

        let attn_weights = (query_states.matmul(&key_states.transpose(2, 3)?)? * self.scale)?;
        let attn_weights = match attention_mask {
            None => attn_weights,
            Some(mask) => attn_weights.broadcast_add(mask)?,
        };
        let attn_weights = softmax(&attn_weights, D::Minus1)?;
        let attn_output = attn_weights.matmul(&value_states)?;
        attn_output
            .transpose(1, 2)?
            .reshape((b_sz, q_len, self.num_heads * self.head_dim))?
            .apply(&self.o_proj)
    }
}

struct RotaryEmbedding {
    sin: Tensor,
    cos: Tensor,
}

impl RotaryEmbedding {
    fn new(
        dtype: DType,
        cfg: &HarrierConfig,
        dev: &Device,
        sliding_window: Option<usize>,
    ) -> candle_core::Result<Self> {
        let dim = cfg.head_dim;
        let max_seq_len = cfg.max_position_embeddings;
        let rope_freq = if sliding_window.is_some() {
            cfg.rope_local_base_freq
        } else {
            cfg.rope_theta
        };
        let inv_freq: Vec<f32> = (0..dim)
            .step_by(2)
            .map(|i| 1f32 / rope_freq.powf(i as f64 / dim as f64) as f32)
            .collect();
        let inv_freq_len = inv_freq.len();
        let inv_freq = Tensor::from_vec(inv_freq, (1, inv_freq_len), dev)?.to_dtype(dtype)?;
        let t = Tensor::arange(0u32, max_seq_len as u32, dev)?
            .to_dtype(dtype)?
            .reshape((max_seq_len, 1))?;
        let freqs = t.matmul(&inv_freq)?;
        Ok(Self {
            sin: freqs.sin()?,
            cos: freqs.cos()?,
        })
    }

    fn apply_rotary_emb_qkv(
        &self,
        q: &Tensor,
        k: &Tensor,
    ) -> candle_core::Result<(Tensor, Tensor)> {
        let (_b, _h, seq_len, _d) = q.dims4()?;
        let cos = self.cos.narrow(0, 0, seq_len)?;
        let sin = self.sin.narrow(0, 0, seq_len)?;
        let q_embed = candle_nn::rotary_emb::rope(&q.contiguous()?, &cos, &sin)?;
        let k_embed = candle_nn::rotary_emb::rope(&k.contiguous()?, &cos, &sin)?;
        Ok((q_embed, k_embed))
    }
}

fn repeat_kv(xs: Tensor, n_rep: usize) -> candle_core::Result<Tensor> {
    if n_rep <= 1 {
        return Ok(xs);
    }
    let (b, n_kv, s, d) = xs.dims4()?;
    xs.unsqueeze(2)?
        .expand((b, n_kv, n_rep, s, d))?
        .reshape((b, n_kv * n_rep, s, d))
}

fn causal_mask(
    batch: usize,
    seq: usize,
    lengths: &[usize],
    device: &Device,
    dtype: DType,
) -> candle_core::Result<Tensor> {
    // Keep a real 4D mask even for seq=1 so broadcast shapes stay consistent.
    let mut data = vec![0f32; batch * seq * seq];
    for (b, &length) in lengths.iter().enumerate() {
        let valid = length.min(seq).max(1);
        let base = b * seq * seq;
        for i in 0..seq {
            for j in 0..seq {
                // Only apply causal masking on valid query rows.
                // Padding rows stay zero so softmax never sees an all -inf row.
                if i < valid && j > i {
                    data[base + i * seq + j] = f32::NEG_INFINITY;
                }
            }
        }
    }
    Tensor::from_vec(data, (batch, 1, seq, seq), device)?.to_dtype(dtype)
}

struct Mlp {
    gate_proj: Linear,
    up_proj: Linear,
    down_proj: Linear,
    act: Activation,
}

impl Mlp {
    fn load(cfg: &HarrierConfig, vb: VarBuilder) -> candle_core::Result<Self> {
        Ok(Self {
            gate_proj: linear(
                cfg.hidden_size,
                cfg.intermediate_size,
                false,
                vb.pp("gate_proj"),
            )?,
            up_proj: linear(
                cfg.hidden_size,
                cfg.intermediate_size,
                false,
                vb.pp("up_proj"),
            )?,
            down_proj: linear(
                cfg.intermediate_size,
                cfg.hidden_size,
                false,
                vb.pp("down_proj"),
            )?,
            act: Activation::GeluPytorchTanh,
        })
    }

    fn forward(&self, xs: &Tensor) -> candle_core::Result<Tensor> {
        let gate = self.gate_proj.forward(xs)?.apply(&self.act)?;
        let up = self.up_proj.forward(xs)?;
        self.down_proj.forward(&(gate * up)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn embeds_with_downloaded_local_harrier_if_present() {
        let model_dir = PathBuf::from(std::env::var("APPDATA").unwrap())
            .join("dev.aichatmemory.desktop")
            .join("models")
            .join("microsoft__harrier-oss-v1-270m");
        if !model_files_present(&model_dir) {
            return;
        }
        let settings = LocalEmbeddingSettings::default();
        let backend =
            LocalHarrierBackend::open("microsoft/harrier-oss-v1-270m".into(), model_dir, &settings)
                .await
                .expect("open local backend");
        let vectors = backend
            .embed_documents(&[
                "hello semantic search".into(),
                "another chat about project planning and weekly goals".into(),
                "short".into(),
                "这是一条中文消息，用于验证批处理 padding 与 attention mask".into(),
            ])
            .await
            .expect("embed documents batch");
        assert_eq!(vectors.len(), 4);
        for vector in &vectors {
            assert_eq!(vector.len(), 640);
            let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!(norm.is_finite() && (norm - 1.0).abs() < 1e-3, "norm={norm}");
        }
        let device = backend.runtime_device_label();
        let dtype = backend.runtime_dtype_label();
        eprintln!("local harrier runtime device={device} dtype={dtype}");
        if matches!(
            settings.device,
            LocalEmbeddingDevice::Auto | LocalEmbeddingDevice::Cuda
        ) {
            assert!(
                device.starts_with("CUDA"),
                "expected CUDA runtime after driver upgrade, got {device}/{dtype}"
            );
        }
    }
}

#[cfg(test)]
mod packing_tests {
    use super::{
        LOCAL_INDEX_MAX_BATCH_ITEMS, LOCAL_INDEX_TOKEN_BUDGET, estimate_token_count,
        plan_local_index_batch,
    };

    #[test]
    fn estimate_token_count_clamps() {
        assert_eq!(estimate_token_count(""), 1);
        assert_eq!(estimate_token_count("你好世界"), 4);
        assert!(estimate_token_count(&"a".repeat(10_000)) <= 2048);
    }

    #[test]
    fn plan_local_index_batch_prefers_similar_lengths() {
        // Mix very short and long so packing should avoid the long outliers first.
        let estimates = vec![20, 22, 21, 500, 18, 19, 23, 510, 17];
        let chosen = plan_local_index_batch(&estimates);
        assert!(!chosen.is_empty());
        assert!(chosen.len() <= LOCAL_INDEX_MAX_BATCH_ITEMS);
        let max_len = chosen.iter().map(|&i| estimates[i]).max().unwrap();
        let padded = max_len * chosen.len();
        assert!(padded <= LOCAL_INDEX_TOKEN_BUDGET || chosen.len() == 1);
        // Should pick the short cluster, and pack several of them (not a singleton).
        let avg = chosen.iter().map(|&i| estimates[i]).sum::<usize>() as f64 / chosen.len() as f64;
        assert!(avg < 100.0, "avg={avg}, chosen={chosen:?}");
        assert!(
            chosen.len() >= 5,
            "expected short-cluster packing, chosen={chosen:?}"
        );
        // Band isolation: no short+long mix.
        assert!(
            chosen.iter().all(|&i| estimates[i] < 100)
                || chosen.iter().all(|&i| estimates[i] >= 100),
            "mixed bands: {chosen:?}"
        );
    }

    #[test]
    fn plan_local_index_batch_restrains_medium_long_items() {
        let estimates = vec![400usize; 64];
        let chosen = plan_local_index_batch(&estimates);
        assert!((12..=16).contains(&chosen.len()), "chosen={}", chosen.len());
        assert!(400 * chosen.len() <= 8192 || chosen.len() == 1);
    }

    #[test]
    fn plan_local_index_batch_restrains_long_items() {
        let estimates = vec![600usize; 40];
        let chosen = plan_local_index_batch(&estimates);
        assert!((8..=10).contains(&chosen.len()), "chosen={}", chosen.len());
        assert!(600 * chosen.len() <= 7168 || chosen.len() == 1);
    }

    #[test]
    fn plan_local_index_batch_packs_many_short_items() {
        let estimates = vec![40usize; 80];
        let chosen = plan_local_index_batch(&estimates);
        assert!(chosen.len() >= 48, "chosen={}", chosen.len());
        assert!(chosen.len() <= LOCAL_INDEX_MAX_BATCH_ITEMS);
    }

    #[test]
    fn plan_local_index_batch_short_first_avoids_singleton() {
        // Lots of shorts plus some longs: must pack many shorts, never a singleton short.
        let mut estimates = vec![35usize; 30];
        estimates.extend(std::iter::repeat_n(600usize, 20));
        let chosen = plan_local_index_batch(&estimates);
        assert!(
            chosen.len() >= 24,
            "expected short-first large pack, chosen={}",
            chosen.len()
        );
        assert!(chosen.iter().all(|&i| estimates[i] < 100));
    }

    #[test]
    fn plan_local_index_batch_skips_sparse_shorts_for_dense_longs() {
        // Only a couple shorts and many longs: do NOT emit short singletons first.
        let mut estimates = vec![30usize, 40usize];
        estimates.extend(std::iter::repeat_n(600usize, 20));
        let chosen = plan_local_index_batch(&estimates);
        assert!(
            chosen.len() >= 8,
            "expected dense long pack, chosen={}",
            chosen.len()
        );
        assert!(
            chosen.iter().all(|&i| estimates[i] >= 500),
            "should skip sparse shorts, chosen={chosen:?}"
        );
    }
}
