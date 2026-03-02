//! Local LLM inference engine using llama-cpp-2.

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing;

/// Configuration for the LLM engine.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// Path to the GGUF model file.
    pub model_path: PathBuf,
    /// Context window length (tokens).
    pub context_length: u32,
    /// Number of layers to offload to GPU (0 = CPU only, 99 = all).
    pub gpu_layers: u32,
    /// Number of threads for inference (0 = auto-detect).
    pub threads: u32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::new(),
            context_length: 4096,
            gpu_layers: 99,
            threads: 0,
        }
    }
}

/// Parameters controlling text generation.
#[derive(Debug, Clone)]
pub struct GenerationParams {
    pub max_tokens: u32,
    pub temperature: f32,
    pub top_p: f32,
    pub stop_sequences: Vec<String>,
}

impl Default for GenerationParams {
    fn default() -> Self {
        Self {
            max_tokens: 2048,
            temperature: 0.7,
            top_p: 0.9,
            stop_sequences: Vec::new(),
        }
    }
}

/// Local LLM inference engine wrapping llama-cpp-2.
pub struct LlmEngine {
    model: llama_cpp_2::model::LlamaModel,
    _backend: llama_cpp_2::llama_backend::LlamaBackend,
    context_length: u32,
}

impl LlmEngine {
    /// Load a GGUF model from disk.
    pub fn load(config: &LlmConfig) -> Result<Self> {
        tracing::info!(
            model_path = %config.model_path.display(),
            context_length = config.context_length,
            gpu_layers = config.gpu_layers,
            threads = config.threads,
            "Loading LLM model"
        );

        let backend = llama_cpp_2::llama_backend::LlamaBackend::init()
            .map_err(|e| Error::config(format!("Failed to init llama backend: {e}")))?;

        let model_params = {
            let params = llama_cpp_2::model::params::LlamaModelParams::default();
            params.with_n_gpu_layers(config.gpu_layers)
        };

        let model = llama_cpp_2::model::LlamaModel::load_from_file(
            &backend,
            &config.model_path,
            &model_params,
        )
        .map_err(|e| Error::config(format!("Failed to load model: {e}")))?;

        tracing::info!("LLM model loaded successfully");

        Ok(Self {
            model,
            _backend: backend,
            context_length: config.context_length,
        })
    }

