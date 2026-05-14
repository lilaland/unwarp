//! unwarp-specific settings.
//!
//! These live under the `[unwarp.*]` TOML namespace, separate from the
//! inherited Warp settings tree (`appearance.*`, `agents.*`, etc.), so:
//! - schema additions don't collide with upstream Warp changes during sync
//! - users can `grep '\[unwarp\.'` in `settings.toml` to see what we added
//!
//! Phase 1 covers `[unwarp.llm]` only (embed wiring, TDD §6 / §7.1).
//! Phase 2/3 will add `[unwarp.vault]`, `[unwarp.rag]`, `[unwarp.redaction]`,
//! and `[unwarp.claude_code]`.

use settings::{
    macros::define_settings_group, Setting, SupportedPlatforms, SyncToCloud,
};
use warpui::{AppContext, SingletonEntity};

use crate::rag::embed::{DEFAULT_EMBED_MODEL, DEFAULT_OLLAMA_BASE_URL};
use crate::vault::config::{
    DEFAULT_MIRROR_MAX_DEPTH, DEFAULT_MIRROR_SOURCE_REL_PATH, DEFAULT_VAULT_REL_PATH,
};

/// Default chat model. `gemma3` is bundled with Ollama via `ollama pull gemma3`.
/// Users override via the model picker or by editing `settings.toml`.
pub const DEFAULT_CHAT_MODEL: &str = "gemma3";

/// Default embedding vector dimension. Matches `nomic-embed-text` (768-dim).
/// Update `unwarp.rag.vector_dimensions` in settings.toml when changing models
/// (e.g., `mxbai-embed-large` is 1024-dim), then run "Re-index all".
pub const DEFAULT_EMBEDDING_DIMENSIONS: usize = 768;

/// Default vault path setting value. We store it as `~/...` so the user
/// editing `settings.toml` sees a portable string, then expand at load time.
fn default_vault_path() -> String {
    format!("~/{DEFAULT_VAULT_REL_PATH}")
}

fn default_mirror_source_root() -> String {
    format!("~/{DEFAULT_MIRROR_SOURCE_REL_PATH}")
}

define_settings_group!(UnwarpSettings,
    settings: [
        default_chat_model: DefaultChatModel {
            type: String,
            default: DEFAULT_CHAT_MODEL.to_string(),
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Never,
            private: false,
            toml_path: "unwarp.llm.default_chat_model",
            description: "Default chat model name passed to the embed provider (Ollama by default).",
        },
        default_embed_model: DefaultEmbedModel {
            type: String,
            default: DEFAULT_EMBED_MODEL.to_string(),
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Never,
            private: false,
            toml_path: "unwarp.llm.default_embed_model",
            description: "Embedding model used by the RAG indexers and slash commands.",
        },
        embed_base_url: EmbedBaseUrl {
            type: String,
            default: DEFAULT_OLLAMA_BASE_URL.to_string(),
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Never,
            private: false,
            toml_path: "unwarp.llm.embed_base_url",
            description: "Base URL of the embedding provider. Must end with `/`. Default is local Ollama.",
        },
        vault_path: UnwarpVaultPath {
            type: String,
            default: default_vault_path(),
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Never,
            private: false,
            toml_path: "unwarp.vault.path",
            description: "Absolute path to the unwarp vault directory. `~` is expanded against the home directory.",
        },
        mirror_source_root: UnwarpMirrorSourceRoot {
            type: String,
            default: default_mirror_source_root(),
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Never,
            private: false,
            toml_path: "unwarp.vault.mirror_source_root",
            description: "Directory the mirror job scans for project README/AGENTS/CLAUDE files.",
        },
        mirror_max_depth: UnwarpMirrorMaxDepth {
            type: i64,
            default: DEFAULT_MIRROR_MAX_DEPTH as i64,
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Never,
            private: false,
            toml_path: "unwarp.vault.mirror_max_depth",
            description: "Maximum directory depth (from mirror_source_root) the mirror job recurses to.",
        },
        rag_vector_dimensions: RagVectorDimensions {
            type: i64,
            default: DEFAULT_EMBEDDING_DIMENSIONS as i64,
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Never,
            private: false,
            toml_path: "unwarp.rag.vector_dimensions",
            description: "Expected embedding vector dimension. Must match your embed model (768 for nomic-embed-text, 1024 for mxbai-embed-large). Change this and run 'Re-index all' when switching embed models.",
        },
    ]
);

impl UnwarpSettings {
    /// Build an [`EmbedClientConfig`](crate::rag::embed::EmbedClientConfig) from
    /// the current setting values. Reads through the `AppContext`-backed setting
    /// handles, so the returned config reflects any user overrides in
    /// `settings.toml`.
    pub fn embed_client_config(ctx: &AppContext) -> crate::rag::embed::EmbedClientConfig {
        let settings = Self::as_ref(ctx);
        crate::rag::embed::EmbedClientConfig {
            base_url: settings.embed_base_url.value().to_owned(),
            model: settings.default_embed_model.value().to_owned(),
            api_key: None,
        }
    }

    /// Build a [`VaultConfig`](crate::vault::VaultConfig) from the current
    /// setting values. Returns `Err` only if a user-supplied path is
    /// malformed; default settings always resolve.
    ///
    /// `home_dir` is passed in (rather than fetched from `dirs::home_dir()`)
    /// so callers can override it for tests or sandboxed environments.
    pub fn vault_config(
        ctx: &AppContext,
        home_dir: &std::path::Path,
    ) -> Result<crate::vault::VaultConfig, crate::vault::VaultConfigError> {
        let settings = Self::as_ref(ctx);
        let depth_raw = *settings.mirror_max_depth.value();
        // Clamp to u8 range; settings store as i64 because the macro doesn't
        // support u8 directly. Negative or huge values fall back to default.
        let depth = if (0..=255).contains(&depth_raw) {
            depth_raw as u8
        } else {
            DEFAULT_MIRROR_MAX_DEPTH
        };
        crate::vault::VaultConfig::from_raw(
            home_dir,
            settings.vault_path.value(),
            settings.mirror_source_root.value(),
            depth,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default `embed_base_url` must end with a trailing slash — the genai
    /// Ollama adapter does `format!("{base_url}api/embed")` via plain string
    /// concatenation, and a missing slash produces `…11434api/embed` (404).
    #[test]
    fn default_embed_base_url_ends_with_slash() {
        assert!(DEFAULT_OLLAMA_BASE_URL.ends_with('/'));
    }

    #[test]
    fn default_models_are_ollama_names() {
        // Sanity check: defaults should be model names that `ollama pull` recognizes.
        assert_eq!(DEFAULT_CHAT_MODEL, "gemma3");
        assert_eq!(DEFAULT_EMBED_MODEL, "nomic-embed-text");
    }

    #[test]
    fn default_vault_path_uses_tilde() {
        // Stored as `~/Documents/...` so the user reading settings.toml sees a
        // portable string. Expansion happens in vault::config::from_raw.
        assert!(default_vault_path().starts_with('~'));
        assert!(default_mirror_source_root().starts_with('~'));
    }
}
