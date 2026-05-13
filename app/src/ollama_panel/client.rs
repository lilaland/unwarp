//! HTTP client for the Ollama native API.
//!
//! Only the endpoints needed for the monitoring panel are wired:
//! - `GET /api/ps`   — models currently loaded
//! - `GET /api/tags` — models pulled locally
//!
//! Uses `reqwest::Client` directly (not the workspace `http_client::Client`
//! wrapper) because this is a localhost-only API; auth/proxy middleware is
//! out of place. A 5s request timeout guards against a hung Ollama.

use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

const REQUEST_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone)]
pub struct OllamaClient {
    base_url: String,
    http: reqwest::Client,
}

impl OllamaClient {
    pub fn new(base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .expect("reqwest::Client should always build with a timeout");
        Self {
            base_url: ensure_trailing_slash(base_url),
            http,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `GET /api/ps` — models Ollama has currently loaded in memory.
    pub async fn list_running(&self) -> Result<Vec<RunningModel>, OllamaError> {
        let url = format!("{}api/ps", self.base_url);
        let env: ModelsEnvelope<RunningModel> = self.get_json(&url).await?;
        Ok(env.models)
    }

    /// `GET /api/tags` — models pulled locally and available to load.
    pub async fn list_pulled(&self) -> Result<Vec<PulledModel>, OllamaError> {
        let url = format!("{}api/tags", self.base_url);
        let env: ModelsEnvelope<PulledModel> = self.get_json(&url).await?;
        Ok(env.models)
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T, OllamaError> {
        let response = self.http.get(url).send().await.map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                OllamaError::NotReachable(self.base_url.clone())
            } else {
                OllamaError::Http {
                    status: 0,
                    body: e.to_string(),
                }
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(OllamaError::Http {
                status: status.as_u16(),
                body,
            });
        }
        response
            .json::<T>()
            .await
            .map_err(|e| OllamaError::Json(e.to_string()))
    }
}

fn ensure_trailing_slash(s: String) -> String {
    if s.ends_with('/') {
        s
    } else {
        format!("{s}/")
    }
}

#[derive(Debug, Deserialize)]
struct ModelsEnvelope<T> {
    models: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunningModel {
    pub name: String,
    pub model: String,
    pub size: u64,
    #[serde(default)]
    pub size_vram: u64,
    pub expires_at: String,
    pub details: ModelDetails,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PulledModel {
    pub name: String,
    pub model: String,
    pub modified_at: String,
    pub size: u64,
    pub details: ModelDetails,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelDetails {
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub parameter_size: String,
    #[serde(default)]
    pub quantization_level: String,
}

#[derive(Debug, Error, Clone)]
pub enum OllamaError {
    #[error("Ollama not reachable at {0}")]
    NotReachable(String),
    #[error("Ollama returned HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("Invalid JSON response: {0}")]
    Json(String),
}

/// Format a byte count as a human-friendly string ("1.2 GB", "270 MB", "42 KB").
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_trailing_slash_adds_when_missing() {
        assert_eq!(
            ensure_trailing_slash("http://localhost:11434".into()),
            "http://localhost:11434/"
        );
    }

    #[test]
    fn ensure_trailing_slash_preserves_when_present() {
        assert_eq!(
            ensure_trailing_slash("http://localhost:11434/".into()),
            "http://localhost:11434/"
        );
    }

    #[test]
    fn format_bytes_uses_appropriate_unit() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(2048), "2 KB");
        assert_eq!(format_bytes(270 * 1024 * 1024), "270 MB");
        assert_eq!(format_bytes(12 * 1024 * 1024 * 1024), "12.0 GB");
    }

    #[test]
    fn client_normalizes_base_url() {
        let c = OllamaClient::new("http://localhost:11434".into());
        assert_eq!(c.base_url(), "http://localhost:11434/");
    }

    /// Live test against a running Ollama. Ignored by default.
    #[tokio::test]
    #[ignore]
    async fn list_pulled_against_live_ollama() {
        let c = OllamaClient::new("http://localhost:11434/".into());
        let pulled = c.list_pulled().await.expect("ollama must be running");
        assert!(
            !pulled.is_empty(),
            "expected at least one pulled model; run `ollama pull nomic-embed-text`"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn list_running_against_live_ollama() {
        let c = OllamaClient::new("http://localhost:11434/".into());
        // May be empty (no model currently loaded) — just verify the call succeeds.
        let _ = c.list_running().await.expect("ollama must be running");
    }
}
