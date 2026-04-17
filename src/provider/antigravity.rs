use super::cli_common::{build_cli_prompt, run_cli_text_command};
use super::{EventStream, Provider};
use crate::auth::antigravity as antigravity_auth;
use crate::message::{Message, ToolDefinition};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, RwLock};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

const DEFAULT_MODEL: &str = "default";
const AVAILABLE_MODELS: &[&str] = &[
    "default",
    "claude-opus-4-6-thinking",
    "claude-sonnet-4-6",
    "gemini-3-pro-high",
    "gemini-3-pro-low",
    "gemini-3-flash",
    "gemini-3.1-pro-high",
    "gemini-3.1-pro-low",
    "gemini-3-flash-agent",
    "gpt-oss-120b-medium",
];
const FETCH_MODELS_API_URL: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels";
const VERSION_ENV: &str = "JCODE_ANTIGRAVITY_VERSION";
const ANTIGRAVITY_VERSION: &str = "1.18.3";
const X_GOOG_API_CLIENT: &str = "google-cloud-sdk vscode_cloudshelleditor/0.1";

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PersistedCatalog {
    models: Vec<String>,
    fetched_at_rfc3339: String,
}

#[derive(Debug, Deserialize)]
struct FetchAvailableModelsResponse {
    #[serde(default)]
    models: HashMap<String, FetchAvailableModelEntry>,
}

#[derive(Debug, Deserialize)]
struct FetchAvailableModelEntry {
    #[serde(default)]
    model_name: Option<String>,
}

fn metadata_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "WINDOWS"
    } else {
        "MACOS"
    }
}

fn antigravity_version() -> String {
    std::env::var(VERSION_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ANTIGRAVITY_VERSION.to_string())
}

fn antigravity_user_agent() -> String {
    if cfg!(target_os = "windows") {
        format!("antigravity/{} windows/amd64", antigravity_version())
    } else if cfg!(target_arch = "aarch64") {
        format!("antigravity/{} darwin/arm64", antigravity_version())
    } else {
        format!("antigravity/{} darwin/amd64", antigravity_version())
    }
}

fn client_metadata_header() -> String {
    format!(
        "{{\"ideType\":\"ANTIGRAVITY\",\"platform\":\"{}\",\"pluginType\":\"GEMINI\"}}",
        metadata_platform()
    )
}

fn merge_antigravity_model_lists(models: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut preferred = Vec::new();

    for known in AVAILABLE_MODELS {
        if models.iter().any(|model| model == known) && seen.insert((*known).to_string()) {
            preferred.push((*known).to_string());
        }
    }

    let mut extras: Vec<String> = models
        .into_iter()
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty() && seen.insert(model.clone()))
        .collect();
    extras.sort();
    preferred.extend(extras);
    preferred
}

fn parse_fetch_available_models_response(response: &FetchAvailableModelsResponse) -> Vec<String> {
    let mut discovered = BTreeSet::new();

    for (model_id, entry) in &response.models {
        let trimmed = model_id.trim();
        if !trimmed.is_empty() {
            discovered.insert(trimmed.to_string());
        }
        if let Some(model_name) = entry.model_name.as_deref() {
            let trimmed = model_name.trim();
            if !trimmed.is_empty() {
                discovered.insert(trimmed.to_string());
            }
        }
    }

    merge_antigravity_model_lists(discovered.into_iter().collect())
}

pub struct AntigravityCliProvider {
    cli_path: String,
    client: reqwest::Client,
    model: Arc<RwLock<String>>,
    fetched_models: Arc<RwLock<Vec<String>>>,
    prompt_flag: Option<String>,
    model_flag: Option<String>,
}

impl Clone for AntigravityCliProvider {
    fn clone(&self) -> Self {
        Self {
            cli_path: self.cli_path.clone(),
            client: self.client.clone(),
            model: self.model.clone(),
            fetched_models: self.fetched_models.clone(),
            prompt_flag: self.prompt_flag.clone(),
            model_flag: self.model_flag.clone(),
        }
    }
}

impl AntigravityCliProvider {
    fn persisted_catalog_path() -> Result<std::path::PathBuf> {
        Ok(crate::storage::app_config_dir()?.join("antigravity_models_cache.json"))
    }

    fn load_persisted_catalog() -> Option<PersistedCatalog> {
        let path = Self::persisted_catalog_path().ok()?;
        crate::storage::read_json(&path)
            .ok()
            .filter(|catalog: &PersistedCatalog| !catalog.models.is_empty())
    }

