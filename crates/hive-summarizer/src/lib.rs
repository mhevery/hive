use anyhow::{Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::t5::{Config, T5ForConditionalGeneration};
use tokenizers::Tokenizer;

/// A thin wrapper around the Falconsai/text_summarization (T5) model loaded via Candle.
///
/// The model is a small T5 (~60M params) fine-tuned for abstractive summarization.
/// It expects inputs prefixed with "summarize: " (standard T5 text-to-text).
pub struct TextSummarizer {
    model: T5ForConditionalGeneration,
    tokenizer: Tokenizer,
    device: Device,
    config: Config,
}

impl TextSummarizer {
    /// Load the default Falconsai/text_summarization model from the Hugging Face Hub.
    /// Weights are cached by hf-hub under ~/.cache/huggingface/hub .
    pub fn new() -> Result<Self> {
        Self::new_from_repo("Falconsai/text_summarization")
    }

    /// Load from an arbitrary HF repo id (e.g. "google-t5/t5-small" for the base).
    pub fn new_from_repo(repo_id: &str) -> Result<Self> {
        let device = Device::Cpu;

        let config_path = hf_resolve_cached(repo_id, "config.json")
            .with_context(|| format!("downloading config.json for {}", repo_id))?;
        let tokenizer_path = hf_resolve_cached(repo_id, "tokenizer.json")
            .with_context(|| format!("downloading tokenizer.json for {}", repo_id))?;
        let weights_path = hf_resolve_cached(repo_id, "model.safetensors")
            .with_context(|| format!("downloading model.safetensors for {}", repo_id))?;

        let config: Config = serde_json::from_str(
            &std::fs::read_to_string(&config_path)
                .with_context(|| format!("reading {:?}", config_path))?,
        )
        .with_context(|| format!("parsing T5 Config for {}", repo_id))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            anyhow::anyhow!("failed to load tokenizer from {:?}: {}", tokenizer_path, e)
        })?;

        // Use F32 to match the "F32 weights ~240MB" description; the small model runs fine in F32 on CPU.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], DType::F32, &device)
                .context("creating VarBuilder from safetensors")?
        };

        let model = T5ForConditionalGeneration::load(vb, &config)
            .context("loading T5ForConditionalGeneration weights into model")?;

        Ok(Self {
            model,
            tokenizer,
            device,
            config,
        })
    }

    /// Summarize the provided text using the T5 model.
    ///
    /// The input is prefixed with the conventional "summarize: " task instruction.
    /// Returns a concise abstractive summary.
    pub fn summarize(&mut self, text: &str) -> Result<String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(String::new());
        }

        let prompt = format!("summarize: {}", text);

        // Tokenize (no special tokens added by us; the tokenizer and model handle it)
        let encoding = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| anyhow::anyhow!("tokenizer encode failed: {}", e))?;

        let mut input_ids: Vec<u32> = encoding.get_ids().to_vec();

        // T5-small style models have a limited context (commonly 512 tokens).
        const MAX_INPUT_LEN: usize = 512;
        if input_ids.len() > MAX_INPUT_LEN {
            input_ids.truncate(MAX_INPUT_LEN);
        }

        let input_ids_tensor = Tensor::new(input_ids.as_slice(), &self.device)
            .context("creating input_ids tensor")?
            .unsqueeze(0)?; // batch dimension -> [1, seq]

        // Greedy generation loop.
        let decoder_start_token_id = self
            .config
            .decoder_start_token_id
            .map(|v| v as u32)
            .unwrap_or(0u32);
        let eos_token_id = self.config.eos_token_id as u32;

        let encoder_output = self
            .model
            .encode(&input_ids_tensor)
            .context("T5 model encoder pass during generation")?;
        self.model.clear_kv_cache();

        let mut output_ids: Vec<u32> = Vec::new();
        let mut decoder_token_id = decoder_start_token_id;

        // Keep summaries short and sweet for this small model.
        const MAX_NEW_TOKENS: usize = 128;

        for _ in 0..MAX_NEW_TOKENS {
            let decoder_tensor = Tensor::new(&[decoder_token_id], &self.device)
                .context("creating decoder_input_ids tensor")?
                .unsqueeze(0)?;

            let logits = self
                .model
                .decode(&decoder_tensor, &encoder_output)
                .context("T5 model decoder pass during generation")?;

            // The forward impl returns [batch, vocab] logits for the just-predicted last token
            // (see narrow + squeeze in the decode path). Squeeze the batch dim for convenience.
            let logits = logits.squeeze(0)?;

            // Greedy selection: argmax over vocab dimension.
            let next_token_id = logits
                .argmax(0)?
                .to_scalar::<u32>()
                .context("extracting argmax token id")?;

            if next_token_id == eos_token_id {
                break;
            }

            output_ids.push(next_token_id);
            decoder_token_id = next_token_id;
        }

        let summary = self
            .tokenizer
            .decode(&output_ids, true)
            .map_err(|e| anyhow::anyhow!("tokenizer decode failed: {}", e))?;

        Ok(summary.trim().to_string())
    }
}

