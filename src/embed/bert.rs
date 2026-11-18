//! A BERT-family sentence embedder (default: BAAI/bge-small-en-v1.5) running
//! on the CPU with candle. Model files are downloaded once into
//! `<data_dir>/models/<repo>/`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use super::Embedder;

const FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];
const MAX_TOKENS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pooling {
    /// [CLS] token (BGE family).
    Cls,
    /// Mean over tokens (MiniLM / sentence-transformers family).
    Mean,
}

pub struct BertEmbedder {
    model_id: String,
    model: BertModel,
    // Tokenizer padding settings are mutated per call; keep it behind a lock.
    tokenizer: Mutex<Tokenizer>,
    device: Device,
    dim: usize,
    pooling: Pooling,
    query_prefix: &'static str,
}

impl BertEmbedder {
    /// Loads a model from a directory holding config.json, tokenizer.json
    /// and model.safetensors.
    pub fn load(model_id: &str, dir: &Path) -> Result<Self> {
        let device = Device::Cpu;
        let config: BertConfig = serde_json::from_str(
            &std::fs::read_to_string(dir.join("config.json")).context("reading config.json")?,
        )
        .context("parsing config.json")?;
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| anyhow::anyhow!("loading tokenizer: {e}"))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|e| anyhow::anyhow!("tokenizer truncation: {e}"))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            ..Default::default()
        }));
        // SAFETY: the file is ours and is not modified while mapped.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[dir.join("model.safetensors")],
                DType::F32,
                &device,
            )?
        };
        let model = BertModel::load(vb, &config).context("loading BERT weights")?;
        let lower = model_id.to_lowercase();
        let (pooling, query_prefix) = if lower.contains("bge") {
            (
                Pooling::Cls,
                "Represent this sentence for searching relevant passages: ",
            )
        } else {
            (Pooling::Mean, "")
        };
        Ok(Self {
            model_id: model_id.to_owned(),
            model,
            tokenizer: Mutex::new(tokenizer),
            device,
            dim: config.hidden_size,
            pooling,
            query_prefix,
        })
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let encodings = {
            let tok = self.tokenizer.lock().unwrap_or_else(|e| e.into_inner());
            tok.encode_batch(texts.to_vec(), true)
                .map_err(|e| anyhow::anyhow!("tokenizing: {e}"))?
        };
        let ids: Vec<Tensor> = encodings
            .iter()
            .map(|e| Tensor::new(e.get_ids(), &self.device))
            .collect::<candle_core::Result<_>>()?;
        let masks: Vec<Tensor> = encodings
            .iter()
            .map(|e| Tensor::new(e.get_attention_mask(), &self.device))
            .collect::<candle_core::Result<_>>()?;
        let input_ids = Tensor::stack(&ids, 0)?;
        let attention_mask = Tensor::stack(&masks, 0)?;
        let token_type_ids = input_ids.zeros_like()?;
        let hidden = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))?;
        let pooled = match self.pooling {
            Pooling::Cls => hidden.narrow(1, 0, 1)?.squeeze(1)?,
            Pooling::Mean => {
                let mask = attention_mask.to_dtype(DType::F32)?.unsqueeze(2)?;
                let summed = hidden.broadcast_mul(&mask)?.sum(1)?;
                let counts = mask.sum(1)?.clamp(1e-9, f64::MAX)?;
                summed.broadcast_div(&counts)?
            }
        };
        let mut out: Vec<Vec<f32>> = pooled.to_vec2()?;
        for v in &mut out {
            super::normalize(v);
        }
        Ok(out)
    }
}

impl Embedder for BertEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = self.embed(&[format!("{}{text}", self.query_prefix)])?;
        v.pop().context("no embedding returned")
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts)
    }
}

/// Where a model's files live under the models directory.
pub fn model_dir(models_dir: &Path, model_id: &str) -> PathBuf {
    models_dir.join(model_id.replace('/', "--"))
}

