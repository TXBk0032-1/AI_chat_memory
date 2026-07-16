use async_trait::async_trait;
use candle_core::{D, DType, Device, IndexOp, Module, Tensor};
use candle_nn::{Activation, Embedding, Linear, VarBuilder, linear_b as linear, ops::softmax};
use hf_hub::api::tokio::{ApiBuilder, Progress};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokenizers::Tokenizer;
use tokio::sync::Mutex;

use super::{BackendIdentity, EmbeddingBackend, ensure_dimensions};
use crate::{
    error::{AppError, Result},
    models::{EmbeddingBackendKind, EmbeddingHealth, ModelDownloadProgress},
};

const DEFAULT_QUERY_INSTRUCTION: &str = "Instruct: Given a chat history search query, retrieve relevant conversation passages that answer the query\nQuery: ";
const MODEL_FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];
const MAX_SEQUENCE_LEN: usize = 2048;

pub type DownloadProgressCallback = Arc<dyn Fn(ModelDownloadProgress) + Send + Sync>;

pub struct LocalHarrierBackend {
    model_id: String,
    model_dir: PathBuf,
    dimensions: usize,
    /// Serialize first-time weight loading across warm-up / indexer / query.
    load_gate: Mutex<()>,
    /// Multiple CPU model replicas for parallel embedding workers.
    replicas: Arc<std::sync::Mutex<Vec<ModelReplica>>>,
    worker_count: usize,
}

struct ModelReplica {
    tokenizer: Tokenizer,
    model: HarrierModel,
    device: Device,
}

