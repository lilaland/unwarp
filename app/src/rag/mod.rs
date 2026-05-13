//! unwarp RAG (Retrieval-Augmented Generation) subsystem.
//!
//! Phase 1 scope: the embedding call site only (see TDD §7.1).
//! Phase 3 will add: indexers, vector store (sqlite-vec), redaction, query API.
//!
//! Public surface:
//! - [`embed`] — builds the embed client and exposes `embed_text` / `embed_batch`.

pub mod embed;
