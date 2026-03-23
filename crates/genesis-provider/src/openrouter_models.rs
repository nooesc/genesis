//! OpenRouter models API client — fetch available models with metadata.
//!
//! Calls `GET /api/v1/models` to retrieve the full model catalog including
//! pricing, context length, capabilities, and creation timestamps. Results
//! are cached locally with a configurable TTL.

use genesis_config::defaults::timeouts;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

/// Default cache TTL: 1 hour.
const CACHE_TTL: Duration = Duration::from_secs(timeouts::OPENROUTER_MODEL_CACHE_TTL_SECS);

/// OpenRouter API base URL.
const API_BASE: &str = "https://openrouter.ai/api/v1";

/// A model from the OpenRouter catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenRouterModel {
    /// Model ID (e.g. "openai/gpt-4.1").
    pub id: String,
    /// Human-readable name (e.g. "OpenAI: GPT-4.1").
    pub name: String,
    /// Unix timestamp when the model was added.
    #[serde(default)]
    pub created: u64,
    /// Maximum context window in tokens.
    #[serde(default)]
    pub context_length: u64,
    /// Pricing per token.
    #[serde(default)]
    pub pricing: ModelPricing,
    /// Architecture details.
    #[serde(default)]
    pub architecture: ModelArchitecture,
    /// Supported API parameters (e.g. "tools", "reasoning").
    #[serde(default)]
    pub supported_parameters: Vec<String>,
}

impl OpenRouterModel {
    /// Whether this model supports tool/function calling.
    pub fn supports_tools(&self) -> bool {
        self.supported_parameters.iter().any(|p| p == "tools")
    }

    /// Whether this model supports vision/image input.
    pub fn supports_vision(&self) -> bool {
        self.architecture
            .input_modalities
            .iter()
            .any(|m| m == "image")
    }

    /// Whether this model supports extended thinking/reasoning.
    pub fn supports_reasoning(&self) -> bool {
        self.supported_parameters.iter().any(|p| p == "reasoning")
    }

    /// Format pricing for display (per 1M tokens).
    pub fn price_display(&self) -> String {
        let input = self.pricing.prompt_per_million();
        let output = self.pricing.completion_per_million();
        if input == 0.0 && output == 0.0 {
            "free".to_owned()
        } else {
            format!("${:.2}/${:.2}", input, output)
        }
    }

    /// Format context length for display.
    pub fn context_display(&self) -> String {
        if self.context_length >= 1_000_000 {
            format!("{}M", self.context_length / 1_000_000)
        } else if self.context_length >= 1_000 {
            format!("{}k", self.context_length / 1_000)
        } else {
            self.context_length.to_string()
        }
    }
}

/// Token pricing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelPricing {
    /// Price per input token as a string (e.g. "0.0000002").
    #[serde(default)]
    pub prompt: String,
    /// Price per output token as a string.
    #[serde(default)]
    pub completion: String,
}

impl ModelPricing {
    /// Input price per 1M tokens.
    pub fn prompt_per_million(&self) -> f64 {
        self.prompt.parse::<f64>().unwrap_or(0.0) * 1_000_000.0
    }

    /// Output price per 1M tokens.
    pub fn completion_per_million(&self) -> f64 {
        self.completion.parse::<f64>().unwrap_or(0.0) * 1_000_000.0
    }
}

/// Model architecture metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelArchitecture {
    /// Input modalities (e.g. ["text", "image"]).
    #[serde(default)]
    pub input_modalities: Vec<String>,
    /// Output modalities (e.g. ["text"]).
    #[serde(default)]
    pub output_modalities: Vec<String>,
}

/// API response wrapper.
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<OpenRouterModel>,
}

