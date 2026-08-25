use async_trait::async_trait;
use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use futures::StreamExt;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokenizers::Tokenizer;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use super::local::DownloadProgressCallback;
use super::{BackendIdentity, EmbeddingBackend, ensure_dimensions};
use crate::{
    error::{AppError, Result},
    models::{
        EmbeddingBackendKind, EmbeddingHealth, LocalEmbeddingDType, LocalEmbeddingDevice,
        LocalEmbeddingSettings, ModelDownloadProgress,
    },
};

const MODEL_FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];
const MAX_SEQUENCE_LEN: usize = 512;
const DEFAULT_QUERY_PREFIX: &str = "为这个句子生成表示以用于检索相关文章：";

#[derive(Deserialize)]
struct ModelMetadata {
    model_type: Option<String>,
    hidden_size: usize,
}

pub struct LocalBgeBackend {
    model_id: String,
    model_dir: PathBuf,
    dimensions: usize,
    preferred_device: LocalEmbeddingDevice,
    preferred_dtype: LocalEmbeddingDType,
    load_gate: Mutex<()>,
    state: Arc<std::sync::Mutex<Option<LoadedModel>>>,
    runtime_device: Arc<std::sync::Mutex<String>>,
    runtime_dtype: Arc<std::sync::Mutex<String>>,
    cancel_flag: Arc<AtomicBool>,
}

struct LoadedModel {
    tokenizer: Tokenizer,
    model: BertModel,
    device: Device,
    device_label: String,
    dtype_label: String,
}

async fn run_model_task<T, F>(task: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|error| AppError::Configuration(format!("BGE 模型执行器异常退出: {error}")))?
}

impl LocalBgeBackend {
    pub async fn open(
        model_id: String,
        model_dir: PathBuf,
        settings: &LocalEmbeddingSettings,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<Self> {
        tokio::fs::create_dir_all(&model_dir)
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        let dimensions = resolve_dimensions(&model_id, &model_dir).await?;
        Ok(Self {
            model_id,
            model_dir,
            dimensions,
            preferred_device: settings.device.clone(),
            preferred_dtype: settings.dtype.clone(),
            load_gate: Mutex::new(()),
            state: Arc::new(std::sync::Mutex::new(None)),
            runtime_device: Arc::new(std::sync::Mutex::new("unloaded".into())),
            runtime_dtype: Arc::new(std::sync::Mutex::new("unloaded".into())),
            cancel_flag,
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
                    message: format!("本地模型已就绪：{}", self.model_dir.display()),
                });
            }
            return Ok(());
        }
        download_model(
            &self.model_id,
            &self.model_dir,
            on_progress,
            Some(self.cancel_flag.clone()),
        )
        .await
    }

    pub async fn import_from_path(&self, source: &Path) -> Result<()> {
        if !source.is_dir() {
            return Err(AppError::Configuration(
                "本地模型路径必须是包含 config.json / tokenizer.json / model.safetensors 的目录"
                    .into(),
            ));
        }
        for file in MODEL_FILES {
            let from = source.join(file);
            if !from.exists() {
                return Err(AppError::Configuration(format!("缺少模型文件: {file}")));
            }
        }
        tokio::fs::create_dir_all(&self.model_dir)
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        for file in MODEL_FILES {
            tokio::fs::copy(source.join(file), self.model_dir.join(file))
                .await
                .map_err(|error| AppError::Configuration(error.to_string()))?;
        }
        // optional pooling config
        let pooling = source.join("1_Pooling").join("config.json");
        if pooling.exists() {
            let dest = self.model_dir.join("1_Pooling");
            let _ = tokio::fs::create_dir_all(&dest).await;
            let _ = tokio::fs::copy(pooling, dest.join("config.json")).await;
        }
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
            return Err(AppError::Configuration(format!(
                "本地模型尚未下载：{}",
                self.model_dir.display()
            )));
        }
        let model_dir = self.model_dir.clone();
        let preferred_device = self.preferred_device.clone();
        let preferred_dtype = self.preferred_dtype.clone();
        let loaded =
            run_model_task(move || load_model(&model_dir, &preferred_device, &preferred_dtype))
                .await?;
        if let Ok(mut guard) = self.runtime_device.lock() {
            *guard = loaded.device_label.clone();
        }
        if let Ok(mut guard) = self.runtime_dtype.lock() {
            *guard = loaded.dtype_label.clone();
        }
        if let Ok(mut guard) = self.state.lock() {
            *guard = Some(loaded);
        }
        Ok(())
    }

    async fn embed(&self, texts: &[String], is_query: bool) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if self.cancel_flag.load(Ordering::SeqCst) {
            return Err(AppError::Cancelled("本地编码已取消".into()));
        }
        self.ensure_loaded().await?;
        let dimensions = self.dimensions;
        let state = self.state.clone();
        let texts = texts.to_vec();
        let cancel_flag = self.cancel_flag.clone();
        run_model_task(move || {
            let mut guard = state
                .lock()
                .map_err(|_| AppError::Configuration("local model lock poisoned".into()))?;
            let loaded = guard
                .as_mut()
                .ok_or_else(|| AppError::Configuration("local model not loaded".into()))?;
            embed_texts(loaded, &texts, is_query, dimensions, &cancel_flag)
        })
        .await
    }
}

