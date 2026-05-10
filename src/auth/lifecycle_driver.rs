use anyhow::{Context, ensure};
use serde::Deserialize;

use crate::auth::lifecycle::{
    AuthActivationRequest, AuthActivationResult, AuthCatalogInvariantReport, activate_auth_change,
    validate_catalog_invariants,
};
use crate::auth::test_sandbox::AuthTestSandbox;
use crate::protocol::{
    AuthChanged, AuthCredentialSource, AuthMethod, CatalogNamespace, RuntimeProviderKey,
};
use crate::provider::ModelRoute;
use crate::provider_catalog::OpenAiCompatibleProfile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthLifecycleAuthPath {
    TuiPasteApiKey,
    RemoteTuiPasteApiKey,
    EnvFilePreseeded,
    ProcessEnvPreseeded,
}

impl AuthLifecycleAuthPath {
    fn auth_method(self) -> AuthMethod {
        match self {
            Self::TuiPasteApiKey => AuthMethod::TuiPasteApiKey,
            Self::RemoteTuiPasteApiKey => AuthMethod::RemoteTuiPasteApiKey,
            Self::EnvFilePreseeded => AuthMethod::EnvFilePreseeded,
            Self::ProcessEnvPreseeded => AuthMethod::ProcessEnvPreseeded,
        }
    }

    fn credential_source(self) -> AuthCredentialSource {
        match self {
            Self::TuiPasteApiKey | Self::RemoteTuiPasteApiKey | Self::EnvFilePreseeded => {
                AuthCredentialSource::ApiKeyFile
            }
            Self::ProcessEnvPreseeded => AuthCredentialSource::ProcessEnv,
        }
    }

