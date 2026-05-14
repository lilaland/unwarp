-- Rollback Phase 3 RAG tables.
-- NOTE: SQLite < 3.35 does not support DROP COLUMN; the redacted_output column
-- on blocks is left in place when rolling back this migration.

DROP TABLE IF EXISTS message_chunks;
DROP TABLE IF EXISTS command_block_chunks;
DROP TABLE IF EXISTS vault_note_chunks;
DROP TABLE IF EXISTS vec_messages;
DROP TABLE IF EXISTS vec_command_blocks;
DROP TABLE IF EXISTS vec_vault_notes;
DROP TABLE IF EXISTS unwarp_conversation_blocks;
DROP TABLE IF EXISTS unwarp_messages;
DROP TABLE IF EXISTS unwarp_conversations;
DROP TABLE IF EXISTS seed_commands;
DROP TABLE IF EXISTS vault_files;