    fn persist_catalog(models: &[String]) {
        if models.is_empty() {
            return;
        }
        let Ok(path) = Self::persisted_catalog_path() else {
            return;
        };
        let payload = PersistedCatalog {
            models: models.to_vec(),
            fetched_at_rfc3339: Utc::now().to_rfc3339(),
        };
        if let Err(error) = crate::storage::write_json(&path, &payload) {
            crate::logging::warn(&format!(
                "Failed to persist Antigravity model catalog {}: {}",
                path.display(),
                error
            ));
        }
    }

    fn seed_cached_catalog(&self) {
        if let Some(catalog) = Self::load_persisted_catalog()
            && let Ok(mut models) = self.fetched_models.write()
        {
            *models = catalog.models;
        }
    }

    pub fn new() -> Self {
        let cli_path = std::env::var("JCODE_ANTIGRAVITY_CLI_PATH")
            .unwrap_or_else(|_| "antigravity".to_string());
        let model =
            std::env::var("JCODE_ANTIGRAVITY_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
        let prompt_flag = std::env::var("JCODE_ANTIGRAVITY_PROMPT_FLAG")
            .ok()
            .or_else(|| Some("-p".to_string()));
        let model_flag = std::env::var("JCODE_ANTIGRAVITY_MODEL_FLAG")
            .ok()
            .or_else(|| Some("--model".to_string()));

        let provider = Self {
            cli_path,
            client: crate::provider::shared_http_client(),
            model: Arc::new(RwLock::new(model)),
            fetched_models: Arc::new(RwLock::new(Vec::new())),
            prompt_flag,
            model_flag,
        };
        provider.seed_cached_catalog();
        provider
    }

    async fn fetch_available_models(&self) -> Result<Vec<String>> {
        let mut tokens = antigravity_auth::load_or_refresh_tokens().await?;
        let project_id = match tokens.project_id.clone() {
            Some(project_id) if !project_id.trim().is_empty() => project_id,
            _ => {
                let project_id = antigravity_auth::fetch_project_id(&tokens.access_token)
                    .await
                    .context("Failed to resolve Antigravity project for model discovery")?;
                tokens.project_id = Some(project_id.clone());
                let _ = antigravity_auth::save_tokens(&tokens);
                project_id
            }
        };

        let response = self
            .client
            .post(FETCH_MODELS_API_URL)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", tokens.access_token),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, antigravity_user_agent())
            .header(
                reqwest::header::HeaderName::from_static("x-goog-api-client"),
                X_GOOG_API_CLIENT,
            )
            .header(
                reqwest::header::HeaderName::from_static("client-metadata"),
                client_metadata_header(),
            )
            .json(&serde_json::json!({ "project": project_id }))
            .send()
            .await
            .context("Failed to fetch Antigravity model catalog")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Antigravity model catalog request failed ({}): {}",
                status,
                body.trim()
            );
        }

        let parsed: FetchAvailableModelsResponse = response
            .json()
            .await
            .context("Failed to decode Antigravity model catalog response")?;
        Ok(parse_fetch_available_models_response(&parsed))
    }
}

