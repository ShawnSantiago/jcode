use crate::protocol::{AuthChanged, CatalogNamespace, RuntimeProviderKey};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthActivationRequest {
    pub legacy_provider_hint: Option<String>,
    pub auth: Option<AuthChanged>,
}

impl AuthActivationRequest {
    pub fn new(legacy_provider_hint: Option<String>, auth: Option<AuthChanged>) -> Self {
        Self {
            legacy_provider_hint,
            auth,
        }
    }

    pub fn provider_id(&self) -> Option<String> {
        self.auth
            .as_ref()
            .map(|auth| auth.provider.as_str().to_string())
            .or_else(|| self.legacy_provider_hint.clone())
            .and_then(|provider| {
                normalized_auth_provider_id(Some(provider.as_str())).map(str::to_string)
            })
    }

    pub fn expected_runtime(&self) -> Option<&RuntimeProviderKey> {
        self.auth
            .as_ref()
            .and_then(|auth| auth.expected_runtime.as_ref())
    }

    pub fn expected_catalog_namespace(&self) -> Option<&CatalogNamespace> {
        self.auth
            .as_ref()
            .and_then(|auth| auth.expected_catalog_namespace.as_ref())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthActivationResult {
    pub provider_id: Option<String>,
    pub provider_label: Option<String>,
    pub activated_model: Option<String>,
    pub expected_runtime: Option<String>,
    pub expected_catalog_namespace: Option<String>,
}

impl AuthActivationResult {
    pub fn model_switch_request(&self, current_provider_name: &str, model: &str) -> String {
        model_switch_request_for_provider_id(
            self.provider_id.as_deref(),
            current_provider_name,
            model,
        )
    }
}

pub fn normalized_auth_provider_id(provider_hint: Option<&str>) -> Option<&'static str> {
    let provider = provider_hint?.trim();
    if provider.eq_ignore_ascii_case("azure")
        || provider.eq_ignore_ascii_case("azure-openai")
        || provider.eq_ignore_ascii_case("azure openai")
    {
        Some("azure-openai")
    } else if let Some(profile) =
        crate::provider_catalog::resolve_openai_compatible_profile_selection(provider)
    {
        Some(profile.id)
    } else {
        None
    }
}

pub fn provider_display_label(provider_id: Option<&str>) -> Option<String> {
    let provider = normalized_auth_provider_id(provider_id)?;
    if provider == "azure-openai" {
        return Some("Azure OpenAI".to_string());
    }
    crate::provider_catalog::openai_compatible_profile_by_id(provider)
        .map(|profile| profile.display_name.to_string())
        .or_else(|| Some(provider.to_string()))
}

pub fn activate_auth_change(request: &AuthActivationRequest) -> AuthActivationResult {
    let provider_id = request.provider_id();
    let provider_label = provider_display_label(provider_id.as_deref());
    let activated_model = apply_auth_provider_runtime(provider_id.as_deref());
    AuthActivationResult {
        provider_id,
        provider_label,
        activated_model,
        expected_runtime: request
            .expected_runtime()
            .map(|runtime| runtime.as_str().to_string()),
        expected_catalog_namespace: request
            .expected_catalog_namespace()
            .map(|namespace| namespace.as_str().to_string()),
    }
}

fn apply_auth_provider_runtime(provider_id: Option<&str>) -> Option<String> {
    match normalized_auth_provider_id(provider_id) {
        Some("azure-openai") => match crate::provider::activation::apply_azure_openai_runtime() {
            Ok(model) => model,
            Err(error) => {
                let message = error.to_string();
                crate::logging::auth_event(
                    "auth_changed_runtime_activation_failed",
                    "azure-openai",
                    &[("reason", message.as_str())],
                );
                None
            }
        },
        Some(profile_id) => {
            if let Some(profile) =
                crate::provider_catalog::openai_compatible_profile_by_id(profile_id)
            {
                crate::provider_catalog::force_apply_openai_compatible_profile_env(Some(profile));
                let default_model =
                    crate::provider_catalog::resolve_openai_compatible_profile(profile)
                        .default_model;
                if let Err(error) = crate::provider::activation::apply_openai_compatible_runtime(
                    default_model.clone(),
                ) {
                    let message = error.to_string();
                    crate::logging::auth_event(
                        "auth_changed_runtime_activation_failed",
                        profile_id,
                        &[("reason", message.as_str())],
                    );
                    None
                } else {
                    default_model
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn model_switch_request_for_provider_id(
    provider_id: Option<&str>,
    provider_name: &str,
    model: &str,
) -> String {
    match normalized_auth_provider_id(provider_id) {
        Some("azure-openai") if !provider_name.eq_ignore_ascii_case("openrouter") => {
            format!("openrouter:{}", model)
        }
        Some(profile_id)
            if profile_id != "azure-openai"
                && !provider_name.eq_ignore_ascii_case("openrouter") =>
        {
            format!("openrouter:{}", model)
        }
        _ => model.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_auth_request_provider_id_wins_over_legacy_hint() {
        let request = AuthActivationRequest::new(
            Some("openai".to_string()),
            Some(AuthChanged::new("cerebras")),
        );

        assert_eq!(request.provider_id().as_deref(), Some("cerebras"));
        assert_eq!(
            provider_display_label(request.provider_id().as_deref()).as_deref(),
            Some("Cerebras")
        );
    }

    #[test]
    fn model_switch_request_prefixes_openai_compatible_profiles_for_non_openrouter_provider() {
        assert_eq!(
            model_switch_request_for_provider_id(Some("cerebras"), "mock-auth", "llama3.1-8b"),
            "openrouter:llama3.1-8b"
        );
        assert_eq!(
            model_switch_request_for_provider_id(Some("cerebras"), "openrouter", "llama3.1-8b"),
            "llama3.1-8b"
        );
    }
}
