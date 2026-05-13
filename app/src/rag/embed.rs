//! Embedding client wiring for the RAG subsystem.
//!
//! Constructs a `genai::Client` that targets the user's configured embed
//! provider (default: Ollama at `http://localhost:11434/`) and exposes thin
//! async wrappers around `Client::embed` / `Client::embed_batch`.
//!
//! Design (TDD §7.1):
//! - Client is constructed per-request, not held as a singleton. Construction
//!   is cheap (reqwest::Client + adapter table); the embed model may change in
//!   settings between indexer runs; there is no connection pool to amortize.
//! - A `tokio::sync::Semaphore` with 2 permits bounds concurrent in-flight
//!   embed requests to Ollama, which serves one model-request at a time.
//!   Oversubscribing just queues inside Ollama and adds latency.

use std::sync::Arc;

use genai::{
    adapter::AdapterKind,
    resolver::{AuthData, Endpoint, ServiceTargetResolver},
    Client, ModelIden, ServiceTarget,
};
use once_cell::sync::Lazy;
use thiserror::Error;
use tokio::sync::Semaphore;

/// Default embedding model. `nomic-embed-text` is 768-dim, fast, decent quality,
/// and pre-bundled with Ollama via `ollama pull nomic-embed-text`.
pub const DEFAULT_EMBED_MODEL: &str = "nomic-embed-text";

/// Default Ollama base URL. Trailing slash matters — the genai Ollama adapter
/// constructs `{base_url}api/embed` via plain string concatenation.
pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434/";

/// Concurrency cap for in-flight embed requests. Ollama serves one
/// model-request at a time; oversubscribing this just queues inside Ollama
/// and adds tail latency. Two permits = one in-flight + one queued.
const EMBED_PARALLELISM: usize = 2;

static EMBED_SEMAPHORE: Lazy<Arc<Semaphore>> =
    Lazy::new(|| Arc::new(Semaphore::new(EMBED_PARALLELISM)));

/// Errors that can occur during embedding.
///
/// User-visible messaging for each variant is specified in TDD §7.1.
#[derive(Debug, Error)]
pub enum EmbedError {
    /// Ollama (or the configured provider) is not reachable.
    #[error("embed provider not reachable at {endpoint}: {source}")]
    ConnectionRefused {
        endpoint: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The requested model is not installed on the provider side.
    /// User must run `ollama pull <model>` (or the equivalent for their provider).
    #[error("embed model `{model}` not found on provider; run `ollama pull {model}`")]
    ModelNotFound { model: String },

    /// Returned vector had the wrong dimensionality. This typically means the
    /// configured embed model was swapped without re-indexing — see TDD §10.2.
    #[error("embedding dimension mismatch: got {got}, expected {expected}")]
    DimensionMismatch { got: usize, expected: usize },

    /// Catch-all for upstream errors that didn't match a more specific variant.
    #[error("embed provider error: {0}")]
    Provider(String),
}

impl From<genai::Error> for EmbedError {
    fn from(e: genai::Error) -> Self {
        // genai's error surface doesn't distinguish "model not found" cleanly;
        // we pattern-match on the inner message at the boundary. The strings
        // we look for are stable across the genai 0.6 line.
        let msg = e.to_string();
        if msg.contains("connection refused")
            || msg.contains("Connection refused")
            || msg.contains("error sending request")
        {
            // We don't have the endpoint at this scope; caller should wrap with
            // ConnectionRefused if it has more context. Fall back to Provider.
            EmbedError::Provider(msg)
        } else if msg.contains("model") && (msg.contains("not found") || msg.contains("404")) {
            // genai wraps Ollama's "model not found" as a model-call error.
            // Best-effort extraction of the model name.
            EmbedError::ModelNotFound {
                model: extract_model_name(&msg).unwrap_or_else(|| "<unknown>".to_owned()),
            }
        } else {
            EmbedError::Provider(msg)
        }
    }
}

fn extract_model_name(msg: &str) -> Option<String> {
    // Ollama's error is shaped like: `model "nomic-embed-text" not found, try pulling it first`.
    // We pull the quoted name if present.
    let start = msg.find('"')?;
    let rest = &msg[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// Configuration for the embed client. In v1 we only support Ollama, but this
/// shape leaves room to add other providers without breaking callers.
#[derive(Clone, Debug)]
pub struct EmbedClientConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

impl Default for EmbedClientConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_OLLAMA_BASE_URL.to_owned(),
            model: DEFAULT_EMBED_MODEL.to_owned(),
            api_key: None,
        }
    }
}

/// Build a `genai::Client` configured to talk to the embed provider.
///
/// The returned client uses a `ServiceTargetResolver` that forces
/// `AdapterKind::Ollama` regardless of the model name, so model names that
/// don't match Ollama's heuristics still route correctly.
///
/// Construction is cheap (reqwest::Client + adapter table); call per-request
/// or per-indexer-run rather than holding a singleton — see TDD §7.1.
pub fn build_embed_client(config: &EmbedClientConfig) -> Client {
    let base_url = config.base_url.clone();
    let api_key = config.api_key.clone().unwrap_or_default();

    let resolver = ServiceTargetResolver::from_resolver_fn(
        move |service_target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
            let ServiceTarget { model, .. } = service_target;
            let endpoint = Endpoint::from_owned(base_url.clone());
            let auth = AuthData::from_single(api_key.clone());
            let model = ModelIden::new(AdapterKind::Ollama, model.model_name);
            Ok(ServiceTarget {
                endpoint,
                auth,
                model,
            })
        },
    );

    Client::builder()
        .with_service_target_resolver(resolver)
        .build()
}