    fn shows_paste_prompt(self) -> bool {
        matches!(self, Self::TuiPasteApiKey | Self::RemoteTuiPasteApiKey)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AuthLifecycleSpec {
    pub provider_id: &'static str,
    pub provider_label: &'static str,
    pub profile: OpenAiCompatibleProfile,
    pub auth_path: AuthLifecycleAuthPath,
    pub api_key: String,
    pub catalog_models_after_auth: Vec<String>,
    pub selected_model_override: Option<String>,
    pub current_runtime_provider_name: &'static str,
}

impl AuthLifecycleSpec {
    pub(crate) fn cerebras_fixture(auth_path: AuthLifecycleAuthPath) -> Self {
        Self {
            provider_id: "cerebras",
            provider_label: "Cerebras",
            profile: crate::provider_catalog::CEREBRAS_PROFILE,
            auth_path,
            api_key: "test-cerebras-key".to_string(),
            catalog_models_after_auth: vec![
                "qwen-3-235b-a22b-instruct-2507".to_string(),
                "llama3.1-8b".to_string(),
                "gpt-oss-120b".to_string(),
            ],
            selected_model_override: None,
            current_runtime_provider_name: "mock-auth",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PickerSnapshot {
    pub selected_model: Option<String>,
    pub provider_entries: Vec<String>,
    pub switch_target: Option<String>,
    pub switch_request: Option<String>,
}

impl PickerSnapshot {
    fn build(
        spec: &AuthLifecycleSpec,
        activation: &AuthActivationResult,
        selected_model: Option<&str>,
        routes: &[ModelRoute],
    ) -> Self {
        let provider_entries = routes
            .iter()
            .filter(|route| route.available && route_matches_spec(route, spec))
            .map(|route| route.model.clone())
            .collect::<Vec<_>>();
        let selected_model = selected_model
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(ToString::to_string);
        let switch_target = provider_entries
            .iter()
            .find(|model| Some(model.as_str()) != selected_model.as_deref())
            .or_else(|| provider_entries.first())
            .cloned();
        let switch_request = switch_target.as_deref().map(|model| {
            activation.model_switch_request(spec.current_runtime_provider_name, model)
        });

        Self {
            selected_model,
            provider_entries,
            switch_target,
            switch_request,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AuthLifecycleResult {
    pub activation: AuthActivationResult,
    pub transcript: Vec<String>,
    pub catalog_report: AuthCatalogInvariantReport,
    pub picker: PickerSnapshot,
    pub catalog_routes: Vec<ModelRoute>,
    pub credential_location: Option<String>,
}

impl AuthLifecycleResult {
    pub(crate) fn assert_success(&self, spec: &AuthLifecycleSpec) {
        assert!(self.catalog_report.ok(), "{}", self.failure_report(spec));
        assert_eq!(
            self.activation.provider_id.as_deref(),
            Some(spec.provider_id)
        );
        assert_eq!(
            self.activation.provider_label.as_deref(),
            Some(spec.provider_label)
        );
        assert_eq!(
            self.activation.expected_runtime.as_deref(),
            Some("openai-compatible")
        );
        assert_eq!(
            self.activation.expected_catalog_namespace.as_deref(),
            Some(spec.provider_id)
        );
        assert!(
            self.transcript_text()
                .contains(&format!("{} credentials are active", spec.provider_label)),
            "{}",
            self.failure_report(spec)
        );
        assert!(
            !self
                .transcript_text()
                .contains("OpenAI credentials are active"),
            "{}",
            self.failure_report(spec)
        );
        assert!(
            !self
                .transcript_text()
                .contains("OpenRouter credentials are active"),
            "{}",
            self.failure_report(spec)
        );
        assert!(
            !self.picker.provider_entries.is_empty(),
            "{}",
            self.failure_report(spec)
        );
        assert!(
            self.picker
                .selected_model
                .as_ref()
                .is_some_and(|selected| self
                    .picker
                    .provider_entries
                    .iter()
                    .any(|entry| entry == selected)),
            "{}",
            self.failure_report(spec)
        );
        assert!(
            self.picker.switch_target.is_some(),
            "{}",
            self.failure_report(spec)
        );
        assert!(
            self.picker
                .switch_request
                .as_ref()
                .is_some_and(|request| !request.trim().is_empty()),
            "{}",
            self.failure_report(spec)
        );
    }

    pub(crate) fn transcript_text(&self) -> String {
        self.transcript.join("\n\n")
    }

    pub(crate) fn failure_report(&self, spec: &AuthLifecycleSpec) -> String {
        let warning = self
            .catalog_report
            .warning_message()
            .unwrap_or_else(|| "none".to_string());
        let route_sample = self
            .catalog_routes
            .iter()
            .take(8)
            .map(|route| {
                format!(
                    "{} via {} provider={} available={}",
                    route.model, route.api_method, route.provider, route.available
                )
            })
            .collect::<Vec<_>>()
            .join("\n  ");
        format!(
            "auth lifecycle failed for {} via {:?}\ncredential: {:?}\nactivation: {:?}\ncatalog invariant: {:?}\nwarning: {}\npicker: {:?}\nroutes:\n  {}\ntranscript:\n{}",
            spec.provider_label,
            spec.auth_path,
            self.credential_location,
            self.activation,
            self.catalog_report,
            warning,
            self.picker,
            route_sample,
            self.transcript_text()
        )
    }
}

pub(crate) struct AuthLifecycleDriver {
    sandbox: AuthTestSandbox,
}

impl AuthLifecycleDriver {
    pub(crate) fn new() -> anyhow::Result<Self> {
        Ok(Self {
            sandbox: AuthTestSandbox::new()?,
        })
    }

    pub(crate) fn run_openai_compatible_fixture(
        &self,
        spec: &AuthLifecycleSpec,
    ) -> anyhow::Result<AuthLifecycleResult> {
        let resolved = crate::provider_catalog::resolve_openai_compatible_profile(spec.profile);
        ensure!(
            resolved.id == spec.provider_id,
            "spec provider id {} did not match profile {}",
            spec.provider_id,
            resolved.id
        );

        let credential_location = self.apply_credentials(spec, &resolved)?;
        let auth = AuthChanged {
            provider: crate::protocol::AuthProviderId::new(spec.provider_id),
            credential_source: Some(spec.auth_path.credential_source()),
            auth_method: Some(spec.auth_path.auth_method()),
            expected_runtime: Some(RuntimeProviderKey::new("openai-compatible")),
            expected_catalog_namespace: Some(CatalogNamespace::new(spec.provider_id)),
        };
        let activation = activate_auth_change(&AuthActivationRequest::new(None, Some(auth)));
        let selected_model = spec
            .selected_model_override
            .clone()
            .or_else(|| activation.activated_model.clone());
        let catalog_routes = self.catalog_routes_for_spec(spec);
        let catalog_report =
            validate_catalog_invariants(&activation, selected_model.as_deref(), &catalog_routes);
        let picker = PickerSnapshot::build(
            spec,
            &activation,
            selected_model.as_deref(),
            &catalog_routes,
        );
        let transcript = self.user_visible_transcript(
            spec,
            &resolved,
            selected_model.as_deref(),
            catalog_report.warning_message().as_deref(),
        );

        Ok(AuthLifecycleResult {
            activation,
            transcript,
            catalog_report,
            picker,
            catalog_routes,
            credential_location,
        })
    }

    fn apply_credentials(
        &self,
        spec: &AuthLifecycleSpec,
        resolved: &crate::provider_catalog::ResolvedOpenAiCompatibleProfile,
    ) -> anyhow::Result<Option<String>> {
        match spec.auth_path {
            AuthLifecycleAuthPath::TuiPasteApiKey
            | AuthLifecycleAuthPath::RemoteTuiPasteApiKey
            | AuthLifecycleAuthPath::EnvFilePreseeded => {
                let path = self
                    .sandbox
                    .write_openai_compatible_api_key(spec.profile, &spec.api_key)
                    .with_context(|| format!("write {} API key file", spec.provider_label))?;
                Ok(Some(path.display().to_string()))
            }
            AuthLifecycleAuthPath::ProcessEnvPreseeded => {
                crate::env::set_var(&resolved.api_key_env, &spec.api_key);
                crate::auth::AuthStatus::invalidate_cache();
                Ok(Some(format!("process env {}", resolved.api_key_env)))
            }
        }
    }

    fn catalog_routes_for_spec(&self, spec: &AuthLifecycleSpec) -> Vec<ModelRoute> {
        spec.catalog_models_after_auth
            .iter()
            .map(|model| ModelRoute {
                model: model.clone(),
                provider: spec.provider_label.to_string(),
                api_method: format!("openai-compatible:{}", spec.provider_id),
                available: true,
                detail: "fixture live-catalog route".to_string(),
                cheapness: None,
            })
            .collect()
    }

    fn user_visible_transcript(
        &self,
        spec: &AuthLifecycleSpec,
        resolved: &crate::provider_catalog::ResolvedOpenAiCompatibleProfile,
        selected_model: Option<&str>,
        warning: Option<&str>,
    ) -> Vec<String> {
        let mut transcript = Vec::new();
        if spec.auth_path.shows_paste_prompt() {
            transcript.push(format!(
                "**{} API Key**\n\nSetup docs: {}\nStored variable: `{}`\nEndpoint: `{}`\nSuggested default model: `{}`\n\n**Paste your API key below** (it will be saved securely), or type `/cancel` to abort.",
                spec.provider_label,
                resolved.setup_url,
                resolved.api_key_env,
                resolved.api_base,
                resolved.default_model.as_deref().unwrap_or("none")
            ));
            transcript.push(format!(
                "**{} API key saved.**\n\nStored at `{}`.\nFetching models now. Jcode will switch to an accessible model returned by the live catalog and show the catalog diff when discovery finishes.",
                spec.provider_label,
                self.sandbox.env_file_path(&resolved.env_file).display()
            ));
        } else {
            transcript.push(format!(
                "**{} credentials detected.**\n\nCredential source: {:?}. Fetching models now.",
                spec.provider_label,
                spec.auth_path.credential_source()
            ));
        }
        transcript.push(
            "**Auth Change Received**\n\nThe server is reloading provider credentials and refreshing model route availability for this session."
                .to_string(),
        );
        transcript.push(
            "**Auth Model Routes Updating**\n\nCredentials are reloaded. Jcode is pushing an updated model catalog snapshot to connected clients."
                .to_string(),
        );
        let mut updated = format!(
            "**Auth Model Catalog Updated**\n\n{} credentials are active. Catalog diff:\n\nModels: fixture-before → fixture-after\nRoutes: fixture-before → fixture-after\n\nSelected model: `{}`.",
            spec.provider_label,
            selected_model.unwrap_or("none")
        );
        if let Some(warning) = warning {
            updated.push_str(warning);
        }
        transcript.push(updated);
        transcript
    }
}

fn route_matches_spec(route: &ModelRoute, spec: &AuthLifecycleSpec) -> bool {
    route.provider.eq_ignore_ascii_case(spec.provider_label)
        || route
            .api_method
            .eq_ignore_ascii_case(&format!("openai-compatible:{}", spec.provider_id))
        || route.api_method.eq_ignore_ascii_case(spec.provider_id)
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatibleModelsResponse {
    #[serde(default)]
    data: Vec<OpenAiCompatibleModelInfo>,
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatibleModelInfo {
    id: String,
}

pub(crate) async fn fetch_live_openai_compatible_models(
    profile: OpenAiCompatibleProfile,
    api_key: &str,
) -> anyhow::Result<Vec<String>> {
    let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
    let url = format!("{}/models", resolved.api_base.trim_end_matches('/'));
    let request = crate::provider::shared_http_client()
        .get(&url)
        .bearer_auth(api_key);
    let response = tokio::time::timeout(std::time::Duration::from_secs(20), request.send())
        .await
        .context("timed out fetching live model catalog")?
        .with_context(|| {
            format!(
                "fetch live {} model catalog from {url}",
                resolved.display_name
            )
        })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    ensure!(
        status.is_success(),
        "{} live model catalog failed (HTTP {}): {}",
        resolved.display_name,
        status,
        body.trim()
    );

    let parsed: OpenAiCompatibleModelsResponse = serde_json::from_str(&body)
        .with_context(|| format!("parse live {} model catalog", resolved.display_name))?;
    let models = parsed
        .data
        .into_iter()
        .map(|model| model.id.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();
    ensure!(
        !models.is_empty(),
        "{} live model catalog returned no models",
        resolved.display_name
    );
    Ok(models)
}

pub(crate) async fn run_live_openai_compatible_smoke(
    profile: OpenAiCompatibleProfile,
    api_key: &str,
    model: &str,
) -> anyhow::Result<()> {
    let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
    let url = format!(
        "{}/chat/completions",
        resolved.api_base.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "user", "content": "Reply with exactly AUTH_TEST_OK and nothing else."}
        ],
        "temperature": 0,
        "stream": false
    });
    let request = crate::provider::shared_http_client()
        .post(&url)
        .bearer_auth(api_key)
        .json(&body);
    let response = tokio::time::timeout(std::time::Duration::from_secs(30), request.send())
        .await
        .context("timed out running live smoke completion")?
        .with_context(|| format!("run live {} smoke completion", resolved.display_name))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    ensure!(
        status.is_success(),
        "{} live smoke failed (HTTP {}): {}",
        resolved.display_name,
        status,
        text.trim()
    );
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("parse live {} smoke response", resolved.display_name))?;
    let content = parsed
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .unwrap_or_default()
        .trim();
    ensure!(
        content.contains("AUTH_TEST_OK"),
        "{} live smoke returned unexpected content: {:?}",
        resolved.display_name,
        content
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_truthy(key: &str) -> bool {
        std::env::var(key)
            .ok()
            .map(|value| {
                let value = value.trim();
                !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
            })
            .unwrap_or(false)
    }

    fn live_cerebras_api_key() -> Option<String> {
        std::env::var("JCODE_AUTH_LIFECYCLE_CEREBRAS_API_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    fn stale_openai_route(model: &str) -> ModelRoute {
        ModelRoute {
            model: model.to_string(),
            provider: "OpenAI".to_string(),
            api_method: "openai".to_string(),
            available: true,
            detail: "stale route".to_string(),
            cheapness: None,
        }
    }

    #[test]
    fn cerebras_remote_tui_paste_key_fixture_covers_catalog_picker_and_switch() {
        let driver = AuthLifecycleDriver::new().expect("driver");
        let spec = AuthLifecycleSpec::cerebras_fixture(AuthLifecycleAuthPath::RemoteTuiPasteApiKey);

        let result = driver
            .run_openai_compatible_fixture(&spec)
            .expect("lifecycle result");

        result.assert_success(&spec);
        assert!(result.transcript_text().contains("**Cerebras API Key**"));
        assert!(
            result
                .transcript_text()
                .contains("**Cerebras API key saved.**")
        );
        assert_eq!(
            result.picker.selected_model.as_deref(),
            Some("qwen-3-235b-a22b-instruct-2507")
        );
        assert_eq!(result.picker.switch_target.as_deref(), Some("llama3.1-8b"));
        assert_eq!(
            result.picker.switch_request.as_deref(),
            Some("openrouter:llama3.1-8b")
        );
    }

    #[test]
    fn cerebras_state_space_catches_stale_openai_catalog_after_auth() {
        let driver = AuthLifecycleDriver::new().expect("driver");
        let mut spec =
            AuthLifecycleSpec::cerebras_fixture(AuthLifecycleAuthPath::RemoteTuiPasteApiKey);
        spec.catalog_models_after_auth.clear();
        spec.selected_model_override = Some("gpt-5.5".to_string());

        let mut result = driver
            .run_openai_compatible_fixture(&spec)
            .expect("lifecycle result");
        result.catalog_routes = vec![stale_openai_route("gpt-5.5")];
        result.catalog_report = validate_catalog_invariants(
            &result.activation,
            result.picker.selected_model.as_deref(),
            &result.catalog_routes,
        );
        result.picker = PickerSnapshot::build(
            &spec,
            &result.activation,
            result.picker.selected_model.as_deref(),
            &result.catalog_routes,
        );

        assert!(!result.catalog_report.ok());
        let failure = result.failure_report(&spec);
        assert!(failure.contains("Expected selectable Cerebras model routes"));
        assert!(failure.contains("Selected model: `gpt-5.5`"));
        assert!(failure.contains("OpenAI"));
    }

    #[test]
    fn cerebras_env_file_and_process_env_paths_share_same_lifecycle_invariants() {
        for auth_path in [
            AuthLifecycleAuthPath::TuiPasteApiKey,
            AuthLifecycleAuthPath::EnvFilePreseeded,
            AuthLifecycleAuthPath::ProcessEnvPreseeded,
        ] {
            let driver = AuthLifecycleDriver::new().expect("driver");
            let spec = AuthLifecycleSpec::cerebras_fixture(auth_path);

            let result = driver
                .run_openai_compatible_fixture(&spec)
                .expect("lifecycle result");

            result.assert_success(&spec);
            if auth_path.shows_paste_prompt() {
                assert!(result.transcript_text().contains("**Cerebras API Key**"));
            } else {
                assert!(
                    result
                        .transcript_text()
                        .contains("**Cerebras credentials detected.**")
                );
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cerebras_live_opt_in_catalog_lifecycle_uses_isolated_sandbox() {
        if !env_truthy("JCODE_AUTH_LIFECYCLE_LIVE") {
            eprintln!(
                "skipping live Cerebras auth lifecycle test; set JCODE_AUTH_LIFECYCLE_LIVE=1 and JCODE_AUTH_LIFECYCLE_CEREBRAS_API_KEY"
            );
            return;
        }
        let api_key = live_cerebras_api_key()
            .expect("JCODE_AUTH_LIFECYCLE_LIVE=1 requires JCODE_AUTH_LIFECYCLE_CEREBRAS_API_KEY");

        let models = fetch_live_openai_compatible_models(
            crate::provider_catalog::CEREBRAS_PROFILE,
            &api_key,
        )
        .await
        .expect("live Cerebras model catalog");
        let default_model = crate::provider_catalog::CEREBRAS_PROFILE.default_model;
        let selected = default_model
            .filter(|default| models.iter().any(|model| model == default))
            .map(ToString::to_string)
            .or_else(|| models.first().cloned())
            .expect("live catalog has model");

        let driver = AuthLifecycleDriver::new().expect("driver");
        let mut spec =
            AuthLifecycleSpec::cerebras_fixture(AuthLifecycleAuthPath::RemoteTuiPasteApiKey);
        spec.api_key = api_key.clone();
        spec.catalog_models_after_auth = models;
        spec.selected_model_override = Some(selected.clone());

        let result = driver
            .run_openai_compatible_fixture(&spec)
            .expect("live lifecycle result");

        result.assert_success(&spec);
        assert!(
            result
                .catalog_routes
                .iter()
                .any(|route| route.model == selected && route.provider == "Cerebras"),
            "{}",
            result.failure_report(&spec)
        );

        if env_truthy("JCODE_AUTH_LIFECYCLE_SMOKE") {
            run_live_openai_compatible_smoke(
                crate::provider_catalog::CEREBRAS_PROFILE,
                &api_key,
                &selected,
            )
            .await
            .expect("live Cerebras smoke completion");
        }
    }
}
