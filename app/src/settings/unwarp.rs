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

/// Default chat model. `gemma3` is bundled with Ollama via `ollama pull gemma3`.
/// Users override via the model picker or by editing `settings.toml`.
pub const DEFAULT_CHAT_MODEL: &str = "gemma3";

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
}