/// Downloads any missing model files. Each file is written to a `.part`
/// file first and renamed, so an interrupted download is retried cleanly.
pub async fn ensure_downloaded(
    client: &reqwest::Client,
    hub_url: &str,
    model_id: &str,
    dir: &Path,
) -> Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    for file in FILES {
        let dest = dir.join(file);
        if dest.exists() {
            continue;
        }
        let url = format!(
            "{}/{model_id}/resolve/main/{file}",
            hub_url.trim_end_matches('/')
        );
        tracing::info!(%url, "downloading model file");
        let resp = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("fetching {url}"))?;
        if !resp.status().is_success() {
            bail!("{url} returned {}", resp.status());
        }
        let bytes = resp.bytes().await?;
        if bytes.is_empty() {
            bail!("{url} was empty");
        }
        let part = dir.join(format!("{file}.part"));
        tokio::fs::write(&part, &bytes).await?;
        tokio::fs::rename(&part, &dest).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_dir_is_flat() {
        assert_eq!(
            model_dir(Path::new("/m"), "BAAI/bge-small-en-v1.5"),
            PathBuf::from("/m/BAAI--bge-small-en-v1.5")
        );
    }

    /// Runs the real model when LIBRARY_TEST_MODEL_DIR points at a downloaded
    /// copy of bge-small-en-v1.5 (skipped otherwise).
    #[test]
    fn real_model_ranks_related_text_higher() {
        let Ok(dir) = std::env::var("LIBRARY_TEST_MODEL_DIR") else {
            eprintln!("skipping: LIBRARY_TEST_MODEL_DIR not set");
            return;
        };
        let e = BertEmbedder::load("BAAI/bge-small-en-v1.5", Path::new(&dir)).unwrap();
        assert_eq!(e.dim(), 384);
        let docs = e
            .embed_documents(&[
                "Moby-Dick. Herman Melville. A whaling voyage across the ocean.".into(),
                "Trangia stove. Camping cooker with pans and a kettle.".into(),
            ])
            .unwrap();
        let q = e.embed_query("books about the sea").unwrap();
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(dot(&q, &docs[0]) > dot(&q, &docs[1]));
        let norm = dot(&q, &q).sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
    }
}

#[cfg(test)]
mod calibrate {
    use super::*;

    /// Prints query/document similarities for tuning thresholds:
    /// LIBRARY_TEST_MODEL_DIR=… cargo test --release calibrate -- --nocapture
    #[test]
    fn calibrate_print_similarities() {
        let Ok(dir) = std::env::var("LIBRARY_TEST_MODEL_DIR") else {
            return;
        };
        let e = BertEmbedder::load("BAAI/bge-small-en-v1.5", Path::new(&dir)).unwrap();
        let docs: Vec<String> = [
            "Moby-Dick. Author: Herman Melville. Book. whaling voyage, obsession, Captain Ahab. 1851",
            "The Old Man and the Sea. Author: Ernest Hemingway. Book. fisherman, marlin, Gulf Stream. 1952",
            "A Wizard of Earthsea. Author: Ursula K. Le Guin. Book. young wizard, school of magic, archipelago. 1968",
            "Pride and Prejudice. Author: Jane Austen. Book. Regency romance, marriage, class. 2003",
            "La Mer. Artist: Claude Debussy. Music. orchestral impressions of the ocean. 1964",
            "Low. Artist: David Bowie. Music. ambient, Berlin, electronic. 1977",
            "My Neighbour Totoro. Director: Hayao Miyazaki. Film. forest spirits, childhood. 1988",
            "Ponyo. Director: Hayao Miyazaki. Film. goldfish girl, ocean, tsunami. 2008",
            "MSR Hubba Hubba NX 2. Brand: MSR. Equipment Camping. two person backpacking tent. 2023",
            "Trangia stove set. Brand: Trangia. Equipment Camping. camp cooker, pans, kettle, meths burner. 2019",
            "Cordless drill. Brand: Makita. Equipment Tools. drilling, screwdriver, DIY. 2021",
        ]
        .map(String::from)
        .to_vec();
        let dv = e.embed_documents(&docs).unwrap();
        for q in [
            "books about the sea",
            "ocean",
            "camping gear",
            "cooking outdoors",
            "japanese animation",
            "electronic music",
            "love story",
            "le guin",
            "hammer",
            "films for a rainy sunday",
        ] {
            let qv = e.embed_query(q).unwrap();
            let mut sims: Vec<(f32, &str)> = dv
                .iter()
                .zip(&docs)
                .map(|(d, t)| {
                    (
                        d.iter().zip(&qv).map(|(a, b)| a * b).sum::<f32>(),
                        t.split('.').next().unwrap(),
                    )
                })
                .collect();
            sims.sort_by(|a, b| b.0.total_cmp(&a.0));
            let line: Vec<String> = sims.iter().map(|(s, t)| format!("{t}={s:.3}")).collect();
            println!("{q:28} {}", line.join("  "));
        }
    }
}