fn hf_resolve_cached(repo_id: &str, filename: &str) -> Result<std::path::PathBuf> {
    let cache_path = hive_hf_cache_dir()
        .join(repo_id.replace('/', "--"))
        .join(filename);
    if cache_path.exists() {
        return Ok(cache_path);
    }

    std::fs::create_dir_all(
        cache_path
            .parent()
            .context("cache path should have a parent directory")?,
    )
    .with_context(|| format!("creating cache directory for {:?}", cache_path))?;

    // Always use a fully qualified absolute URL.
    // This avoids any "RelativeUrlWithoutBase" issues that can occur if
    // HF_ENDPOINT is set to a relative or invalid value in the environment.
    let url = format!("https://huggingface.co/{}/resolve/main/{}", repo_id, filename);
    eprintln!("Downloading from {}", url);

    let mut response = ureq::get(&url)
        .call()
        .with_context(|| format!("requesting {}", url))?
        .into_reader();

    let tmp_path = cache_path.with_extension("download");
    {
        let mut file = std::fs::File::create(&tmp_path)
            .with_context(|| format!("creating temporary download {:?}", tmp_path))?;
        std::io::copy(&mut response, &mut file)
            .with_context(|| format!("writing download to {:?}", tmp_path))?;
    }

    std::fs::rename(&tmp_path, &cache_path)
        .with_context(|| format!("moving {:?} into cache at {:?}", tmp_path, cache_path))?;

    Ok(cache_path)
}

fn hive_hf_cache_dir() -> std::path::PathBuf {
    if let Ok(hf_home) = std::env::var("HF_HOME") {
        return std::path::PathBuf::from(hf_home)
            .join("hub")
            .join("hive-summarizer");
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home)
        .join(".cache")
        .join("huggingface")
        .join("hub")
        .join("hive-summarizer")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test demonstrates that we can load the Falconsai/text_summarization model
    /// (via Candle + hf-hub) and use it to produce an abstractive summary.
    ///
    /// It will:
    /// - Download the model weights on first execution (cached afterwards)
    /// - Run a short inference on CPU
    ///
    /// The first run may take a minute or two depending on bandwidth and CPU.
    #[test]
    fn falconsai_text_summarization_works() {
        let mut summarizer = TextSummarizer::new()
            .expect("failed to load Falconsai/text_summarization model via Candle");

        let input = "Artificial intelligence is transforming software development. \
Agents can now edit code, run commands, and maintain long-running tasks across repositories. \
Observability tools are becoming essential to understand what multiple agents are doing in parallel \
across different working directories and projects.";

        let summary = summarizer
            .summarize(input)
            .expect("model inference for summarization should succeed");

        // Basic sanity checks that demonstrate successful usage.
        assert!(
            !summary.is_empty(),
            "expected a non-empty summary, got empty string"
        );
        // The exact wording varies across model/runtime versions, but it should compress the input.
        assert!(
            summary.len() < input.len(),
            "summary should be shorter than input (got {} chars for {} char input): {}",
            summary.len(),
            input.len(),
            summary
        );
    }
}