impl LocalHarrierBackend {
    pub async fn open(model_id: String, model_dir: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&model_dir)
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        // Keep startup cheap: only validate directory here and load weights on first use.
        Ok(Self {
            model_id,
            model_dir,
            dimensions: 640,
            load_gate: Mutex::new(()),
            replicas: Arc::new(std::sync::Mutex::new(Vec::new())),
            worker_count: recommended_worker_count(),
        })
    }

    pub fn is_loaded(&self) -> bool {
        self.replicas
            .lock()
            .map(|guard| !guard.is_empty())
            .unwrap_or(false)
    }

    pub fn model_dir(&self) -> &Path {
        &self.model_dir
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
        if let Ok(mut guard) = self.replicas.lock() {
            guard.clear();
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
        let worker_count = self.worker_count;
        let replicas = tokio::task::spawn_blocking(move || load_replicas(&model_dir, worker_count))
            .await
            .map_err(|error| AppError::Configuration(format!("加载本地模型任务失败: {error}")))?
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        let mut guard = self
            .replicas
            .lock()
            .map_err(|_| AppError::Configuration("local model state lock poisoned".into()))?;
        if guard.is_empty() {
            *guard = replicas;
            tracing::info!(workers = worker_count, "local embedding CPU workers ready");
        }
        Ok(())
    }

    async fn embed(&self, texts: &[String], is_query: bool) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_loaded().await?;
        let replicas = Arc::clone(&self.replicas);
        let texts = texts.to_vec();
        let dimensions = self.dimensions;
        let worker_count = self.worker_count;
        let vectors = tokio::task::spawn_blocking(move || {
            embed_with_replicas(replicas, texts, is_query, dimensions, worker_count)
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
        Ok(EmbeddingHealth {
            ok: true,
            backend: EmbeddingBackendKind::Local,
            model_id: self.model_id.clone(),
            dimensions: Some(self.dimensions),
            message: if self.is_loaded() {
                "本地模型已加载".into()
            } else {
                "本地模型文件已就绪".into()
            },
        })
    }

    fn is_ready(&self) -> bool {
        self.is_loaded()
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

fn recommended_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4)
        .clamp(2, 6)
}

fn load_replicas(model_dir: &Path, worker_count: usize) -> Result<Vec<ModelReplica>> {
    let mut replicas = Vec::with_capacity(worker_count);
    for index in 0..worker_count {
        replicas.push(load_model(model_dir)?);
        tracing::debug!(
            worker = index + 1,
            workers = worker_count,
            "local embedding replica loaded"
        );
    }
    Ok(replicas)
}

fn load_model(model_dir: &Path) -> Result<ModelReplica> {
    let device = Device::Cpu;
    let dtype = DType::F32;
    let config_text = std::fs::read_to_string(model_dir.join("config.json"))
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    let config: HarrierConfig = serde_json::from_str(&config_text)
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    let tokenizer = Tokenizer::from_file(model_dir.join("tokenizer.json"))
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    let vb = unsafe {
        VarBuilder::from_mmaped_safetensors(&[model_dir.join("model.safetensors")], dtype, &device)
            .map_err(candle_err)?
    };
    let model = HarrierModel::load(&config, vb).map_err(candle_err)?;
    Ok(ModelReplica {
        tokenizer,
        model,
        device,
    })
}

fn take_replica(replicas: &std::sync::Mutex<Vec<ModelReplica>>) -> Result<ModelReplica> {
    let mut guard = replicas
        .lock()
        .map_err(|_| AppError::Configuration("local model state lock poisoned".into()))?;
    guard
        .pop()
        .ok_or_else(|| AppError::Configuration("no free local embedding worker".into()))
}

fn return_replica(
    replicas: &std::sync::Mutex<Vec<ModelReplica>>,
    replica: ModelReplica,
) -> Result<()> {
    let mut guard = replicas
        .lock()
        .map_err(|_| AppError::Configuration("local model state lock poisoned".into()))?;
    guard.push(replica);
    Ok(())
}

fn embed_with_replicas(
    replicas: Arc<std::sync::Mutex<Vec<ModelReplica>>>,
    texts: Vec<String>,
    is_query: bool,
    dimensions: usize,
    worker_count: usize,
) -> Result<Vec<Vec<f32>>> {
    if texts.len() == 1 {
        let mut replica = take_replica(&replicas)?;
        let result = embed_one(&mut replica, &texts[0], is_query);
        return_replica(&replicas, replica)?;
        let vector = result?;
        ensure_dimensions(std::slice::from_ref(&vector), dimensions)?;
        return Ok(vec![vector]);
    }

    let chunk_size = texts.len().div_ceil(worker_count).max(1);
    let chunks = texts
        .chunks(chunk_size)
        .enumerate()
        .map(|(chunk_index, chunk)| (chunk_index, chunk.to_vec()))
        .collect::<Vec<_>>();

    let results = chunks
        .into_par_iter()
        .map(|(chunk_index, chunk_texts)| {
            let mut replica = take_replica(&replicas)?;
            let mut vectors = Vec::with_capacity(chunk_texts.len());
            let mut error = None;
            for text in &chunk_texts {
                match embed_one(&mut replica, text, is_query) {
                    Ok(vector) => vectors.push(vector),
                    Err(err) => {
                        error = Some(err);
                        break;
                    }
                }
            }
            return_replica(&replicas, replica)?;
            if let Some(err) = error {
                return Err(err);
            }
            Ok((chunk_index, vectors))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut ordered = results;
    ordered.sort_by_key(|(index, _)| *index);
    let vectors = ordered
        .into_iter()
        .flat_map(|(_, values)| values)
        .collect::<Vec<_>>();
    ensure_dimensions(&vectors, dimensions)?;
    Ok(vectors)
}

fn embed_one(replica: &mut ModelReplica, text: &str, is_query: bool) -> Result<Vec<f32>> {
    let prepared = if is_query {
        format!("{DEFAULT_QUERY_INSTRUCTION}{text}")
    } else {
        text.to_owned()
    };
    let encoding = replica
        .tokenizer
        .encode(prepared, true)
        .map_err(|error| AppError::Configuration(error.to_string()))?;
    let ids = encoding.get_ids();
    if ids.is_empty() {
        return Err(AppError::Configuration(
            "tokenizer produced empty input".into(),
        ));
    }
    let ids = if ids.len() > MAX_SEQUENCE_LEN {
        &ids[..MAX_SEQUENCE_LEN]
    } else {
        ids
    };
    let input = Tensor::new(ids, &replica.device)
        .map_err(candle_err)?
        .unsqueeze(0)
        .map_err(candle_err)?;
    let embedding = replica
        .model
        .embed(&input)
        .map_err(candle_err)?
        .squeeze(0)
        .map_err(candle_err)?
        .to_dtype(DType::F32)
        .map_err(candle_err)?
        .to_vec1::<f32>()
        .map_err(candle_err)?;
    Ok(embedding)
}

fn candle_err(error: impl ToString) -> AppError {
    AppError::Configuration(error.to_string())
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

    fn embed(&self, input_ids: &Tensor) -> candle_core::Result<Tensor> {
        let (b_size, seq_len) = input_ids.dims2()?;
        let mut xs = self.embed_tokens.forward(input_ids)?;
        xs = (xs * (self.hidden_size as f64).sqrt())?;
        let attention_mask = causal_mask(b_size, seq_len, xs.device(), xs.dtype())?;
        for layer in &self.layers {
            xs = layer.forward(&xs, Some(&attention_mask))?;
        }
        let xs = self.norm.forward(&xs)?;
        // last-token pooling for harrier embeddings
        let pooled = xs.i((.., seq_len - 1, ..))?;
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
    device: &Device,
    dtype: DType,
) -> candle_core::Result<Tensor> {
    if seq <= 1 {
        return Tensor::zeros((batch, 1, 1, 1), dtype, device);
    }
    let mut data = vec![0f32; seq * seq];
    for i in 0..seq {
        for j in (i + 1)..seq {
            data[i * seq + j] = f32::NEG_INFINITY;
        }
    }
    Tensor::from_vec(data, (seq, seq), device)?
        .to_dtype(dtype)?
        .broadcast_as((batch, 1, seq, seq))
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
        let backend = LocalHarrierBackend::open("microsoft/harrier-oss-v1-270m".into(), model_dir)
            .await
            .expect("open local backend");
        let vectors = backend
            .embed_documents(&["hello semantic search".into()])
            .await
            .expect("embed document");
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].len(), 640);
        let norm = vectors[0].iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "norm={norm}");
    }
}
