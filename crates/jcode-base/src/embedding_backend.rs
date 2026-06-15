//! Pluggable embedding backends for memory retrieval.
//!
//! Memory dense-retrieval embeds two kinds of text: stored memories (passages)
//! and the current query. Historically jcode had exactly one embedder, the
//! bundled local all-MiniLM-L6-v2 ONNX model, reached directly via
//! [`crate::embedding`]. This module introduces a small abstraction so the
//! embedder can be swapped (e.g. a stronger local model, or a remote provider
//! like OpenAI when the user has an embeddings-capable key) without the rest of
//! the memory system caring which one is active.
//!
//! Design invariants:
//! - **One vector space per index.** Embeddings from different models are not
//!   comparable. Every backend reports a stable [`EmbeddingBackend::model_id`],
//!   which is stored on each `MemoryEntry` (`embedding_model`). Dense similarity
//!   only compares vectors sharing the active model id; mismatched memories stay
//!   reachable via lexical (BM25) search + RRF fusion, so switching backends
//!   never silently corrupts results.
//! - **Asymmetric query/passage formatting is per-model.** Some models (e5/bge)
//!   require instruction prefixes; others (MiniLM, OpenAI) do not. Each backend
//!   owns its own input formatting via [`EmbeddingBackend::format_query`] /
//!   [`EmbeddingBackend::format_passage`], so callers never hardcode prefixes.
//! - **Local is the always-available default.** Remote backends are opt-in and
//!   only selected when an embeddings-capable credential is present.

use anyhow::Result;

use crate::memory_types::LEGACY_EMBEDDING_MODEL;

/// A source of embedding vectors for memory retrieval.
///
/// Implementations must keep `model_id()` stable for a given vector space: it is
/// persisted alongside each embedding and used to gate cross-model comparisons.
pub trait EmbeddingBackend: Send + Sync {
    /// Stable identifier for the model/vector-space this backend produces, e.g.
    /// `"minilm-l6-v2"` or `"openai:text-embedding-3-small"`. Persisted on
    /// `MemoryEntry::embedding_model`.
    fn model_id(&self) -> &str;

    /// Embedding dimensionality (used for sanity checks and index metadata).
    fn dim(&self) -> usize;

    /// Embed a single text already formatted for this backend's role. Prefer
    /// [`Self::embed_query`] / [`Self::embed_passage`] which apply formatting.
    fn embed_raw(&self, text: &str) -> Result<Vec<f32>>;

    /// Apply this model's query-side formatting (e.g. an `"query: "` prefix).
    /// Default: identity (no prefix), correct for MiniLM and OpenAI.
    fn format_query(&self, text: &str) -> String {
        text.to_string()
    }

    /// Apply this model's passage-side formatting (e.g. a `"passage: "` prefix).
    /// Default: identity.
    fn format_passage(&self, text: &str) -> String {
        text.to_string()
    }

    /// Embed a retrieval query (applies query formatting).
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_raw(&self.format_query(text))
    }

    /// Embed a stored passage/memory (applies passage formatting).
    fn embed_passage(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_raw(&self.format_passage(text))
    }
}

/// The bundled local ONNX embedder (currently all-MiniLM-L6-v2).
///
/// Wraps the process-wide embedder facade in [`crate::embedding`]. Requires no
/// network, no API key, and is always available, so it is the default backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalOnnxBackend;

impl EmbeddingBackend for LocalOnnxBackend {
    fn model_id(&self) -> &str {
        // Matches MemoryEntry::effective_embedding_model() for untagged legacy
        // memories, so existing embeddings remain comparable with new ones.
        LEGACY_EMBEDDING_MODEL
    }

    fn dim(&self) -> usize {
        crate::embedding::embedding_dim()
    }

    fn embed_raw(&self, text: &str) -> Result<Vec<f32>> {
        crate::embedding::embed(text)
    }

    // MiniLM is symmetric and prefix-free: default identity formatting is correct.
}

/// Resolve the active embedding backend.
///
/// For now this always returns the local ONNX backend. Remote backends
/// (OpenAI/openai-compatible) will be added here, selected only when an
/// embeddings-capable API key is configured; see the embedding-backends plan.
pub fn active_backend() -> Box<dyn EmbeddingBackend> {
    Box::new(LocalOnnxBackend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_backend_model_id_matches_legacy_tag() {
        // Critical for backward compatibility: the local backend's model id must
        // equal the legacy tag so pre-tagging memories stay in the same space.
        assert_eq!(LocalOnnxBackend.model_id(), LEGACY_EMBEDDING_MODEL);
    }

    #[test]
    fn default_formatting_is_identity() {
        let b = LocalOnnxBackend;
        assert_eq!(b.format_query("hello"), "hello");
        assert_eq!(b.format_passage("world"), "world");
    }
}
