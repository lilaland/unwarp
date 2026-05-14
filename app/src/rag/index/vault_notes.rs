//! VaultNoteIndexer — walks the vault root and indexes all editable markdown
//! files into the vec_vault_notes embedding index.
//!
//! Design (TDD §11.5):
//! - Skips `*.mirror.md` files (auto-generated; indexed separately via the
//!   command-block indexer if desired).
//! - Content-hash based incremental update: files whose SHA-256 hash matches
//!   the stored `vault_files.content_hash` are skipped.
//! - Redacts secrets before chunking (via `DefaultRedactor`).
//! - Embeds chunks in parallel (bounded by the embed semaphore in embed.rs).

use std::path::{Path, PathBuf};

use diesel::{prelude::*, sql_query, sql_types::Text, sqlite::SqliteConnection, Connection, RunQueryDsl};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::rag::{
    chunk::{chunk_text, ChunkConfig},
    embed::{self, EmbedClientConfig},
    index::{IndexError, IndexReport},
    redact::{DefaultRedactor, Redactor},
    store::VectorStore,
};

// ── DB helpers ────────────────────────────────────────────────────────────────

#[derive(QueryableByName)]
struct VaultFileRow {
    #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
    content_hash: Option<String>,
}

fn open_conn(db_path: &Path) -> Result<SqliteConnection, IndexError> {
    let url = db_path
        .to_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "db path not UTF-8"))?;
    SqliteConnection::establish(url)
        .map_err(|e| IndexError::Store(crate::rag::store::StoreError::Connection(e)))
}

fn get_stored_hash(conn: &mut SqliteConnection, path: &str) -> Option<String> {
    sql_query("SELECT content_hash FROM vault_files WHERE path = ?")
        .bind::<Text, _>(path)
        .get_result::<VaultFileRow>(conn)
        .ok()
        .and_then(|r| r.content_hash)
}

fn upsert_vault_file(
    conn: &mut SqliteConnection,
    path: &str,
    hash: &str,
    chunk_count: i32,
) -> Result<(), IndexError> {
    sql_query(
        "INSERT INTO vault_files(path, content_hash, indexed_at, chunk_count) \
         VALUES (?, ?, CURRENT_TIMESTAMP, ?) \
         ON CONFLICT(path) DO UPDATE SET \
           content_hash = excluded.content_hash, \
           indexed_at   = excluded.indexed_at, \
           chunk_count  = excluded.chunk_count",
    )
    .bind::<Text, _>(path)
    .bind::<Text, _>(hash)
    .bind::<diesel::sql_types::Integer, _>(chunk_count)
    .execute(conn)
    .map_err(|e| IndexError::Store(crate::rag::store::StoreError::Query(e)))?;
    Ok(())
}

// ── VaultNoteIndexer ──────────────────────────────────────────────────────────

/// Indexes editable vault markdown files into `vec_vault_notes`.
#[derive(Clone)]
pub struct VaultNoteIndexer {
    pub vault_root: PathBuf,
    pub embed_config: EmbedClientConfig,
    pub db_path: PathBuf,
    pub chunk_config: ChunkConfig,
}

impl VaultNoteIndexer {
    pub fn new(vault_root: PathBuf, db_path: PathBuf, embed_config: EmbedClientConfig) -> Self {
        Self {
            vault_root,
            embed_config,
            db_path,
            chunk_config: ChunkConfig::default(),
        }
    }

    /// Walk the vault root and index any file whose content hash has changed.
    pub async fn run_full_index(&self) -> Result<IndexReport, IndexError> {
        let files = self.collect_indexable_files()?;
        let mut report = IndexReport::default();

        for path in files {
            match self.index_file(&path).await {
                Ok(chunks) => {
                    report.files_indexed += 1;
                    report.chunks_total += chunks;
                }
                Err(IndexError::Io(e))
                    if e.kind() == std::io::ErrorKind::NotFound =>
                {
                    // File disappeared between walk and read — skip silently.
                    report.files_skipped += 1;
                }
                Err(e) => {
                    report.errors.push(format!("{}: {}", path.display(), e));
                }
            }
        }
        Ok(report)
    }

    /// Index a single vault file.  Returns the number of chunks indexed.
    /// Returns `Ok(0)` if the file is up-to-date (hash match).
    pub async fn index_file(&self, path: &Path) -> Result<usize, IndexError> {
        let content = tokio::fs::read_to_string(path).await?;
        let hash = hex_hash(&content);
        let path_str = path.to_string_lossy().into_owned();
        let db_path = self.db_path.clone();
        let path_str_clone = path_str.clone();

        // Check stored hash on a blocking thread.
        let stored_hash = tokio::task::spawn_blocking(move || {
            let mut conn = open_conn(&db_path)?;
            Ok::<_, IndexError>(get_stored_hash(&mut conn, &path_str_clone))
        })
        .await
        .map_err(|e| IndexError::TaskPanic(e.to_string()))??;

        if stored_hash.as_deref() == Some(&hash) {
            return Ok(0); // up-to-date
        }

        // Redact → chunk → embed.
        let redactor = DefaultRedactor::new(&[])
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        let redacted = redactor.redact(&content);
        let chunks = chunk_text(&redacted, &self.chunk_config);

        if chunks.is_empty() {
            // Blank or whitespace-only file — still update the hash.
            let db_path = self.db_path.clone();
            let hash_clone = hash.clone();
            tokio::task::spawn_blocking(move || {
                let mut conn = open_conn(&db_path)?;
                upsert_vault_file(&mut conn, &path_str, &hash_clone, 0)
            })
            .await
            .map_err(|e| IndexError::TaskPanic(e.to_string()))??;
            return Ok(0);
        }

        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let chunk_pairs: Vec<(i32, String)> = chunks
            .iter()
            .map(|c| (c.chunk_idx as i32, c.text.clone()))
            .collect();

        // Embed all chunks (respects the semaphore in embed.rs).
        let client = embed::build_embed_client(&self.embed_config);
        let embeddings =
            embed::embed_batch(&client, &self.embed_config.model, texts, None).await?;

        let chunk_count = embeddings.len();
        let store = VectorStore::new(&self.db_path)?;
        let db_path = self.db_path.clone();
        let hash_clone = hash.clone();
        let path_str_store = path_str.clone();

        tokio::task::spawn_blocking(move || {
            // Clear stale chunks, insert fresh ones, then update the file record.
            store.delete_vault_file_chunks(&path_str_store)?;
            let chunk_refs: Vec<(i32, &str)> = chunk_pairs
                .iter()
                .map(|(idx, text)| (*idx, text.as_str()))
                .collect();
            store.insert_vault_note_chunks(&path_str_store, &chunk_refs, &embeddings)?;

            let mut conn = open_conn(&db_path)?;
            upsert_vault_file(&mut conn, &path_str_store, &hash_clone, chunk_count as i32)?;
            Ok::<_, IndexError>(())
        })
        .await
        .map_err(|e| IndexError::TaskPanic(e.to_string()))??;

        Ok(chunk_count)
    }

    /// Collect all `.md` files in the vault root, excluding `*.mirror.md`.
    fn collect_indexable_files(&self) -> Result<Vec<PathBuf>, IndexError> {
        let mut files = Vec::new();
        for entry in WalkDir::new(&self.vault_root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.ends_with(".md") && !name.ends_with(".mirror.md") {
                files.push(path.to_path_buf());
            }
        }
        Ok(files)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn hex_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}
