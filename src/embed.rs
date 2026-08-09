//! Sentence embeddings, computed locally.
//!
//! Wraps all-MiniLM-L6-v2 running on the CPU through candle. Verses and spoken
//! phrases become 384-dimension unit vectors, so how close two pieces of text
//! are in meaning is just their dot product.
//!
//! Everything here is offline. The weights ship with the app, and no part of
//! matching touches the network — which is the point: exact detection and
//! paraphrase matching both have to keep working in a hall with no wifi.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

/// all-MiniLM-L6-v2's output width.
pub const DIMENSIONS: usize = 384;

/// Verses and spoken phrases are short; anything longer is padding we would
/// only pay for.
const MAX_TOKENS: usize = 128;

pub struct Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl Embedder {
    /// Loads the bundled model. Weights are stored as f16 and converted to f32
    /// here — half the file size, full inference speed.
    pub fn load(directory: &Path) -> Result<Self> {
        let config: Config = serde_json::from_str(
            &std::fs::read_to_string(directory.join("config.json"))
                .context("could not read the embedding model config")?,
        )
        .context("the embedding model config is malformed")?;

        let mut tokenizer = Tokenizer::from_file(directory.join("tokenizer.json"))
            .map_err(|err| anyhow!("could not load the tokenizer: {err}"))?;
        tokenizer
            .with_padding(Some(PaddingParams {
                strategy: PaddingStrategy::BatchLongest,
                ..Default::default()
            }))
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|err| anyhow!("could not configure the tokenizer: {err}"))?;

        let device = Device::Cpu;
        let weights = directory.join("model.safetensors");
        // Safety: the file is bundled with the app and read-only; the mapping
        // lives as long as the model that borrows from it.
        let builder = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights], DType::F32, &device)
                .context("could not load the embedding model weights")?
        };
        let model = BertModel::load(builder, &config)
            .context("could not build the embedding model")?;

        Ok(Self { model, tokenizer, device })
    }

    /// Embeds a batch, returning one unit vector per input.
    ///
    /// Batching matters: the cost is dominated by matrix multiplies that get
    /// far better throughput over many sentences at once than one at a time.
    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|err| anyhow!("could not tokenize: {err}"))?;

        let mut id_rows = Vec::with_capacity(encodings.len());
        let mut mask_rows = Vec::with_capacity(encodings.len());
        for encoding in &encodings {
            id_rows.push(Tensor::new(encoding.get_ids(), &self.device)?);
            mask_rows.push(Tensor::new(encoding.get_attention_mask(), &self.device)?);
        }

        let input_ids = Tensor::stack(&id_rows, 0)?;
        let attention_mask = Tensor::stack(&mask_rows, 0)?;
        let token_type_ids = input_ids.zeros_like()?;

        // [batch, tokens, hidden]
        let hidden = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))?;

        mean_pool(&hidden, &attention_mask)
    }

    pub fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut vectors = self.embed(&[text.to_string()])?;
        vectors
            .pop()
            .ok_or_else(|| anyhow!("the embedder returned nothing"))
    }
}

/// Averages token vectors, ignoring padding, then scales to unit length.
///
/// This is what all-MiniLM-L6-v2 was trained to have done to it — taking the
/// CLS token instead gives noticeably worse similarity. Normalising means
/// cosine similarity later reduces to a plain dot product.
fn mean_pool(hidden: &Tensor, attention_mask: &Tensor) -> Result<Vec<Vec<f32>>> {
    let mask = attention_mask.to_dtype(DType::F32)?.unsqueeze(2)?;

    let masked = hidden.broadcast_mul(&mask)?;
    let summed = masked.sum(1)?;
    // Clamp so an all-padding row cannot divide by zero.
    let counts = mask.sum(1)?.clamp(1e-9, f32::INFINITY)?;
    let mean = summed.broadcast_div(&counts)?;

    let norms = mean.sqr()?.sum_keepdim(1)?.sqrt()?.clamp(1e-12, f32::INFINITY)?;
    let normalized = mean.broadcast_div(&norms)?;

    Ok(normalized.to_vec2::<f32>()?)
}

