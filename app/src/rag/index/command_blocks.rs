//! CommandBlockIndexer — indexes the `commands` table into `vec_command_blocks`.
//!
//! Uses the `commands` table (plain-text command field) for v1.  Only commands
//! that are not yet in `command_block_chunks` are indexed (incremental).
//!
//! The block_id stored in command_block_chunks is `cmd:<id>` where `<id>` is
//! the integer primary key from the `commands` table.

use std::path::Path;

use diesel::{
    prelude::*,
    sql_query,
    sql_types::{Integer, Text},
    sqlite::SqliteConnection,
    Connection, RunQueryDsl,
};

use crate::rag::{
    chunk::{chunk_command_block, ChunkConfig},
    embed::{self, EmbedClientConfig},
    index::{IndexError, IndexReport},
    redact::{DefaultRedactor, Redactor},
    store::{StoreError, VectorStore},
};

// ── DB helpers ────────────────────────────────────────────────────────────────

#[derive(QueryableByName)]
struct CommandRow {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Text)]
    command: String,
}

fn open_conn(db_path: &Path) -> Result<SqliteConnection, IndexError> {
    let url = db_path
        .to_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "db path not UTF-8"))?;
    SqliteConnection::establish(url)
        .map_err(|e| IndexError::Store(StoreError::Connection(e)))
}

fn block_id(id: i32) -> String {
    format!("cmd:{id}")
}

// ── CommandBlockIndexer ───────────────────────────────────────────────────────

/// Indexes terminal command history (from `commands` table) into `vec_command_blocks`.
#[derive(Clone)]
pub struct CommandBlockIndexer {
    pub db_path: std::path::PathBuf,
    pub embed_config: EmbedClientConfig,
    pub chunk_config: ChunkConfig,
    /// Index at most this many new commands per run (prevents long cold-start).
    pub batch_limit: usize,
}

impl CommandBlockIndexer {
    pub fn new(db_path: std::path::PathBuf, embed_config: EmbedClientConfig) -> Self {
        Self {
            db_path,
            embed_config,
            chunk_config: ChunkConfig::default(),
            batch_limit: 500,
        }
    }

    /// Index up to `batch_limit` commands that aren't yet in `command_block_chunks`.
    pub async fn run_incremental_index(&self) -> Result<IndexReport, IndexError> {
        let db_path = self.db_path.clone();
        let batch_limit = self.batch_limit as i64;

        let rows: Vec<CommandRow> = tokio::task::spawn_blocking(move || {
            let mut conn = open_conn(&db_path)?;
            sql_query(
                "SELECT c.id, c.command FROM commands c \
                 WHERE c.command != '' \
                   AND NOT EXISTS ( \
                     SELECT 1 FROM command_block_chunks cc \
                     WHERE cc.block_id = 'cmd:' || c.id \
                   ) \
                 ORDER BY c.id DESC \
                 LIMIT ?",
            )
            .bind::<diesel::sql_types::BigInt, _>(batch_limit)
            .load::<CommandRow>(&mut conn)
            .map_err(|e| IndexError::Store(StoreError::Query(e)))
        })
        .await
        .map_err(|e| IndexError::TaskPanic(e.to_string()))??;

        let mut report = IndexReport::default();
        if rows.is_empty() {
            return Ok(report);
        }

        let redactor = DefaultRedactor::new(&[])
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        for row in rows {
            let bid = block_id(row.id);
            let command = redactor.redact(&row.command);
            let chunks = chunk_command_block(&command, "", &self.chunk_config);

            if chunks.is_empty() {
                report.files_skipped += 1;
                continue;
            }

            let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
            let chunk_pairs: Vec<(i32, String)> = chunks
                .iter()
                .map(|c| (c.chunk_idx as i32, c.text.clone()))
                .collect();

            let client = embed::build_embed_client(&self.embed_config);
            let embeddings =
                match embed::embed_batch(&client, &self.embed_config.model, texts, None).await {
                    Ok(e) => e,
                    Err(e) => {
                        report.errors.push(format!("{bid}: {e}"));
                        continue;
                    }
                };

            let store = VectorStore::new(&self.db_path)?;
            let bid_store = bid.clone();
            let n_chunks = chunk_pairs.len();

            let result = tokio::task::spawn_blocking(move || {
                let chunk_refs: Vec<(i32, &str)> = chunk_pairs
                    .iter()
                    .map(|(i, t)| (*i, t.as_str()))
                    .collect();
                store.insert_command_block_chunks(&bid_store, &chunk_refs, &embeddings)
            })
            .await
            .map_err(|e| IndexError::TaskPanic(e.to_string()))?;

            match result {
                Ok(()) => {
                    report.files_indexed += 1;
                    report.chunks_total += n_chunks;
                }
                Err(e) => {
                    report.errors.push(format!("{bid}: {e}"));
                }
            }
        }

        Ok(report)
    }
}
