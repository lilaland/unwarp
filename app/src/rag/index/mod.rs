//! RAG indexer pipeline infrastructure.
//!
//! Each indexer (vault notes, command blocks, conversations) is a standalone
//! async task that reads source data, redacts it, chunks it, embeds it, and
//! stores the results in the sqlite-vec vector store.
//!
//! `IndexReport` aggregates the outcome of a single indexer run.

pub mod command_blocks;
pub mod conversations;
pub mod vault_notes;

use thiserror::Error;

use super::{embed::EmbedError, store::StoreError};

/// Errors that can occur during any indexer run.
#[derive(Debug, Error)]
pub enum IndexError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("embed: {0}")]
    Embed(#[from] EmbedError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("task panicked: {0}")]
    TaskPanic(String),
}

/// Summary of a completed indexer run.
#[derive(Debug, Default, Clone)]
pub struct IndexReport {
    pub files_indexed: usize,
    pub chunks_total: usize,
    pub files_skipped: usize,
    pub errors: Vec<String>,
}

impl IndexReport {
    pub fn merge(&mut self, other: IndexReport) {
        self.files_indexed += other.files_indexed;
        self.chunks_total += other.chunks_total;
        self.files_skipped += other.files_skipped;
        self.errors.extend(other.errors);
    }

    pub fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }
}

impl std::fmt::Display for IndexReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "indexed {} files ({} chunks), skipped {}",
            self.files_indexed, self.chunks_total, self.files_skipped
        )?;
        if !self.errors.is_empty() {
            write!(f, " ({} errors)", self.errors.len())?;
        }
        Ok(())
    }
}