async fn resolve_dimensions(model_id: &str, model_dir: &Path) -> Result<usize> {
    let config_path = model_dir.join("config.json");
    if config_path.exists() {
        let content = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        let metadata: ModelMetadata = serde_json::from_str(&content).map_err(|error| {
            AppError::Configuration(format!(
                "无法读取模型配置 {}: {error}",
                config_path.display()
            ))
        })?;
        let architecture = metadata.model_type.as_deref().unwrap_or("bert");
        if architecture != "bert" {
            return Err(AppError::Configuration(format!(
                "BGE 模型架构 {architecture} 与当前 BertModel 执行器不兼容"
            )));
        }
        if metadata.hidden_size == 0 {
            return Err(AppError::Configuration(
                "BGE 模型配置 hidden_size 必须大于 0".into(),
            ));
        }
        return Ok(metadata.hidden_size);
    }

    let lower = model_id.to_ascii_lowercase();
    if lower.contains("bge-small-zh") {
        Ok(512)
    } else if lower.contains("bge-base-zh") {
        Ok(768)
    } else if lower.contains("bge-large-zh") {
        Ok(1024)
    } else if lower.contains("bge-m3") {
        Err(AppError::Configuration(
            "BGE 模型架构 xlm-roberta 与当前 BertModel 执行器不兼容".into(),
        ))
    } else {
        Err(AppError::Configuration(format!(
            "无法在下载前确定 BGE 模型维度：{model_id}"
        )))
    }
}

#[async_trait]
impl EmbeddingBackend for LocalBgeBackend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            backend: EmbeddingBackendKind::Local,
            backend_id: "local".into(),
            model_id: self.model_id.clone(),
            dimensions: self.dimensions,
        }
    }

    fn is_ready(&self) -> bool {
        model_files_present(&self.model_dir) && self.is_loaded()
    }

    fn runtime_device(&self) -> Option<String> {
        Some(self.runtime_device_label())
    }

    fn runtime_dtype(&self) -> Option<String> {
        Some(self.runtime_dtype_label())
    }

    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts, false).await
    }

    async fn embed_queries(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts, true).await
    }

    async fn healthcheck(&self) -> Result<EmbeddingHealth> {
        if !model_files_present(&self.model_dir) {
            return Ok(EmbeddingHealth {
                ok: false,
                backend: EmbeddingBackendKind::Local,
                model_id: self.model_id.clone(),
                dimensions: Some(self.dimensions),
                message: format!("模型文件未就绪：{}", self.model_dir.display()),
            });
        }
        match self.embed_queries(&["健康检查".into()]).await {
            Ok(vectors) => Ok(EmbeddingHealth {
                ok: vectors
                    .first()
                    .map(|v| v.len() == self.dimensions)
                    .unwrap_or(false),
                backend: EmbeddingBackendKind::Local,
                model_id: self.model_id.clone(),
                dimensions: Some(self.dimensions),
                message: format!(
                    "ok · {}/{}",
                    self.runtime_device_label(),
                    self.runtime_dtype_label()
                ),
            }),
            Err(error) => Ok(EmbeddingHealth {
                ok: false,
                backend: EmbeddingBackendKind::Local,
                model_id: self.model_id.clone(),
                dimensions: Some(self.dimensions),
                message: error.to_string(),
            }),
        }
    }
}