/// Similarity of two unit vectors, which for normalised inputs is their dot
/// product. Returns 0 if the vectors do not line up in length.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Packs a unit vector for storage as a SQLite blob.
pub fn to_blob(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Unpacks a stored vector, rejecting anything the wrong size.
pub fn from_blob(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() != DIMENSIONS * 4 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/embedder")
    }

    fn embedder() -> Option<Embedder> {
        if !model_dir().join("model.safetensors").exists() {
            return None;
        }
        Some(Embedder::load(&model_dir()).expect("model should load"))
    }

    #[test]
    fn blob_round_trip() {
        let vector: Vec<f32> = (0..DIMENSIONS).map(|i| i as f32 / 1000.0).collect();
        let restored = from_blob(&to_blob(&vector)).expect("should decode");
        assert_eq!(restored, vector);
    }

    #[test]
    fn a_truncated_blob_is_rejected_rather_than_misread() {
        assert!(from_blob(&[0u8; 16]).is_none());
        assert!(from_blob(&[]).is_none());
    }

    #[test]
    fn cosine_of_identical_vectors_is_one() {
        let vector = vec![0.5f32; DIMENSIONS];
        let norm: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        let unit: Vec<f32> = vector.iter().map(|v| v / norm).collect();
        assert!((cosine(&unit, &unit) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_of_mismatched_lengths_is_zero() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0]), 0.0);
    }

    // ---------------------------------------------------------------------
    // Against the real bundled model.
    //
    //   cargo test --lib -- --ignored --nocapture
    //
    // Ignored by default: loading 45 MB of weights is too slow for every run,
    // and the files are not committed.
    // ---------------------------------------------------------------------

    #[test]
    #[ignore = "needs the bundled model; run scripts/build-embedder.mjs first"]
    fn produces_unit_vectors_of_the_right_width() {
        let Some(embedder) = embedder() else { panic!("model not built") };
        let vectors = embedder
            .embed(&["For God so loved the world".to_string(), "Jesus wept".to_string()])
            .expect("embed");

        assert_eq!(vectors.len(), 2);
        for vector in &vectors {
            assert_eq!(vector.len(), DIMENSIONS);
            let length: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!((length - 1.0).abs() < 1e-4, "not normalised: {length}");
        }
    }

    #[test]
    #[ignore = "needs the bundled model; run scripts/build-embedder.mjs first"]
    fn a_paraphrase_scores_higher_than_unrelated_text() {
        let Some(embedder) = embedder() else { panic!("model not built") };

        let verse = embedder
            .embed_one("For God so loved the world, that he gave his only begotten Son")
            .expect("embed");
        let paraphrase = embedder
            .embed_one("God loved the world so much that he gave his only son")
            .expect("embed");
        let unrelated = embedder
            .embed_one("Please remember to silence your mobile phones before the service")
            .expect("embed");

        let close = cosine(&verse, &paraphrase);
        let far = cosine(&verse, &unrelated);

        assert!(close > 0.7, "paraphrase scored only {close:.3}");
        assert!(far < 0.3, "unrelated text scored {far:.3}");
        assert!(close > far + 0.4, "too little separation: {close:.3} vs {far:.3}");
    }

    #[test]
    #[ignore = "benchmark; run with --release --ignored --nocapture"]
    fn embedding_throughput() {
        let Some(embedder) = embedder() else { panic!("model not built") };
        let corpus: Vec<String> = (0..256)
            .map(|i| format!("And it came to pass in the {i}th year that the word of the LORD came unto the prophet saying, go and speak unto the people."))
            .collect();

        // Warm up so page faults on the mmapped weights are not counted.
        let _ = embedder.embed(&corpus[..8].to_vec()).expect("warmup");

        let started = std::time::Instant::now();
        for chunk in corpus.chunks(32) {
            embedder.embed(&chunk.to_vec()).expect("embed");
        }
        let rate = corpus.len() as f64 / started.elapsed().as_secs_f64();
        println!("\n  {rate:.0} verses/sec -> a full translation in {:.0}s\n", 31_102.0 / rate);
    }

    #[test]
    #[ignore = "needs the bundled model; run scripts/build-embedder.mjs first"]
    fn batching_gives_the_same_answer_as_one_at_a_time() {
        let Some(embedder) = embedder() else { panic!("model not built") };
        let texts = [
            "The LORD is my shepherd; I shall not want.".to_string(),
            "In the beginning God created the heaven and the earth.".to_string(),
            "Jesus wept.".to_string(),
        ];

        let batched = embedder.embed(&texts).expect("batch");
        for (index, text) in texts.iter().enumerate() {
            let single = embedder.embed_one(text).expect("single");
            let agreement = cosine(&batched[index], &single);
            // Padding to the longest sequence must not change the result.
            assert!(agreement > 0.999, "batch differs from single: {agreement:.5}");
        }
    }
}

