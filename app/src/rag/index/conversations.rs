//! ConversationIndexer — indexes `unwarp_messages` into `vec_messages`.
//!
//! Only indexes messages whose `id` is not yet in `message_chunks` (incremental).
//! Both user and assistant messages are indexed; system prompts are skipped.

use std::path::Path;

use diesel::{
    prelude::*,
    sql_query,
    sql_types::Text,
    sqlite::SqliteConnection,
    Connection, RunQueryDsl,
};

use crate::rag::{
    chunk::{chunk_text, ChunkConfig},
    embed::{self, EmbedClientConfig},
    index::{IndexError, IndexReport},
    redact::{DefaultRedactor, Redactor},
    store::{StoreError, VectorStore},
};

// ── DB helpers ────────────────────────────────────────────────────────────────

#[derive(QueryableByName)]
struct MessageRow {
    #[diesel(sql_type = Text)]
    id: String,
    #[diesel(sql_type = Text)]
    role: String,
    #[diesel(sql_type = Text)]
    content: String,
}

fn open_conn(db_path: &Path) -> Result<SqliteConnection, IndexError> {
    let url = db_path
        .to_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "db path not UTF-8"))?;
    SqliteConnection::establish(url)
        .map_err(|e| IndexError::Store(StoreError::Connection(e)))
}

// ── ConversationIndexer ───────────────────────────────────────────────────────

/// Indexes user/assistant messages from `unwarp_messages` into `vec_messages`.
#[derive(Clone)]
pub struct ConversationIndexer {
    pub db_path: std::path::PathBuf,
    pub embed_config: EmbedClientConfig,
    pub chunk_config: ChunkConfig,
    pub batch_limit: usize,
}

impl ConversationIndexer {
    pub fn new(db_path: std::path::PathBuf, embed_config: EmbedClientConfig) -> Self {
        Self {
            db_path,
            embed_config,
            chunk_config: ChunkConfig::default(),
            batch_limit: 200,
        }
    }

    /// Index up to `batch_limit` messages not yet in `message_chunks`.
    pub async fn run_incremental_index(&self) -> Result<IndexReport, IndexError> {
        let db_path = self.db_path.clone();
        let batch_limit = self.batch_limit as i64;

        let rows: Vec<MessageRow> = tokio::task::spawn_blocking(move || {
            let mut conn = open_conn(&db_path)?;
            sql_query(
                "SELECT m.id, m.role, m.content FROM unwarp_messages m \
                 WHERE m.role IN ('user', 'assistant') \
                   AND NOT EXISTS ( \
                     SELECT 1 FROM message_chunks mc WHERE mc.message_id = m.id \
                   ) \
                 ORDER BY m.created_at DESC \
                 LIMIT ?",
            )
            .bind::<diesel::sql_types::BigInt, _>(batch_limit)
            .load::<MessageRow>(&mut conn)
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
            let content = redactor.redact(&row.content);
            let chunks = chunk_text(&content, &self.chunk_config);

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
                        report.errors.push(format!("{}: {}", row.id, e));
                        continue;
                    }
                };

            let store = VectorStore::new(&self.db_path)?;
            let mid = row.id.clone();
            let n_chunks = chunk_pairs.len();

            let result = tokio::task::spawn_blocking(move || {
                let chunk_refs: Vec<(i32, &str)> = chunk_pairs
                    .iter()
                    .map(|(i, t)| (*i, t.as_str()))
                    .collect();
                store.insert_message_chunks(&mid, &chunk_refs, &embeddings)
            })
            .await
            .map_err(|e| IndexError::TaskPanic(e.to_string()))?;

            match result {
                Ok(()) => {
                    report.files_indexed += 1;
                    report.chunks_total += n_chunks;
                }
                Err(e) => {
                    report.errors.push(format!("{}: {}", row.id, e));
                }
            }
        }

        Ok(report)
    }
}
