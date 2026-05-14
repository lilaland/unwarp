//! Vector store: raw SQL against sqlite-vec vec0 virtual tables.
//!
//! `VectorStore` is a stateless handle (just the DB path) that opens a fresh
//! connection per operation.  It is `Clone + Send + Sync`, so callers can
//! clone it into `spawn_blocking` closures freely.
//!
//! Design notes:
//! - The companion metadata tables (vault_note_chunks, command_block_chunks,
//!   message_chunks) live in the same SQLite file as the rest of the app.
//! - vec0 virtual tables are NOT Diesel-managed; all queries are raw SQL via
//!   `diesel::sql_query` + parameter binding.
//! - `sqlite3_auto_extension` is registered by `init_db()` at app start; we
//!   also register here via a OnceLock so tests / standalone callers work.

use std::path::Path;
use std::sync::OnceLock;

use diesel::{
    prelude::*,
    sql_query,
    sql_types::{BigInt, Binary, Integer, Text},
    sqlite::SqliteConnection,
    Connection, RunQueryDsl,
};
use libsqlite3_sys as sqlite3;
use thiserror::Error;

// ── sqlite-vec registration ───────────────────────────────────────────────────

static VEC_LOADED: OnceLock<()> = OnceLock::new();

fn ensure_vec_loaded() {
    VEC_LOADED.get_or_init(|| unsafe {
        sqlite3::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

/// Register the sqlite-vec extension as an auto-extension for all subsequent
/// SQLite connections in this process. Must be called before any connection
/// that creates or queries vec0 virtual tables. Idempotent.
pub fn init_sqlite_vec() {
    ensure_vec_loaded();
}

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("store connection: {0}")]
    Connection(#[from] diesel::result::ConnectionError),
    #[error("store query: {0}")]
    Query(#[from] diesel::result::Error),
    #[error("store task panicked: {0}")]
    TaskPanic(String),
}

// ── Internal query result types ───────────────────────────────────────────────

#[derive(QueryableByName)]
struct RowId {
    #[diesel(sql_type = BigInt)]
    rowid: i64,
}

// ── Public hit types ──────────────────────────────────────────────────────────

#[derive(QueryableByName, Debug, Clone)]
pub struct VaultNoteHit {
    #[diesel(sql_type = BigInt)]
    pub rowid: i64,
    #[diesel(sql_type = diesel::sql_types::Double)]
    pub distance: f64,
    #[diesel(sql_type = Text)]
    pub vault_file_id: String,
    #[diesel(sql_type = Integer)]
    pub chunk_idx: i32,
    #[diesel(sql_type = Text)]
    pub chunk_text: String,
}

#[derive(QueryableByName, Debug, Clone)]
pub struct CommandBlockHit {
    #[diesel(sql_type = BigInt)]
    pub rowid: i64,
    #[diesel(sql_type = diesel::sql_types::Double)]
    pub distance: f64,
    #[diesel(sql_type = Text)]
    pub block_id: String,
    #[diesel(sql_type = Integer)]
    pub chunk_idx: i32,
    #[diesel(sql_type = Text)]
    pub chunk_text: String,
}

#[derive(QueryableByName, Debug, Clone)]
pub struct MessageHit {
    #[diesel(sql_type = BigInt)]
    pub rowid: i64,
    #[diesel(sql_type = diesel::sql_types::Double)]
    pub distance: f64,
    #[diesel(sql_type = Text)]
    pub message_id: String,
    #[diesel(sql_type = Integer)]
    pub chunk_idx: i32,
    #[diesel(sql_type = Text)]
    pub chunk_text: String,
}

// ── VectorStore ───────────────────────────────────────────────────────────────

/// Stateless handle to the vector store. Clone is cheap (String clone).
#[derive(Clone)]
pub struct VectorStore {
    db_url: String,
}

impl VectorStore {
    pub fn new(db_path: &Path) -> Result<Self, StoreError> {
        let url = db_path
            .to_str()
            .ok_or_else(|| {
                diesel::result::ConnectionError::InvalidConnectionUrl(
                    "db path is not valid UTF-8".into(),
                )
            })?
            .to_owned();
        Ok(Self { db_url: url })
    }

    fn connect(&self) -> Result<SqliteConnection, StoreError> {
        ensure_vec_loaded();
        Ok(SqliteConnection::establish(&self.db_url)?)
    }

    // ── Vault note chunks ─────────────────────────────────────────────────

    /// Remove all chunks for `vault_file_id` from both the vec0 and metadata tables.
    /// Call before re-indexing a file.
    pub fn delete_vault_file_chunks(&self, vault_file_id: &str) -> Result<(), StoreError> {
        let mut conn = self.connect()?;
        conn.transaction::<_, diesel::result::Error, _>(|c| {
            // Fetch rowids first, then delete from vec0 individually.
            let rowids: Vec<RowId> =
                sql_query("SELECT rowid FROM vault_note_chunks WHERE vault_file_id = ?")
                    .bind::<Text, _>(vault_file_id)
                    .load(c)?;
            for r in &rowids {
                sql_query("DELETE FROM vec_vault_notes WHERE rowid = ?")
                    .bind::<BigInt, _>(r.rowid)
                    .execute(c)?;
            }
            sql_query("DELETE FROM vault_note_chunks WHERE vault_file_id = ?")
                .bind::<Text, _>(vault_file_id)
                .execute(c)?;
            Ok(())
        })
        .map_err(StoreError::Query)
    }

    /// Insert a batch of chunks + embeddings for `vault_file_id`.
    /// `chunks` and `embeddings` must have the same length.
    pub fn insert_vault_note_chunks(
        &self,
        vault_file_id: &str,
        chunks: &[(i32, &str)],
        embeddings: &[Vec<f32>],
    ) -> Result<(), StoreError> {
        assert_eq!(chunks.len(), embeddings.len());
        let mut conn = self.connect()?;
        conn.transaction::<_, diesel::result::Error, _>(|c| {
            for ((chunk_idx, chunk_text), embedding) in chunks.iter().zip(embeddings) {
                sql_query(
                    "INSERT INTO vault_note_chunks(vault_file_id, chunk_idx, chunk_text) \
                     VALUES (?, ?, ?)",
                )
                .bind::<Text, _>(vault_file_id)
                .bind::<Integer, _>(*chunk_idx)
                .bind::<Text, _>(*chunk_text)
                .execute(c)?;

                let last: RowId =
                    sql_query("SELECT last_insert_rowid() AS rowid").get_result(c)?;

                sql_query("INSERT INTO vec_vault_notes(rowid, embedding) VALUES (?, ?)")
                    .bind::<BigInt, _>(last.rowid)
                    .bind::<Binary, _>(embedding_to_bytes(embedding))
                    .execute(c)?;
            }
            Ok(())
        })
        .map_err(StoreError::Query)
    }

    /// KNN search across vault note embeddings.
    pub fn knn_vault_notes(
        &self,
        query_embedding: &[f32],
        limit: i64,
    ) -> Result<Vec<VaultNoteHit>, StoreError> {
        let mut conn = self.connect()?;
        let sql = format!(
            "SELECT v.rowid, v.distance, m.vault_file_id, m.chunk_idx, m.chunk_text \
             FROM vec_vault_notes v \
             JOIN vault_note_chunks m ON m.rowid = v.rowid \
             WHERE v.embedding MATCH ? AND v.k = {} \
             ORDER BY v.distance",
            limit
        );
        sql_query(&sql)
            .bind::<Binary, _>(embedding_to_bytes(query_embedding))
            .load(&mut conn)
            .map_err(StoreError::Query)
    }

    // ── Command block chunks ──────────────────────────────────────────────

    pub fn delete_block_chunks(&self, block_id: &str) -> Result<(), StoreError> {
        let mut conn = self.connect()?;
        conn.transaction::<_, diesel::result::Error, _>(|c| {
            let rowids: Vec<RowId> =
                sql_query("SELECT rowid FROM command_block_chunks WHERE block_id = ?")
                    .bind::<Text, _>(block_id)
                    .load(c)?;
            for r in &rowids {
                sql_query("DELETE FROM vec_command_blocks WHERE rowid = ?")
                    .bind::<BigInt, _>(r.rowid)
                    .execute(c)?;
            }
            sql_query("DELETE FROM command_block_chunks WHERE block_id = ?")
                .bind::<Text, _>(block_id)
                .execute(c)?;
            Ok(())
        })
        .map_err(StoreError::Query)
    }

    pub fn insert_command_block_chunks(
        &self,
        block_id: &str,
        chunks: &[(i32, &str)],
        embeddings: &[Vec<f32>],
    ) -> Result<(), StoreError> {
        assert_eq!(chunks.len(), embeddings.len());
        let mut conn = self.connect()?;
        conn.transaction::<_, diesel::result::Error, _>(|c| {
            for ((chunk_idx, chunk_text), embedding) in chunks.iter().zip(embeddings) {
                sql_query(
                    "INSERT INTO command_block_chunks(block_id, chunk_idx, chunk_text) \
                     VALUES (?, ?, ?)",
                )
                .bind::<Text, _>(block_id)
                .bind::<Integer, _>(*chunk_idx)
                .bind::<Text, _>(*chunk_text)
                .execute(c)?;

                let last: RowId =
                    sql_query("SELECT last_insert_rowid() AS rowid").get_result(c)?;

                sql_query("INSERT INTO vec_command_blocks(rowid, embedding) VALUES (?, ?)")
                    .bind::<BigInt, _>(last.rowid)
                    .bind::<Binary, _>(embedding_to_bytes(embedding))
                    .execute(c)?;
            }
            Ok(())
        })
        .map_err(StoreError::Query)
    }

    pub fn knn_command_blocks(
        &self,
        query_embedding: &[f32],
        limit: i64,
    ) -> Result<Vec<CommandBlockHit>, StoreError> {
        let mut conn = self.connect()?;
        let sql = format!(
            "SELECT v.rowid, v.distance, m.block_id, m.chunk_idx, m.chunk_text \
             FROM vec_command_blocks v \
             JOIN command_block_chunks m ON m.rowid = v.rowid \
             WHERE v.embedding MATCH ? AND v.k = {} \
             ORDER BY v.distance",
            limit
        );
        sql_query(&sql)
            .bind::<Binary, _>(embedding_to_bytes(query_embedding))
            .load(&mut conn)
            .map_err(StoreError::Query)
    }

    // ── Message chunks ────────────────────────────────────────────────────

    pub fn delete_message_chunks(&self, message_id: &str) -> Result<(), StoreError> {
        let mut conn = self.connect()?;
        conn.transaction::<_, diesel::result::Error, _>(|c| {
            let rowids: Vec<RowId> =
                sql_query("SELECT rowid FROM message_chunks WHERE message_id = ?")
                    .bind::<Text, _>(message_id)
                    .load(c)?;
            for r in &rowids {
                sql_query("DELETE FROM vec_messages WHERE rowid = ?")
                    .bind::<BigInt, _>(r.rowid)
                    .execute(c)?;
            }
            sql_query("DELETE FROM message_chunks WHERE message_id = ?")
                .bind::<Text, _>(message_id)
                .execute(c)?;
            Ok(())
        })
        .map_err(StoreError::Query)
    }

    pub fn insert_message_chunks(
        &self,
        message_id: &str,
        chunks: &[(i32, &str)],
        embeddings: &[Vec<f32>],
    ) -> Result<(), StoreError> {
        assert_eq!(chunks.len(), embeddings.len());
        let mut conn = self.connect()?;
        conn.transaction::<_, diesel::result::Error, _>(|c| {
            for ((chunk_idx, chunk_text), embedding) in chunks.iter().zip(embeddings) {
                sql_query(
                    "INSERT INTO message_chunks(message_id, chunk_idx, chunk_text) \
                     VALUES (?, ?, ?)",
                )
                .bind::<Text, _>(message_id)
                .bind::<Integer, _>(*chunk_idx)
                .bind::<Text, _>(*chunk_text)
                .execute(c)?;

                let last: RowId =
                    sql_query("SELECT last_insert_rowid() AS rowid").get_result(c)?;

                sql_query("INSERT INTO vec_messages(rowid, embedding) VALUES (?, ?)")
                    .bind::<BigInt, _>(last.rowid)
                    .bind::<Binary, _>(embedding_to_bytes(embedding))
                    .execute(c)?;
            }
            Ok(())
        })
        .map_err(StoreError::Query)
    }

    pub fn knn_messages(
        &self,
        query_embedding: &[f32],
        limit: i64,
    ) -> Result<Vec<MessageHit>, StoreError> {
        let mut conn = self.connect()?;
        let sql = format!(
            "SELECT v.rowid, v.distance, m.message_id, m.chunk_idx, m.chunk_text \
             FROM vec_messages v \
             JOIN message_chunks m ON m.rowid = v.rowid \
             WHERE v.embedding MATCH ? AND v.k = {} \
             ORDER BY v.distance",
            limit
        );
        sql_query(&sql)
            .bind::<Binary, _>(embedding_to_bytes(query_embedding))
            .load(&mut conn)
            .map_err(StoreError::Query)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Serialize a float vector as raw little-endian bytes (sqlite-vec blob format).
pub fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
}
