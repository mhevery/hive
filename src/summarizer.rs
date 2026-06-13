use anyhow::{Context, Result};
use candle_core::{D, DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::t5::{Config, T5ForConditionalGeneration};
use hf_hub::api::sync::Api;
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

        let api = Api::new().context("failed to construct hf-hub Api")?;
        let repo = api.model(repo_id.to_string());

        let config_path = repo
            .get("config.json")
            .with_context(|| format!("downloading config.json for {}", repo_id))?;
        let tokenizer_path = repo
            .get("tokenizer.json")
            .with_context(|| format!("downloading tokenizer.json for {}", repo_id))?;
        let weights_path = repo
            .get("model.safetensors")
            .with_context(|| format!("downloading model.safetensors for {}", repo_id))?;

        let config: Config = serde_json::from_str(
            &std::fs::read_to_string(&config_path)
                .with_context(|| format!("reading {:?}", config_path))?,
        )
        .with_context(|| format!("parsing T5 Config for {}", repo_id))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("failed to load tokenizer from {:?}: {}", tokenizer_path, e))?;

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

        let mut decoder_input_ids: Vec<u32> = vec![decoder_start_token_id];

        // Keep summaries short and sweet for this small model.
        const MAX_NEW_TOKENS: usize = 128;

        for _ in 0..MAX_NEW_TOKENS {
            let decoder_tensor = Tensor::new(decoder_input_ids.as_slice(), &self.device)
                .context("creating decoder_input_ids tensor")?
                .unsqueeze(0)?;

            // Note: forward takes &mut self because of internal KV cache management in the stacks.
            let logits = self
                .model
                .forward(&input_ids_tensor, &decoder_tensor)
                .context("T5 model forward pass during generation")?;

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

            decoder_input_ids.push(next_token_id);
        }

        // Drop the initial decoder start token before decoding.
        let output_ids = if decoder_input_ids.len() > 1 {
            &decoder_input_ids[1..]
        } else {
            &[]
        };

        let summary = self
            .tokenizer
            .decode(output_ids, true)
            .map_err(|e| anyhow::anyhow!("tokenizer decode failed: {}", e))?;

        Ok(summary.trim().to_string())
    }
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
    /// Run with:
    ///   cargo test --features summarizer -- --nocapture
    ///
    /// The first run may take a minute or two depending on bandwidth and CPU.
    #[test]
    #[cfg(feature = "summarizer")]
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
        // Summaries from this model are typically much shorter than the source for this kind of input.
        assert!(
            summary.len() < input.len() / 2,
            "summary should be substantially shorter than input (got {} chars for {} char input): {}",
            summary.len(),
            input.len(),
            summary
        );

        // Print for visibility when running with --nocapture so the user sees the model in action.
        println!("\n=== Summarizer demo (Falconsai/text_summarization via Candle) ===");
        println!("Input ({} chars):\n{}\n", input.len(), input);
        println!("Summary ({} chars):\n{}\n", summary.len(), summary);
    }
}
