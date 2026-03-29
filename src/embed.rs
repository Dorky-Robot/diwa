//! Embedding generation via fastembed (in-process, no external server needed).

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::path::PathBuf;
use std::sync::Mutex;

/// Lazily initialized embedding model (singleton behind a mutex).
static MODEL: Mutex<Option<TextEmbedding>> = Mutex::new(None);

/// Model cache directory inside ~/.diwa/models/
fn model_cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = home.join(".diwa").join("models");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn with_model<F, T>(f: F) -> Result<T>
where
    F: FnOnce(&mut TextEmbedding) -> Result<T>,
{
    let mut guard = MODEL
        .lock()
        .map_err(|e| anyhow::anyhow!("model lock poisoned: {e}"))?;
    if guard.is_none() {
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallENV15)
                .with_cache_dir(model_cache_dir())
                .with_show_download_progress(true),
        )
        .context("failed to initialize embedding model")?;
        *guard = Some(model);
    }
    f(guard.as_mut().unwrap())
}

/// Generate an embedding for a single text string.
pub fn embed(text: &str) -> Result<Vec<f32>> {
    with_model(|model| {
        let results = model
            .embed(vec![text.to_string()], None)
            .context("embedding failed")?;
        results.into_iter().next().context("empty embedding result")
    })
}

/// Generate embeddings for multiple texts.
pub fn embed_batch(texts: &[String]) -> Result<Vec<Vec<f32>>> {
    with_model(|model| {
        model
            .embed(texts.to_vec(), None)
            .context("batch embedding failed")
    })
}

/// Cosine similarity between two vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// Serialize an embedding to bytes (for SQLite BLOB storage).
pub fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Deserialize an embedding from bytes.
pub fn embedding_from_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_empty() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn test_cosine_similarity_mismatched() {
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn test_embedding_roundtrip() {
        let original = vec![1.5, -2.3, 0.0, 42.0, -0.001];
        let bytes = embedding_to_bytes(&original);
        let restored = embedding_from_bytes(&bytes);
        assert_eq!(original, restored);
    }
}
