//! unwarp RAG (Retrieval-Augmented Generation) subsystem.
//!
//! Phase 1 shipped the embedding call site ([`embed`]).
//! Phase 3 adds the full indexer pipeline, vector store, and query API.
//!
//! Public surface:
//! - [`embed`]  — builds the embed client and exposes `embed_text` / `embed_batch`.
//! - [`redact`] — text redaction before indexing (TDD §7.3).
//! - [`chunk`]  — text chunking for embedding (TDD §7.2).

pub mod chunk;
pub mod embed;
pub mod redact;
