use async_trait::async_trait;
use candle_core::{D, DType, Device, Module, Tensor};
use candle_nn::{Activation, Embedding, Linear, VarBuilder, linear_b as linear, ops::softmax};
use hf_hub::api::tokio::{ApiBuilder, Progress};
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

pub type DownloadProgressCallback = Arc<dyn Fn(ModelDownloadProgress) + Send + Sync>;

pub struct LocalHarrierBackend {
    model_id: String,
    model_dir: PathBuf,
    dimensions: usize,
    state: Mutex<Option<LoadedModel>>,
}

struct LoadedModel {
    tokenizer: Tokenizer,
    model: HarrierModel,
    device: Device,
}

impl LocalHarrierBackend {
    pub async fn open(model_id: String, model_dir: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&model_dir)
            .await
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        let backend = Self {
            model_id,
            model_dir,
            dimensions: 640,
            state: Mutex::new(None),
        };
        if model_files_present(&backend.model_dir) {
            backend.ensure_loaded().await?;
        }
        Ok(backend)
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
        *self.state.lock().await = None;
        self.ensure_loaded().await?;
        Ok(())
    }

    async fn ensure_loaded(&self) -> Result<()> {
        let mut guard = self.state.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        if !model_files_present(&self.model_dir) {
            return Err(AppError::Configuration(
                "本地 embedding 模型尚未准备好，请先下载或导入模型".into(),
            ));
        }
        let loaded = load_model(&self.model_dir)?;
        *guard = Some(loaded);
        Ok(())
    }

    async fn embed(&self, texts: &[String], is_query: bool) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_loaded().await?;
        let mut guard = self.state.lock().await;
        let loaded = guard
            .as_mut()
            .ok_or_else(|| AppError::Configuration("local model not loaded".into()))?;
        let mut vectors = Vec::with_capacity(texts.len());
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
            let ids = encoding.get_ids();
            if ids.is_empty() {
                return Err(AppError::Configuration(
                    "tokenizer produced empty input".into(),
                ));
            }
            let ids = if ids.len() > 2048 { &ids[..2048] } else { ids };
            let input = Tensor::new(ids, &loaded.device)
                .map_err(candle_err)?
                .unsqueeze(0)
                .map_err(candle_err)?;
            let embedding = loaded
                .model
                .embed(&input)
                .map_err(candle_err)?
                .squeeze(0)
                .map_err(candle_err)?
                .to_dtype(DType::F32)
                .map_err(candle_err)?
                .to_vec1::<f32>()
                .map_err(candle_err)?;
            vectors.push(embedding);
        }
        ensure_dimensions(&vectors, self.dimensions)?;
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
        if !model_files_present(&self.model_dir) {
            return Ok(EmbeddingHealth {
                ok: false,
                backend: EmbeddingBackendKind::Local,
                model_id: self.model_id.clone(),
                dimensions: Some(self.dimensions),
                message: "本地模型未下载或未导入".into(),
            });
        }
        match self.embed_queries(&["healthcheck".into()]).await {
            Ok(vectors) => Ok(EmbeddingHealth {
                ok: !vectors.is_empty(),
                backend: EmbeddingBackendKind::Local,
                model_id: self.model_id.clone(),
                dimensions: vectors.first().map(Vec::len),
                message: "ok".into(),
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

fn load_model(model_dir: &Path) -> Result<LoadedModel> {
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
    Ok(LoadedModel {
        tokenizer,
        model,
        device,
    })
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
    #[serde(default = "default_query_scalar")]
    query_pre_attn_scalar: usize,
    #[serde(default = "default_sliding_pattern")]
    sliding_window_pattern: usize,
    #[serde(default = "default_sliding_window")]
    sliding_window: usize,
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

struct HarrierModel {
    embed_tokens: Embedding,
    layers: Vec<DecoderLayer>,
    norm: RmsNorm,
    hidden_size: usize,
}

impl HarrierModel {
    fn load(cfg: &HarrierConfig, vb: VarBuilder) -> candle_core::Result<Self> {
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

    fn embed(&mut self, input_ids: &Tensor) -> candle_core::Result<Tensor> {
        let (_b, seq_len) = input_ids.dims2()?;
        let mut xs = self.embed_tokens.forward(input_ids)?;
        xs = (xs * (self.hidden_size as f64).sqrt())?;
        for layer in self.layers.iter_mut() {
            xs = layer.forward(&xs)?;
        }
        let xs = self.norm.forward(&xs)?;
        let pooled = xs.narrow(1, seq_len - 1, 1)?.squeeze(1)?;
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
        })
    }

    fn forward(&mut self, xs: &Tensor) -> candle_core::Result<Tensor> {
        let residual = xs;
        let xs = self.input_layernorm.forward(xs)?;
        let xs = self.self_attn.forward(&xs)?;
        let xs = (xs + residual)?;
        let residual = &xs;
        let xs = self.post_attention_layernorm.forward(&xs)?;
        let xs = self.mlp.forward(&xs)?;
        xs + residual
    }
}

struct Attention {
    q_proj: Linear,
    k_proj: Linear,
    v_proj: Linear,
    o_proj: Linear,
    num_heads: usize,
    num_kv_heads: usize,
    head_dim: usize,
    scale: f64,
}

impl Attention {
    fn load(
        cfg: &HarrierConfig,
        vb: VarBuilder,
        _sliding_window: Option<usize>,
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
            num_heads: cfg.num_attention_heads,
            num_kv_heads: cfg.num_key_value_heads,
            head_dim: cfg.head_dim,
            scale: 1.0 / (cfg.query_pre_attn_scalar as f64).sqrt(),
        })
    }

    fn forward(&self, xs: &Tensor) -> candle_core::Result<Tensor> {
        let (b, seq, _h) = xs.dims3()?;
        let q = self
            .q_proj
            .forward(xs)?
            .reshape((b, seq, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;
        let k = self
            .k_proj
            .forward(xs)?
            .reshape((b, seq, self.num_kv_heads, self.head_dim))?
            .transpose(1, 2)?;
        let v = self
            .v_proj
            .forward(xs)?
            .reshape((b, seq, self.num_kv_heads, self.head_dim))?
            .transpose(1, 2)?;
        let k = repeat_kv(k, self.num_heads / self.num_kv_heads.max(1))?;
        let v = repeat_kv(v, self.num_heads / self.num_kv_heads.max(1))?;
        let attn = (q.matmul(&k.transpose(D::Minus1, D::Minus2)?)? * self.scale)?;
        let mask = causal_mask(seq, xs.device())?;
        let attn = softmax(&(attn + mask)?, D::Minus1)?;
        let out =
            attn.matmul(&v)?
                .transpose(1, 2)?
                .reshape((b, seq, self.num_heads * self.head_dim))?;
        self.o_proj.forward(&out)
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

fn causal_mask(seq: usize, device: &Device) -> candle_core::Result<Tensor> {
    let mut data = vec![0f32; seq * seq];
    for i in 0..seq {
        for j in (i + 1)..seq {
            data[i * seq + j] = f32::NEG_INFINITY;
        }
    }
    Tensor::from_vec(data, (1, 1, seq, seq), device)
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
