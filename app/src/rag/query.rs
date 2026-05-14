//! RAG query API: embed a text query, run KNN across one or more indexes.
//!
//! `RagQuery` is the entry point for Phase 3 slash commands and the inline
//! search panel. It handles:
//! 1. Embedding the query string via the configured Ollama endpoint.
//! 2. Dispatching KNN searches to `VectorStore` (on a blocking thread).
//! 3. Merging and ranking results from multiple indexes by distance.

use std::cmp::Ordering;
use std::path::Path;

use thiserror::Error;

use super::{
    embed::{self, EmbedClientConfig, EmbedError},
    store::{CommandBlockHit, MessageHit, StoreError, VaultNoteHit, VectorStore},
};

/// Errors that can occur during a RAG query.
#[derive(Debug, Error)]
pub enum QueryError {
    #[error("embed: {0}")]
    Embed(#[from] EmbedError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("search task panicked: {0}")]
    TaskPanic(String),
}

/// Which embedding index(es) to include in a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    VaultNote,
    CommandBlock,
    Message,
}

/// A single ranked result from any embedding index.
#[derive(Debug, Clone)]
pub struct RagHit {
    /// The raw chunk text (possibly prefixed with a command for CommandBlock).
    pub chunk_text: String,
    /// Identifies the source: vault file path, block_id, or message_id.
    pub source_id: String,
    pub kind: HitKind,
    pub distance: f64,
}

impl RagHit {
    fn from_vault(h: VaultNoteHit) -> Self {
        Self {
            chunk_text: h.chunk_text,
            source_id: h.vault_file_id,
            kind: HitKind::VaultNote,
            distance: h.distance,
        }
    }

    fn from_block(h: CommandBlockHit) -> Self {
        Self {
            chunk_text: h.chunk_text,
            source_id: h.block_id,
            kind: HitKind::CommandBlock,
            distance: h.distance,
        }
    }

    fn from_message(h: MessageHit) -> Self {
        Self {
            chunk_text: h.chunk_text,
            source_id: h.message_id,
            kind: HitKind::Message,
            distance: h.distance,
        }
    }
}

/// Entry point for RAG search. Clone is cheap (String + VectorStore).
#[derive(Clone)]
pub struct RagQuery {
    embed_config: EmbedClientConfig,
    store: VectorStore,
    /// Maximum results returned per index.
    pub limit_per_kind: i64,
}

impl RagQuery {
    /// Construct a query handle for `db_path`.  Shares the global embed semaphore.
    pub fn new(db_path: &Path, embed_config: EmbedClientConfig) -> Result<Self, StoreError> {
        Ok(Self {
            embed_config,
            store: VectorStore::new(db_path)?,
            limit_per_kind: 5,
        })
    }

    /// Embed `query_text` and search the specified indexes.
    ///
    /// Results are sorted by distance ascending (nearest first) across all
    /// selected kinds.  Returns at most `limit_per_kind` hits per kind.
    pub async fn search(
        &self,
        query_text: &str,
        kinds: &[HitKind],
    ) -> Result<Vec<RagHit>, QueryError> {
        let client = embed::build_embed_client(&self.embed_config);
        let embedding =
            embed::embed_text(&client, &self.embed_config.model, query_text, None).await?;

        let mut hits: Vec<RagHit> = Vec::new();

        for &kind in kinds {
            let store = self.store.clone();
            let emb = embedding.clone();
            let limit = self.limit_per_kind;

            let results = match kind {
                HitKind::VaultNote => {
                    tokio::task::spawn_blocking(move || store.knn_vault_notes(&emb, limit))
                        .await
                        .map_err(|e| QueryError::TaskPanic(e.to_string()))??
                        .into_iter()
                        .map(RagHit::from_vault)
                        .collect::<Vec<_>>()
                }
                HitKind::CommandBlock => {
                    tokio::task::spawn_blocking(move || store.knn_command_blocks(&emb, limit))
                        .await
                        .map_err(|e| QueryError::TaskPanic(e.to_string()))??
                        .into_iter()
                        .map(RagHit::from_block)
                        .collect::<Vec<_>>()
                }
                HitKind::Message => {
                    tokio::task::spawn_blocking(move || store.knn_messages(&emb, limit))
                        .await
                        .map_err(|e| QueryError::TaskPanic(e.to_string()))??
                        .into_iter()
                        .map(RagHit::from_message)
                        .collect::<Vec<_>>()
                }
            };
            hits.extend(results);
        }

        hits.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap_or(Ordering::Equal));
        Ok(hits)
    }

    /// Convenience: search all three indexes.
    pub async fn search_all(&self, query_text: &str) -> Result<Vec<RagHit>, QueryError> {
        self.search(
            query_text,
            &[HitKind::VaultNote, HitKind::CommandBlock, HitKind::Message],
        )
        .await
    }
}