/// Embed a single text string. Returns the vector and its dimensionality.
///
/// If `expected_dimensions` is `Some`, the returned vector is validated to
/// match exactly; on mismatch, returns [`EmbedError::DimensionMismatch`].
pub async fn embed_text(
    client: &Client,
    model: &str,
    text: &str,
    expected_dimensions: Option<usize>,
) -> Result<Vec<f32>, EmbedError> {
    let _permit = EMBED_SEMAPHORE
        .acquire()
        .await
        .expect("embed semaphore should never close");

    let response = client.embed(model, text, None).await?;
    let embedding = response
        .first_embedding()
        .ok_or_else(|| EmbedError::Provider("empty embedding response".to_owned()))?;

    if let Some(expected) = expected_dimensions {
        if embedding.dimensions != expected {
            return Err(EmbedError::DimensionMismatch {
                got: embedding.dimensions,
                expected,
            });
        }
    }

    Ok(embedding.vector.clone())
}

/// Embed a batch of text strings in a single request. Returns vectors in the
/// same order as `texts`. Empty input returns an empty Vec without calling
/// the provider.
pub async fn embed_batch(
    client: &Client,
    model: &str,
    texts: Vec<String>,
    expected_dimensions: Option<usize>,
) -> Result<Vec<Vec<f32>>, EmbedError> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }

    let _permit = EMBED_SEMAPHORE
        .acquire()
        .await
        .expect("embed semaphore should never close");

    let expected_len = texts.len();
    let response = client.embed_batch(model, texts, None).await?;

    if response.embeddings.len() != expected_len {
        return Err(EmbedError::Provider(format!(
            "expected {expected_len} embeddings, got {}",
            response.embeddings.len()
        )));
    }

    if let Some(expected) = expected_dimensions {
        for emb in &response.embeddings {
            if emb.dimensions != expected {
                return Err(EmbedError::DimensionMismatch {
                    got: emb.dimensions,
                    expected,
                });
            }
        }
    }

    // Embeddings come back with their request index. Sort by index to guarantee
    // input-order in case the adapter doesn't preserve it.
    let mut indexed: Vec<(usize, Vec<f32>)> = response
        .embeddings
        .into_iter()
        .map(|e| (e.index, e.vector))
        .collect();
    indexed.sort_by_key(|(idx, _)| *idx);

    Ok(indexed.into_iter().map(|(_, v)| v).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_targets_ollama_localhost() {
        let cfg = EmbedClientConfig::default();
        assert_eq!(cfg.base_url, "http://localhost:11434/");
        assert_eq!(cfg.model, "nomic-embed-text");
        assert!(cfg.api_key.is_none());
    }

    #[test]
    fn default_base_url_has_trailing_slash() {
        // The Ollama adapter does `format!("{base_url}api/embed")` — without
        // the trailing `/` we'd hit `…11434api/embed`. Guard against regression.
        assert!(DEFAULT_OLLAMA_BASE_URL.ends_with('/'));
    }

    #[test]
    fn build_client_does_not_panic() {
        let _client = build_embed_client(&EmbedClientConfig::default());
    }

    #[test]
    fn extract_model_name_pulls_quoted_name() {
        let msg = r#"model "nomic-embed-text" not found, try pulling it first"#;
        assert_eq!(extract_model_name(msg), Some("nomic-embed-text".to_owned()));
    }

    #[test]
    fn extract_model_name_returns_none_without_quotes() {
        assert_eq!(extract_model_name("model not found"), None);
    }

    /// Live test against a running Ollama. Ignored by default so CI doesn't
    /// require an Ollama instance. Run with `cargo test -p warp --
    /// rag::embed::tests::embed_text_against_live_ollama --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn embed_text_against_live_ollama() {
        let client = build_embed_client(&EmbedClientConfig::default());
        let vec = embed_text(&client, DEFAULT_EMBED_MODEL, "hello world", None)
            .await
            .expect("ollama must be running with nomic-embed-text pulled");
        // nomic-embed-text is 768-dim
        assert_eq!(vec.len(), 768);
        // Verify near-unit length (model documents normalized output)
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.05,
            "expected near-unit-length vector, got norm = {norm}"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn embed_batch_against_live_ollama() {
        let client = build_embed_client(&EmbedClientConfig::default());
        let texts = vec![
            "the quick brown fox".to_owned(),
            "jumps over the lazy dog".to_owned(),
            "rust is a systems language".to_owned(),
        ];
        let vecs = embed_batch(&client, DEFAULT_EMBED_MODEL, texts, Some(768))
            .await
            .expect("ollama must be running");
        assert_eq!(vecs.len(), 3);
        for v in &vecs {
            assert_eq!(v.len(), 768);
        }
    }
}
