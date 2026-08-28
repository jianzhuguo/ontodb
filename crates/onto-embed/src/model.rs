use std::path::Path;
use std::sync::Mutex;
use ort::session::Session;
use tokenizers::Tokenizer;
use onto_plugin::{PluginError, PluginResult};
use tracing::{info, debug};

/// 内置 Embedding 模型 — 基于 ONNX Runtime 加载和推理。
///
/// 使用 `Mutex` 包装 ONNX Session 以支持 `&self` 推理（ort 2.x 需要 `&mut`）。
pub struct EmbeddingModel {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    dimension: usize,
    max_length: usize,
    normalize: bool,
}

impl EmbeddingModel {
    /// 从模型目录加载 ONNX 模型和 tokenizer。
    pub fn load(
        model_dir: &Path,
        max_length: usize,
        normalize: bool,
    ) -> PluginResult<Self> {
        let model_path = model_dir.join("model.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !model_path.exists() {
            return Err(PluginError::ModelLoad(format!(
                "model.onnx not found at {}", model_path.display()
            )));
        }
        if !tokenizer_path.exists() {
            return Err(PluginError::ModelLoad(format!(
                "tokenizer.json not found at {}", tokenizer_path.display()
            )));
        }

        info!("loading embedding model from {}", model_dir.display());

        let session = Session::builder()
            .map_err(|e| PluginError::ModelLoad(format!("session builder: {}", e)))?
            .commit_from_file(&model_path)
            .map_err(|e| PluginError::ModelLoad(format!("load model: {}", e)))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| PluginError::ModelLoad(format!("load tokenizer: {}", e)))?;

        let dimension = 384;
        info!("embedding model loaded: dim={}, max_length={}", dimension, max_length);

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            dimension,
            max_length,
            normalize,
        })
    }

    /// 生成文本的语义向量。
    pub fn encode(&self, text: &str) -> PluginResult<Vec<f32>> {
        if text.is_empty() {
            return Ok(vec![0.0; self.dimension]);
        }

        let encoding = self.tokenizer
            .encode(text, true)
            .map_err(|e| PluginError::Inference(format!("tokenize: {}", e)))?;

        let ids = encoding.get_ids();
        let attention_mask = encoding.get_attention_mask();
        let token_type_ids = encoding.get_type_ids();

        let len = ids.len().min(self.max_length);
        let ids_vec: Vec<i64> = ids[..len].iter().map(|&x| x as i64).collect();
        let mask_vec: Vec<i64> = attention_mask[..len].iter().map(|&x| x as i64).collect();
        let type_ids_vec: Vec<i64> = token_type_ids[..len].iter().map(|&x| x as i64).collect();

        let ids_val = ort::value::Tensor::from_array(([1usize, len], ids_vec))
            .map_err(|e| PluginError::Inference(format!("tensor: {}", e)))?;
        let mask_val = ort::value::Tensor::from_array(([1usize, len], mask_vec))
            .map_err(|e| PluginError::Inference(format!("tensor: {}", e)))?;
        let type_ids_val = ort::value::Tensor::from_array(([1usize, len], type_ids_vec))
            .map_err(|e| PluginError::Inference(format!("tensor: {}", e)))?;

        let inputs: Vec<(&str, ort::session::SessionInputValue)> = vec![
            ("input_ids", ids_val.into()),
            ("attention_mask", mask_val.into()),
            ("token_type_ids", type_ids_val.into()),
        ];

        // 在 Mutex 锁内完成推理 + 数据提取
        let (hidden_data, actual_dim) = {
            let mut session = self.session.lock().map_err(|e| {
                PluginError::Inference(format!("session lock: {}", e))
            })?;

            let outputs = session.run(inputs)
                .map_err(|e| PluginError::Inference(format!("run: {}", e)))?;

            let output = &outputs[0];
            let (shape, hidden_view) = output.try_extract_tensor::<f32>()
                .map_err(|e| PluginError::Inference(format!("extract: {}", e)))?;

            let dim = if shape.len() >= 3 {
                shape[shape.len() - 1] as usize
            } else {
                self.dimension
            };

            // 拷贝数据出来，释放锁
            (hidden_view.to_vec(), dim)
        };

        let mask_i64: Vec<i64> = attention_mask[..len].iter().map(|&x| x as i64).collect();
        let pooled = mean_pool(&hidden_data, len, &mask_i64, actual_dim);

        let result = if self.normalize { l2_normalize(&pooled) } else { pooled };

        debug!("encoded {} chars -> {} dim", text.len(), result.len());
        Ok(result)
    }

    pub fn dimension(&self) -> usize { self.dimension }
}

fn mean_pool(hidden: &[f32], seq_len: usize, mask: &[i64], dim: usize) -> Vec<f32> {
    let mut result = vec![0.0f32; dim];
    let mut count = 0.0f32;
    for pos in 0..seq_len {
        if pos >= mask.len() || mask[pos] == 0 { continue; }
        let off = pos * dim;
        for d in 0..dim {
            if off + d < hidden.len() { result[d] += hidden[off + d]; }
        }
        count += 1.0;
    }
    if count > 0.0 { for d in 0..dim { result[d] /= count; } }
    result
}

fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 { v.iter().map(|x| x / norm).collect() } else { v.to_vec() }
}