/// Fetch the model catalog from OpenRouter, using a local cache when fresh.
///
/// - `api_key`: Optional OpenRouter API key (the models endpoint is public)
/// - `cache_dir`: Directory for the cache file (e.g. `~/.genesis/cache/`)
pub async fn fetch_models(
    api_key: Option<&str>,
    cache_dir: &std::path::Path,
) -> Result<Vec<OpenRouterModel>, FetchError> {
    let cache_path = cache_dir.join("openrouter-models.json");

    // Try reading from cache.
    if let Some(models) = read_cache(&cache_path) {
        return Ok(models);
    }

    // Fetch from API.
    let url = format!("{API_BASE}/models");
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(key) = api_key {
        req = req.header("Authorization", format!("Bearer {key}"));
    }

    let response = req
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(
            genesis_config::defaults::limits::OPENROUTER_FETCH_TIMEOUT_SECS,
        ))
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    if !response.status().is_success() {
        return Err(FetchError::Api {
            status: response.status().as_u16(),
            body: response.text().await.unwrap_or_default(),
        });
    }

    let body = response
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let parsed: ModelsResponse =
        serde_json::from_str(&body).map_err(|e| FetchError::Parse(e.to_string()))?;

    // Write to cache.
    let _ = write_cache(&cache_path, &body);

    Ok(parsed.data)
}

/// Read models from cache if the file exists and is fresh.
fn read_cache(path: &std::path::Path) -> Option<Vec<OpenRouterModel>> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;

    if age > CACHE_TTL {
        return None;
    }

    let content = std::fs::read_to_string(path).ok()?;
    let parsed: ModelsResponse = serde_json::from_str(&content).ok()?;
    Some(parsed.data)
}

/// Write the raw API response to the cache file.
fn write_cache(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// Errors from model fetching.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("network error: {0}")]
    Network(String),
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },
    #[error("parse error: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_supports_tools_from_parameters() {
        let model = OpenRouterModel {
            id: "test/model".to_owned(),
            name: "Test".to_owned(),
            created: 0,
            context_length: 128000,
            pricing: ModelPricing::default(),
            architecture: ModelArchitecture::default(),
            supported_parameters: vec!["tools".to_owned(), "reasoning".to_owned()],
        };
        assert!(model.supports_tools());
        assert!(model.supports_reasoning());
        assert!(!model.supports_vision());
    }

    #[test]
    fn model_supports_vision_from_architecture() {
        let model = OpenRouterModel {
            id: "test/model".to_owned(),
            name: "Test".to_owned(),
            created: 0,
            context_length: 128000,
            pricing: ModelPricing::default(),
            architecture: ModelArchitecture {
                input_modalities: vec!["text".to_owned(), "image".to_owned()],
                output_modalities: vec!["text".to_owned()],
            },
            supported_parameters: vec![],
        };
        assert!(model.supports_vision());
    }

    #[test]
    fn price_display_formatting() {
        let model = OpenRouterModel {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            created: 0,
            context_length: 0,
            pricing: ModelPricing {
                prompt: "0.000003".to_owned(),
                completion: "0.000015".to_owned(),
            },
            architecture: ModelArchitecture::default(),
            supported_parameters: vec![],
        };
        assert_eq!(model.price_display(), "$3.00/$15.00");
    }

    #[test]
    fn context_display_formatting() {
        let mut model = OpenRouterModel {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            created: 0,
            context_length: 128000,
            pricing: ModelPricing::default(),
            architecture: ModelArchitecture::default(),
            supported_parameters: vec![],
        };
        assert_eq!(model.context_display(), "128k");

        model.context_length = 1_000_000;
        assert_eq!(model.context_display(), "1M");
    }

    #[test]
    fn free_model_displays_free() {
        let model = OpenRouterModel {
            id: "test".to_owned(),
            name: "Free Model".to_owned(),
            created: 0,
            context_length: 0,
            pricing: ModelPricing {
                prompt: "0".to_owned(),
                completion: "0".to_owned(),
            },
            architecture: ModelArchitecture::default(),
            supported_parameters: vec![],
        };
        assert_eq!(model.price_display(), "free");
    }
}
