use anyhow::{bail, Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::t5::{Config, T5ForConditionalGeneration};
use tokenizers::Tokenizer;

const DEFAULT_REPO_ID: &str = "Falconsai/text_summarization";
const EMBEDDED_CONFIG: &str = include_str!("../assets/Falconsai--text_summarization/config.json");
const EMBEDDED_TOKENIZER: &[u8] =
    include_bytes!("../assets/Falconsai--text_summarization/tokenizer.json");
const EMBEDDED_WEIGHTS: &[u8] =
    include_bytes!("../assets/Falconsai--text_summarization/model.safetensors");

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
    /// Load the default Falconsai/text_summarization model.
    /// The model config, tokenizer, and weights are embedded in the binary.
    pub fn new() -> Result<Self> {
        Self::new_from_repo(DEFAULT_REPO_ID)
    }

    /// Load the embedded default model.
    pub fn new_from_repo(repo_id: &str) -> Result<Self> {
        if repo_id != DEFAULT_REPO_ID {
            bail!(
                "hive-summarizer is built with embedded assets for {DEFAULT_REPO_ID}; \
                 cannot load {repo_id} without adding that model to the binary"
            );
        }

        let device = Device::Cpu;

        let config: Config = serde_json::from_str(EMBEDDED_CONFIG)
            .with_context(|| format!("parsing embedded T5 Config for {}", repo_id))?;

        let tokenizer = Tokenizer::from_bytes(EMBEDDED_TOKENIZER)
            .map_err(|e| anyhow::anyhow!("failed to load embedded tokenizer: {}", e))?;

        // Use F32 to match the "F32 weights ~240MB" description; the small model runs fine in F32 on CPU.
        let vb = VarBuilder::from_slice_safetensors(EMBEDDED_WEIGHTS, DType::F32, &device)
            .context("creating VarBuilder from embedded safetensors")?;

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

        // Keep Hive list output scannable; this companion is used for quick agent overviews.
        const MAX_NEW_TOKENS: usize = 32;

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

        Ok(first_sentence(summary.trim()).to_string())
    }
}

fn first_sentence(summary: &str) -> &str {
    summary
        .find(['.', '!', '?'])
        .map(|idx| summary[..=idx].trim())
        .unwrap_or(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test demonstrates that we can load the Falconsai/text_summarization model
    /// (via Candle and embedded model assets) and use it to produce an abstractive summary.
    ///
    /// It will run a short inference on CPU without fetching anything from the network.
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
        assert_ne!(summary, "-", "summary should not be a placeholder");
        // The exact wording varies across model/runtime versions, but it should stay concise.
        assert!(
            summary.len() < input.len() / 2,
            "summary should be substantially shorter than input (got {} chars for {} char input): {}",
            summary.len(),
            input.len(),
            summary
        );
    }
}
