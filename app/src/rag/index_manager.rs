//! `RagIndexManager` — singleton model that connects VaultManager filesystem
//! events to the VaultNoteIndexer so vault file changes are automatically
//! re-indexed without a full scan.
//!
//! On `VaultManagerEvent::FileChanged { path }`:
//!   - Editable `.md` files (not `*.mirror.md`) → re-index via `VaultNoteIndexer::index_file`
//!   - All other paths → ignored (tree-refresh is handled by VaultPanel directly)
//!
//! TDD §7.4: "Trigger: VaultManager::FileChanged event. Re-index only changed files."
//!
//! §10.2: Embedding dimension validation — the indexer is constructed with the
//! dimension configured in `unwarp.rag.vector_dimensions` (default 768). If the
//! embed model changes to one with a different dimension, `DimensionMismatch`
//! errors surface in `IndexReport::errors` with a clear log message telling the
//! user to update the setting and click "Re-index all".

use warpui::{Entity, ModelContext, SingletonEntity};

use settings::Setting;
use warpui::AppContext;

use crate::{
    persistence::database_file_path,
    rag::{
        embed::EmbedClientConfig,
        index::vault_notes::VaultNoteIndexer,
    },
    settings::{UnwarpSettings, DEFAULT_EMBEDDING_DIMENSIONS},
    vault::{
        manager::{VaultManager, VaultManagerEvent, VaultState},
    },
};

pub struct RagIndexManager;

impl RagIndexManager {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let vault = VaultManager::handle(ctx);
        ctx.subscribe_to_model(&vault, |_, event, ctx| {
            match event {
                VaultManagerEvent::FileChanged { path } => {
                    // Only re-index editable markdown files.
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if !name.ends_with(".md") || name.ends_with(".mirror.md") {
                        return;
                    }
                    // Only index if vault is Ready and the path is under the vault root.
                    let vault_root = VaultManager::handle(ctx)
                        .as_ref(ctx)
                        .vault_root()
                        .filter(|r| path.starts_with(r))
                        .map(|r| r.to_path_buf());
                    let Some(vault_root) = vault_root else { return };

                    let path = path.clone();
                    let db_path = database_file_path();
                    let embed_config = EmbedClientConfig::default();
                    let expected_dim = embed_dim_from_settings(ctx);

                    ctx.spawn(
                        async move {
                            let indexer =
                                VaultNoteIndexer::new(vault_root, db_path, embed_config)
                                    .with_expected_dimensions(expected_dim);
                            match indexer.index_file(&path).await {
                                Ok(0) => {} // unchanged / up-to-date
                                Ok(n) => {
                                    log::debug!(
                                        "rag: re-indexed {} ({n} chunks)",
                                        path.display()
                                    );
                                }
                                Err(e) => {
                                    log::warn!("rag: re-index failed for {}: {e}", path.display());
                                }
                            }
                        },
                        |_, _, _| {},
                    );
                }
                VaultManagerEvent::StateChanged { new: VaultState::Ready } => {
                    // Vault just became ready — kick off a full index in the background.
                    let vault_root = VaultManager::handle(ctx)
                        .as_ref(ctx)
                        .vault_root()
                        .map(|r| r.to_path_buf());
                    let Some(vault_root) = vault_root else { return };

                    let db_path = database_file_path();
                    let embed_config = EmbedClientConfig::default();
                    let expected_dim = embed_dim_from_settings(ctx);

                    ctx.spawn(
                        async move {
                            let indexer =
                                VaultNoteIndexer::new(vault_root, db_path, embed_config)
                                    .with_expected_dimensions(expected_dim);
                            match indexer.run_full_index().await {
                                Ok(report) => {
                                    if report.files_indexed > 0 || !report.errors.is_empty() {
                                        log::info!("rag: vault startup index: {report}");
                                    }
                                    // §10.2: surface dimension mismatch with actionable guidance.
                                    let has_mismatch = report
                                        .errors
                                        .iter()
                                        .any(|e| e.contains("dimension mismatch"));
                                    if has_mismatch {
                                        log::error!(
                                            "rag: embedding dimension mismatch detected. \
                                             Your embed model returns a different vector size \
                                             than `unwarp.rag.vector_dimensions` (currently {expected_dim}). \
                                             Update that setting to match your model's dimension, \
                                             then click 'Re-index all' in the vault panel."
                                        );
                                    }
                                }
                                Err(e) => {
                                    log::warn!("rag: vault startup index failed: {e}");
                                }
                            }
                        },
                        |_, _, _| {},
                    );
                }
                _ => {}
            }
        });
        Self
    }
}

/// Read `unwarp.rag.vector_dimensions` from settings, clamped to a valid `usize`.
fn embed_dim_from_settings(ctx: &AppContext) -> usize {
    let raw = *UnwarpSettings::as_ref(ctx).rag_vector_dimensions.value();
    if raw > 0 {
        raw as usize
    } else {
        DEFAULT_EMBEDDING_DIMENSIONS
    }
}

impl Entity for RagIndexManager {
    type Event = ();
}

impl SingletonEntity for RagIndexManager {}