    /// Generate a completion, returning collected token strings.
    ///
    /// Returns a vec of decoded token strings. The vec is empty if generation produced nothing.
    pub fn complete_streaming(
        &self,
        prompt: &str,
        params: &GenerationParams,
    ) -> Result<Vec<String>> {
        use llama_cpp_2::context::params::LlamaContextParams;
        use llama_cpp_2::llama_batch::LlamaBatch;
        use llama_cpp_2::sampling::LlamaSampler;

        let threads = std::thread::available_parallelism()
            .map(|n| (n.get() as u32).max(1) / 2)
            .unwrap_or(4)
            .max(1);

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(std::num::NonZeroU32::new(self.context_length))
            .with_n_threads(threads as i32)
            .with_n_threads_batch(threads as i32);

        let mut ctx = self
            .model
            .new_context(&self._backend, ctx_params)
            .map_err(|e| Error::Other(format!("Failed to create context: {e}")))?;

        // Tokenize the prompt
        let tokens = self
            .model
            .str_to_token(prompt, llama_cpp_2::model::AddBos::Always)
            .map_err(|e| Error::Other(format!("Tokenization failed: {e}")))?;

        if tokens.len() as u32 >= self.context_length {
            return Err(Error::Other(format!(
                "Prompt ({} tokens) exceeds context length ({})",
                tokens.len(),
                self.context_length
            )));
        }

        // Create batch and fill with prompt tokens
        let mut batch = LlamaBatch::new(self.context_length as usize, 1);

        for (i, &token) in tokens.iter().enumerate() {
            let is_last = i == tokens.len() - 1;
            batch
                .add(token, i as i32, &[0], is_last)
                .map_err(|_| Error::Other("Failed to add token to batch".into()))?;
        }

        // Process prompt
        ctx.decode(&mut batch)
            .map_err(|e| Error::Other(format!("Decode failed: {e}")))?;

        // Build sampler chain: temperature -> top-p -> dist (random sampling)
        let seed = rand::random::<u32>();
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::temp(params.temperature),
            LlamaSampler::top_p(params.top_p, 1),
            LlamaSampler::dist(seed),
        ]);

        // Create a UTF-8 decoder for token-to-string conversion
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        // Generate tokens
        let mut output_tokens = Vec::new();
        let mut n_cur = tokens.len() as i32;
        let max_gen = params
            .max_tokens
            .min(self.context_length - tokens.len() as u32);
        let mut accumulated = String::new();

        for _ in 0..max_gen {
            // Sample next token using the sampler chain
            let new_token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(new_token);

            // Check for end of generation
            if self.model.is_eog_token(new_token) {
                break;
            }

            // Decode token to string
            let token_str = self
                .model
                .token_to_piece(new_token, &mut decoder, false, None)
                .unwrap_or_default();

            // Check stop sequences against accumulated output
            accumulated.push_str(&token_str);
            let should_stop = params
                .stop_sequences
                .iter()
                .any(|seq| accumulated.contains(seq.as_str()));

            if should_stop {
                // Remove the stop sequence from the last token if partially included
                break;
            }

            output_tokens.push(token_str);

            // Prepare next batch
            batch.clear();
            batch
                .add(new_token, n_cur, &[0], true)
                .map_err(|_| Error::Other("Failed to add token to batch".into()))?;
            n_cur += 1;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Other(format!("Decode step failed: {e}")))?;
        }

        Ok(output_tokens)
    }

    /// Generate a completion, sending tokens through an mpsc channel as they are produced.
    ///
    /// Checks `cancel` at the top of each iteration and breaks if set.
    /// Breaks if the receiver is dropped (tx.blocking_send fails).
    pub fn complete_streaming_channel(
        &self,
        prompt: &str,
        params: &GenerationParams,
        tx: tokio::sync::mpsc::Sender<String>,
        cancel: Arc<AtomicBool>,
    ) -> Result<()> {
        use llama_cpp_2::context::params::LlamaContextParams;
        use llama_cpp_2::llama_batch::LlamaBatch;
        use llama_cpp_2::sampling::LlamaSampler;

        let threads = std::thread::available_parallelism()
            .map(|n| (n.get() as u32).max(1) / 2)
            .unwrap_or(4)
            .max(1);

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(std::num::NonZeroU32::new(self.context_length))
            .with_n_threads(threads as i32)
            .with_n_threads_batch(threads as i32);

        let mut ctx = self
            .model
            .new_context(&self._backend, ctx_params)
            .map_err(|e| Error::Other(format!("Failed to create context: {e}")))?;

        let tokens = self
            .model
            .str_to_token(prompt, llama_cpp_2::model::AddBos::Always)
            .map_err(|e| Error::Other(format!("Tokenization failed: {e}")))?;

        if tokens.len() as u32 >= self.context_length {
            return Err(Error::Other(format!(
                "Prompt ({} tokens) exceeds context length ({})",
                tokens.len(),
                self.context_length
            )));
        }

        let mut batch = LlamaBatch::new(self.context_length as usize, 1);

        for (i, &token) in tokens.iter().enumerate() {
            let is_last = i == tokens.len() - 1;
            batch
                .add(token, i as i32, &[0], is_last)
                .map_err(|_| Error::Other("Failed to add token to batch".into()))?;
        }

        ctx.decode(&mut batch)
            .map_err(|e| Error::Other(format!("Decode failed: {e}")))?;

        let seed = rand::random::<u32>();
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::temp(params.temperature),
            LlamaSampler::top_p(params.top_p, 1),
            LlamaSampler::dist(seed),
        ]);

        let mut decoder = encoding_rs::UTF_8.new_decoder();

        let mut n_cur = tokens.len() as i32;
        let max_gen = params
            .max_tokens
            .min(self.context_length - tokens.len() as u32);
        let mut accumulated = String::new();

        for _ in 0..max_gen {
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            let new_token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(new_token);

            if self.model.is_eog_token(new_token) {
                break;
            }

            let token_str = self
                .model
                .token_to_piece(new_token, &mut decoder, false, None)
                .unwrap_or_default();

            accumulated.push_str(&token_str);
            let should_stop = params
                .stop_sequences
                .iter()
                .any(|seq| accumulated.contains(seq.as_str()));

            if should_stop {
                break;
            }

            // Send token through channel; break if receiver dropped
            if tx.blocking_send(token_str).is_err() {
                break;
            }

            batch.clear();
            batch
                .add(new_token, n_cur, &[0], true)
                .map_err(|_| Error::Other("Failed to add token to batch".into()))?;
            n_cur += 1;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Other(format!("Decode step failed: {e}")))?;
        }

        Ok(())
    }

    /// Get the model's context length.
    pub fn context_length(&self) -> u32 {
        self.context_length
    }
}

/// Validate that a model path exists and is readable.
pub fn validate_model_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(Error::config(format!(
            "Model file not found: {}",
            path.display()
        )));
    }
    if !path.is_file() {
        return Err(Error::config(format!(
            "Model path is not a file: {}",
            path.display()
        )));
    }
    // Check extension
    if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
        tracing::warn!(
            path = %path.display(),
            "Model file does not have .gguf extension"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_llm_config_default() {
        let config = LlmConfig::default();
        assert_eq!(config.context_length, 4096);
        assert_eq!(config.gpu_layers, 99);
        assert_eq!(config.threads, 0);
    }

    #[test]
    fn test_generation_params_default() {
        let params = GenerationParams::default();
        assert_eq!(params.max_tokens, 2048);
        assert!((params.temperature - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn test_validate_model_path_missing() {
        let result = validate_model_path(&PathBuf::from("/nonexistent/model.gguf"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_validate_model_path_directory() {
        let result = validate_model_path(&std::env::temp_dir());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not a file"));
    }

    #[test]
    fn test_validate_model_path_valid_gguf() {
        let dir = tempfile::tempdir().unwrap();
        let model_path = dir.path().join("model.gguf");
        std::fs::write(&model_path, b"fake gguf data").unwrap();
        let result = validate_model_path(&model_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_model_path_wrong_extension() {
        let dir = tempfile::tempdir().unwrap();
        let model_path = dir.path().join("model.bin");
        std::fs::write(&model_path, b"fake data").unwrap();
        // Should still succeed but emit a warning
        let result = validate_model_path(&model_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_llm_config_custom() {
        let config = LlmConfig {
            model_path: PathBuf::from("/some/path.gguf"),
            context_length: 8192,
            gpu_layers: 0,
            threads: 4,
        };
        assert_eq!(config.context_length, 8192);
        assert_eq!(config.gpu_layers, 0);
        assert_eq!(config.threads, 4);
    }

    #[test]
    fn test_generation_params_custom() {
        let params = GenerationParams {
            max_tokens: 512,
            temperature: 0.5,
            top_p: 0.8,
            stop_sequences: vec!["<stop>".to_string()],
        };
        assert_eq!(params.max_tokens, 512);
        assert_eq!(params.stop_sequences.len(), 1);
    }
}