pub fn model_files_present(dir: &Path) -> bool {
    MODEL_FILES.iter().all(|file| dir.join(file).exists())
}

pub fn is_bge_model(model_id: &str) -> bool {
    let lower = model_id.to_ascii_lowercase();
    lower.contains("bge-small-zh")
        || lower.contains("bge-base-zh")
        || lower.contains("bge-large-zh")
        || lower.contains("bge-m3")
        || lower.ends_with("bge-small-zh-v1.5")
}

async fn download_model(
    model_id: &str,
    model_dir: &Path,
    on_progress: Option<DownloadProgressCallback>,
    cancel_flag: Option<Arc<AtomicBool>>,
) -> Result<()> {
    tokio::fs::create_dir_all(model_dir)
        .await
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    check_cancelled(cancel_flag.as_ref(), "模型下载已取消")?;

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

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(30 * 60))
        .build()
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    let file_count = MODEL_FILES.len();

    for (file_index, file) in MODEL_FILES.iter().enumerate() {
        check_cancelled(cancel_flag.as_ref(), "模型下载已取消")?;
        let url = format!("https://huggingface.co/{model_id}/resolve/main/{file}");
        let response =
            client.get(&url).send().await.map_err(|error| {
                AppError::Configuration(format!("download {file} failed: {error}"))
            })?;
        if !response.status().is_success() {
            return Err(AppError::Configuration(format!(
                "download {file} failed: HTTP {}",
                response.status()
            )));
        }
        let total_bytes = response.content_length();
        let destination = model_dir.join(file);
        let temporary = model_dir.join(format!("{file}.part"));
        let mut output = tokio::fs::File::create(&temporary)
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        let mut stream = response.bytes_stream();
        let mut downloaded_bytes = 0u64;

        while let Some(chunk) = stream.next().await {
            if let Err(error) = check_cancelled(cancel_flag.as_ref(), "模型下载已取消") {
                drop(output);
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(error);
            }
            let chunk = chunk.map_err(|error| {
                AppError::Configuration(format!("download {file} failed: {error}"))
            })?;
            output
                .write_all(&chunk)
                .await
                .map_err(|error| AppError::Configuration(error.to_string()))?;
            downloaded_bytes = downloaded_bytes.saturating_add(chunk.len() as u64);
            if let Some(on_progress) = &on_progress {
                let file_fraction = total_bytes
                    .filter(|total| *total > 0)
                    .map(|total| (downloaded_bytes as f32 / total as f32).clamp(0.0, 1.0))
                    .unwrap_or(0.0);
                let fraction = (file_index as f32 + file_fraction) / file_count.max(1) as f32;
                on_progress(ModelDownloadProgress {
                    stage: "downloading".into(),
                    file: Some((*file).into()),
                    file_index,
                    file_count,
                    downloaded_bytes,
                    total_bytes,
                    fraction: fraction.clamp(0.0, 0.999),
                    message: format!(
                        "正在下载 {file}：{} / {}",
                        format_bytes(downloaded_bytes),
                        total_bytes.map(format_bytes).unwrap_or_else(|| "?".into())
                    ),
                });
            }
        }
        output
            .flush()
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        drop(output);
        check_cancelled(cancel_flag.as_ref(), "模型下载已取消")?;
        if destination.exists() {
            let _ = tokio::fs::remove_file(&destination).await;
        }
        tokio::fs::rename(&temporary, &destination)
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

fn check_cancelled(cancel_flag: Option<&Arc<AtomicBool>>, message: &str) -> Result<()> {
    if cancel_flag.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
        Err(AppError::Cancelled(message.into()))
    } else {
        Ok(())
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

fn safe_new_cuda(ordinal: usize) -> std::result::Result<Device, String> {
    std::panic::catch_unwind(|| Device::new_cuda(ordinal))
        .map_err(|payload| {
            if let Some(s) = payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "CUDA 初始化异常（动态库未就绪或设备不可用）".to_string()
            }
        })
        .and_then(|res| res.map_err(|e| e.to_string()))
}

fn load_model(
    model_dir: &Path,
    preferred_device: &LocalEmbeddingDevice,
    preferred_dtype: &LocalEmbeddingDType,
) -> Result<LoadedModel> {
    let config_text = std::fs::read_to_string(model_dir.join("config.json"))
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    let config: BertConfig = serde_json::from_str(&config_text)
        .map_err(|error| AppError::Configuration(format!("invalid bert config: {error}")))?;
    let tokenizer = Tokenizer::from_file(model_dir.join("tokenizer.json"))
        .map_err(|error| AppError::Configuration(format!("tokenizer load failed: {error}")))?;

    let mut candidates = Vec::new();
    match preferred_device {
        LocalEmbeddingDevice::Cpu => candidates.push((Device::Cpu, "CPU")),
        LocalEmbeddingDevice::Cuda | LocalEmbeddingDevice::Auto => {
            match safe_new_cuda(0) {
                Ok(device) => candidates.push((device, "CUDA:0")),
                Err(error) => {
                    if matches!(preferred_device, LocalEmbeddingDevice::Cuda) {
                        return Err(AppError::Configuration(format!(
                            "CUDA unavailable: {error}"
                        )));
                    }
                    tracing::warn!(%error, "CUDA unavailable for bge; falling back to CPU");
                    candidates.push((Device::Cpu, "CPU"));
                }
            }
            if matches!(preferred_device, LocalEmbeddingDevice::Auto) {
                candidates.push((Device::Cpu, "CPU"));
            }
        }
    }

    let mut last_error = None;
    for (device, device_label) in candidates {
        let dtype_order = match (preferred_dtype, device_label.starts_with("CUDA")) {
            (LocalEmbeddingDType::F16, _) => vec![DType::F16],
            (LocalEmbeddingDType::F32, _) => vec![DType::F32],
            (LocalEmbeddingDType::Auto, true) => vec![DType::F16, DType::F32],
            (LocalEmbeddingDType::Auto, false) => vec![DType::F32],
        };
        for dtype in dtype_order {
            let dtype_label = match dtype {
                DType::F16 => "F16",
                DType::F32 => "F32",
                _ => "OTHER",
            };
            match try_load(model_dir, &config, &device, dtype) {
                Ok(model) => {
                    let mut loaded = LoadedModel {
                        tokenizer: tokenizer.clone(),
                        model,
                        device: device.clone(),
                        device_label: device_label.into(),
                        dtype_label: dtype_label.into(),
                    };
                    if let Err(error) = warmup_model(&mut loaded, config.hidden_size) {
                        last_error = Some(error);
                        continue;
                    }
                    tracing::info!(
                        device = %loaded.device_label,
                        dtype = %loaded.dtype_label,
                        "loaded local bge embedding model"
                    );
                    return Ok(loaded);
                }
                Err(error) => {
                    tracing::warn!(
                        device = %device_label,
                        dtype = %dtype_label,
                        %error,
                        "bge load failed; trying next device/dtype"
                    );
                    last_error = Some(error);
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        AppError::Configuration("failed to load local bge embedding model".into())
    }))
}

fn try_load(
    model_dir: &Path,
    config: &BertConfig,
    device: &Device,
    dtype: DType,
) -> Result<BertModel> {
    // sentence-transformers export sometimes nests under "bert." or has none.
    let weights = [model_dir.join("model.safetensors")];
    let vb_bert = unsafe {
        VarBuilder::from_mmaped_safetensors(&weights, dtype, device).map_err(candle_err)?
    };
    if let Ok(model) = BertModel::load(vb_bert.pp("bert"), config) {
        return Ok(model);
    }
    let vb = unsafe {
        VarBuilder::from_mmaped_safetensors(&weights, dtype, device).map_err(candle_err)?
    };
    BertModel::load(vb, config).map_err(candle_err)
}

fn warmup_model(loaded: &mut LoadedModel, dimensions: usize) -> Result<()> {
    let dummy_cancel = AtomicBool::new(false);
    let vectors = embed_texts(loaded, &["warmup".into()], false, dimensions, &dummy_cancel)?;
    if vectors.len() != 1 || vectors[0].len() != dimensions {
        return Err(AppError::Configuration(
            "bge warmup returned unexpected shape".into(),
        ));
    }
    let norm = vectors[0].iter().map(|v| v * v).sum::<f32>().sqrt();
    if !norm.is_finite() || (norm - 1.0).abs() > 5e-2 {
        return Err(AppError::Configuration(format!(
            "bge warmup produced invalid vector norm={norm}"
        )));
    }
    Ok(())
}

const SUB_BATCH_SIZE: usize = 32;

fn embed_texts(
    loaded: &LoadedModel,
    texts: &[String],
    is_query: bool,
    dimensions: usize,
    cancel_flag: &AtomicBool,
) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let mut all_vectors = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(SUB_BATCH_SIZE) {
        if cancel_flag.load(Ordering::SeqCst) {
            return Err(AppError::Cancelled("本地编码已取消".into()));
        }
        let batch_vectors = embed_single_batch(loaded, chunk, is_query, dimensions)?;
        all_vectors.extend(batch_vectors);
    }
    Ok(all_vectors)
}

fn embed_single_batch(
    loaded: &LoadedModel,
    texts: &[String],
    is_query: bool,
    dimensions: usize,
) -> Result<Vec<Vec<f32>>> {
    let prepared = texts
        .iter()
        .map(|text| {
            if is_query {
                format!("{DEFAULT_QUERY_PREFIX}{text}")
            } else {
                text.clone()
            }
        })
        .collect::<Vec<_>>();

    let mut encodings = Vec::with_capacity(prepared.len());
    let mut max_len = 1usize;
    for text in &prepared {
        let encoding = loaded
            .tokenizer
            .encode(text.as_str(), true)
            .map_err(|error| AppError::Configuration(format!("tokenize failed: {error}")))?;
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
        encodings.push(ids);
    }
    max_len = max_len.min(MAX_SEQUENCE_LEN);

    let mut flat_ids = Vec::with_capacity(encodings.len() * max_len);
    let mut flat_type_ids = Vec::with_capacity(encodings.len() * max_len);
    let mut flat_mask = Vec::with_capacity(encodings.len() * max_len);
    let mut lengths = Vec::with_capacity(encodings.len());
    for ids in &encodings {
        let len = ids.len().min(max_len);
        lengths.push(len);
        flat_ids.extend_from_slice(&ids[..len]);
        flat_ids.extend(std::iter::repeat_n(0u32, max_len - len));
        flat_type_ids.extend(std::iter::repeat_n(0u32, max_len));
        flat_mask.extend(std::iter::repeat_n(1u32, len));
        flat_mask.extend(std::iter::repeat_n(0u32, max_len - len));
    }

    let bsz = encodings.len();
    let input_ids =
        Tensor::from_vec(flat_ids, (bsz, max_len), &loaded.device).map_err(candle_err)?;
    let token_type_ids =
        Tensor::from_vec(flat_type_ids, (bsz, max_len), &loaded.device).map_err(candle_err)?;
    let attention_mask =
        Tensor::from_vec(flat_mask, (bsz, max_len), &loaded.device).map_err(candle_err)?;

    let started = std::time::Instant::now();
    let hidden = loaded
        .model
        .forward(&input_ids, &token_type_ids, Some(&attention_mask))
        .map_err(candle_err)?;
    // BGE v1.5's published sentence-transformers config uses CLS pooling.
    let pooled = hidden.i((.., 0, ..)).map_err(candle_err)?;
    let norms = pooled
        .sqr()
        .map_err(candle_err)?
        .sum_keepdim(candle_core::D::Minus1)
        .map_err(candle_err)?
        .sqrt()
        .map_err(candle_err)?
        .clamp(1e-12, f64::INFINITY)
        .map_err(candle_err)?;
    let normalized = pooled.broadcast_div(&norms).map_err(candle_err)?;
    let vectors = normalized
        .to_dtype(DType::F32)
        .map_err(candle_err)?
        .to_vec2::<f32>()
        .map_err(candle_err)?;
    ensure_dimensions(&vectors, dimensions)?;

    let total_tokens: usize = lengths.iter().sum();
    let elapsed = started.elapsed().as_millis().max(1);
    tracing::info!(
        batch_size = bsz,
        max_len,
        total_tokens,
        avg_tokens = total_tokens as f64 / bsz as f64,
        forward_ms = elapsed,
        chunks_per_sec = bsz as f64 * 1000.0 / elapsed as f64,
        device = %loaded.device_label,
        dtype = %loaded.dtype_label,
        is_query,
        "local bge embedding batch profile"
    );
    Ok(vectors)
}

fn candle_err(error: candle_core::Error) -> AppError {
    AppError::Configuration(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ai-chat-memory-bge-{name}-{}", std::process::id()))
    }

    #[tokio::test]
    async fn reads_embedding_dimensions_from_model_config() {
        let dir = test_dir("dimensions");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(
            dir.join("config.json"),
            r#"{"model_type":"bert","hidden_size":384}"#,
        )
        .await
        .unwrap();

        let backend = LocalBgeBackend::open(
            "fixture/bge-model".into(),
            dir.clone(),
            &LocalEmbeddingSettings::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();

        assert_eq!(backend.identity().dimensions, 384);
        let _ = tokio::fs::remove_dir_all(dir).await;
    }

    #[tokio::test]
    async fn rejects_incompatible_model_architecture_before_loading() {
        let dir = test_dir("architecture");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(
            dir.join("config.json"),
            r#"{"model_type":"xlm-roberta","hidden_size":1024}"#,
        )
        .await
        .unwrap();

        let result = LocalBgeBackend::open(
            "fixture/bge-m3".into(),
            dir.clone(),
            &LocalEmbeddingSettings::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .await;

        assert!(
            matches!(result, Err(AppError::Configuration(message)) if message.contains("xlm-roberta"))
        );
        let _ = tokio::fs::remove_dir_all(dir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_model_work_does_not_block_tokio_worker() {
        let model_work = run_model_task(|| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            Ok::<_, AppError>(7)
        });
        tokio::pin!(model_work);

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
            result = &mut model_work => panic!("model work completed on Tokio worker: {result:?}"),
        }

        assert_eq!(model_work.await.unwrap(), 7);
    }

    #[tokio::test]
    async fn embeds_with_downloaded_bge_if_present() {
        let Some(appdata) = std::env::var_os("APPDATA") else {
            return;
        };
        let model_dir = PathBuf::from(appdata)
            .join("dev.aichatmemory.desktop")
            .join("models")
            .join("BAAI__bge-small-zh-v1.5");
        if !model_files_present(&model_dir) {
            return;
        }
        let settings = LocalEmbeddingSettings::default();
        let backend = LocalBgeBackend::open(
            "BAAI/bge-small-zh-v1.5".into(),
            model_dir,
            &settings,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("open bge backend");
        let vectors = backend
            .embed_documents(&[
                "这是一段用于中文语义搜索的测试消息".into(),
                "Rust Candle GPU embedding".into(),
            ])
            .await
            .expect("embed bge documents");
        assert_eq!(vectors.len(), 2);
        for vector in vectors {
            assert_eq!(vector.len(), 512);
            let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 5e-2, "unexpected norm {norm}");
        }
    }
}
