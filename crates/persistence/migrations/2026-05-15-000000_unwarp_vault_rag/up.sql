-- Phase 3: unwarp vault + RAG persistence
-- Adds embedding infrastructure, vault file tracker, conversation tables,
-- and seed command corpus.  sqlite-vec must be loaded before this migration
-- runs so that the vec0 virtual tables can be created.

-- Extend blocks with a pre-redacted copy of the output for embedding.
ALTER TABLE blocks ADD COLUMN redacted_output TEXT;

-- ── Vault file index tracker ─────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS vault_files (
  path         TEXT PRIMARY KEY NOT NULL,
  content_hash TEXT,
  indexed_at   TIMESTAMP,
  chunk_count  INTEGER NOT NULL DEFAULT 0
);

-- ── Seed commands corpus (/sugg) ─────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS seed_commands (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  command     TEXT NOT NULL,
  description TEXT,
  tags        TEXT,           -- JSON array of strings
  source      TEXT NOT NULL DEFAULT 'builtin'
);

-- ── RAG conversation tables ───────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS unwarp_conversations (
  id           TEXT PRIMARY KEY NOT NULL,   -- UUID v4
  created_at   TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at   TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  title        TEXT,
  context_path TEXT
);

CREATE TABLE IF NOT EXISTS unwarp_messages (
  id              TEXT PRIMARY KEY NOT NULL,  -- UUID v4
  conversation_id TEXT NOT NULL REFERENCES unwarp_conversations(id) ON DELETE CASCADE,
  role            TEXT NOT NULL CHECK(role IN ('user', 'assistant', 'system')),
  content         TEXT NOT NULL,
  created_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_unwarp_messages_conversation
  ON unwarp_messages(conversation_id, created_at);

CREATE TABLE IF NOT EXISTS unwarp_conversation_blocks (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id TEXT NOT NULL REFERENCES unwarp_conversations(id) ON DELETE CASCADE,
  block_id        TEXT NOT NULL,
  added_at        TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- ── Vector embedding virtual tables (sqlite-vec vec0) ────────────────────────
-- Each rowid here is the shared key to the companion metadata table below.

CREATE VIRTUAL TABLE IF NOT EXISTS vec_vault_notes USING vec0(
  embedding float[768]
);

CREATE VIRTUAL TABLE IF NOT EXISTS vec_command_blocks USING vec0(
  embedding float[768]
);

CREATE VIRTUAL TABLE IF NOT EXISTS vec_messages USING vec0(
  embedding float[768]
);

-- ── Embedding metadata tables ────────────────────────────────────────────────
-- rowid is shared with the companion vec0 virtual table.

CREATE TABLE IF NOT EXISTS vault_note_chunks (
  rowid         INTEGER PRIMARY KEY,
  vault_file_id TEXT NOT NULL REFERENCES vault_files(path) ON DELETE CASCADE,
  chunk_idx     INTEGER NOT NULL,
  chunk_text    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_vault_note_chunks_file
  ON vault_note_chunks(vault_file_id);

CREATE TABLE IF NOT EXISTS command_block_chunks (
  rowid      INTEGER PRIMARY KEY,
  block_id   TEXT NOT NULL,
  chunk_idx  INTEGER NOT NULL,
  chunk_text TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_command_block_chunks_block
  ON command_block_chunks(block_id);

CREATE TABLE IF NOT EXISTS message_chunks (
  rowid      INTEGER PRIMARY KEY,
  message_id TEXT NOT NULL REFERENCES unwarp_messages(id) ON DELETE CASCADE,
  chunk_idx  INTEGER NOT NULL,
  chunk_text TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_message_chunks_message
  ON message_chunks(message_id);