impl Default for AntigravityCliProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for AntigravityCliProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let prompt = build_cli_prompt(system, messages);
        let model = self
            .model
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let cli_path = self.cli_path.clone();
        let prompt_flag = self.prompt_flag.clone();
        let model_flag = self.model_flag.clone();
        let cwd = std::env::current_dir().ok();
        let (tx, rx) = mpsc::channel::<Result<crate::message::StreamEvent>>(100);

        tokio::spawn(async move {
            if tx
                .send(Ok(crate::message::StreamEvent::ConnectionType {
                    connection: "cli subprocess".to_string(),
                }))
                .await
                .is_err()
            {
                return;
            }
            let mut cmd = Command::new(&cli_path);
            if let Some(flag) = model_flag.as_ref().filter(|f| !f.trim().is_empty()) {
                cmd.arg(flag).arg(&model);
            }
            if let Some(flag) = prompt_flag.as_ref().filter(|f| !f.trim().is_empty()) {
                cmd.arg(flag).arg(prompt);
            } else {
                cmd.arg(prompt);
            }
            if let Some(dir) = cwd {
                cmd.current_dir(dir);
            }

            if let Err(e) = run_cli_text_command(cmd, tx.clone(), "Antigravity").await {
                let _ = tx.send(Err(e)).await;
            }
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &'static str {
        "antigravity"
    }

    fn model(&self) -> String {
        self.model
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        let trimmed = model.trim();
        if trimmed.is_empty() {
            anyhow::bail!("Antigravity model cannot be empty");
        }
        *self
            .model
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = trimmed.to_string();
        Ok(())
    }

    fn available_models(&self) -> Vec<&'static str> {
        AVAILABLE_MODELS.to_vec()
    }

    fn available_models_display(&self) -> Vec<String> {
        let dynamic = self
            .fetched_models
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        merge_antigravity_model_lists(
            dynamic
                .into_iter()
                .chain(std::iter::once(self.model()))
                .collect(),
        )
    }

    fn available_models_for_switching(&self) -> Vec<String> {
        self.available_models_display()
    }

    fn model_routes(&self) -> Vec<super::ModelRoute> {
        self.available_models_display()
            .into_iter()
            .map(|model| super::ModelRoute {
                model,
                provider: "Antigravity".to_string(),
                api_method: "cli".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            })
            .collect()
    }

    fn on_auth_changed(&self) {
        let provider = self.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if provider.prefetch_models().await.is_ok() {
                    crate::bus::Bus::global().publish(crate::bus::BusEvent::ModelsUpdated);
                }
            });
        }
    }

    async fn prefetch_models(&self) -> Result<()> {
        match self.fetch_available_models().await {
            Ok(models) => {
                if !models.is_empty() {
                    crate::logging::info(&format!(
                        "Discovered Antigravity models: {}",
                        models.join(", ")
                    ));
                    Self::persist_catalog(&models);
                    *self
                        .fetched_models
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = models;
                }
            }
            Err(err) => {
                crate::logging::warn(&format!(
                    "Antigravity model catalog refresh failed; keeping fallback list: {}",
                    err
                ));
            }
        }

        Ok(())
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            cli_path: self.cli_path.clone(),
            client: self.client.clone(),
            model: Arc::new(RwLock::new(self.model())),
            fetched_models: self.fetched_models.clone(),
            prompt_flag: self.prompt_flag.clone(),
            model_flag: self.model_flag.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fetch_available_models_response_discovers_keys_and_model_names() {
        let response: FetchAvailableModelsResponse = serde_json::from_value(serde_json::json!({
            "models": {
                "claude-opus-4-6-thinking": {
                    "modelName": "claude-opus-4-6-thinking"
                },
                "gemini-3-pro-high": {
                    "modelName": "gemini-3-pro-high"
                },
                "gpt-oss-120b-medium": {}
            }
        }))
        .expect("parse response");

        assert_eq!(
            parse_fetch_available_models_response(&response),
            vec![
                "claude-opus-4-6-thinking".to_string(),
                "gemini-3-pro-high".to_string(),
                "gpt-oss-120b-medium".to_string(),
            ]
        );
    }

    #[test]
    fn available_models_display_includes_dynamic_cache_and_current_override() {
        let provider = AntigravityCliProvider::new();
        *provider
            .fetched_models
            .write()
            .expect("fetched models lock") = vec![
            "claude-opus-4-6-thinking".to_string(),
            "gemini-3-pro-high".to_string(),
        ];
        provider
            .set_model("custom-antigravity-model")
            .expect("set custom model");

        let models = provider.available_models_display();

        assert!(models.contains(&"claude-opus-4-6-thinking".to_string()));
        assert!(models.contains(&"gemini-3-pro-high".to_string()));
        assert!(models.contains(&"custom-antigravity-model".to_string()));
    }

    #[test]
    fn available_models_display_seeds_from_persisted_catalog() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::TempDir::new().expect("temp dir");
        let previous = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", temp.path());

        let path = AntigravityCliProvider::persisted_catalog_path().expect("catalog path");
        crate::storage::write_json(
            &path,
            &PersistedCatalog {
                models: vec!["claude-opus-4-6-thinking".to_string()],
                fetched_at_rfc3339: Utc::now().to_rfc3339(),
            },
        )
        .expect("write persisted catalog");

        let provider = AntigravityCliProvider::new();
        assert!(
            provider
                .available_models_display()
                .contains(&"claude-opus-4-6-thinking".to_string())
        );

        if let Some(previous) = previous {
            crate::env::set_var("JCODE_HOME", previous);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }
}
