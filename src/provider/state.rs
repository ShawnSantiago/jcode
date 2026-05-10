use super::selection::{ActiveProvider, ConfigProviderSelection, ProviderAvailability};
use super::MultiProvider;
use crate::auth::AuthStatus;
use crate::config::Config;

/// Canonical aggregate view of provider-related state.
///
/// This is intentionally not the durable storage for auth or config. Credentials
/// still live in auth/env files, and preferences still live in config.toml. This
/// facade is the single in-process place that combines those persisted sources
/// with provider catalog identity/resolution so CLI, TUI, and runtime code do not
/// each reinterpret provider strings differently.
pub(crate) struct ProviderState<'a> {
    config: &'a Config,
    auth_status: &'a AuthStatus,
}

impl<'a> ProviderState<'a> {
    pub(crate) fn from_parts(config: &'a Config, auth_status: &'a AuthStatus) -> Self {
        Self {
            config,
            auth_status,
        }
    }

    pub(crate) fn auth_status(&self) -> &'a AuthStatus {
        self.auth_status
    }

    pub(crate) fn default_model(&self) -> Option<&'a str> {
        self.config.provider.default_model.as_deref()
    }

    pub(crate) fn default_provider_key(&self) -> Option<&'a str> {
        self.config.provider.default_provider.as_deref()
    }

    pub(crate) fn default_provider_selection(&self) -> Option<ConfigProviderSelection> {
        self.default_provider_key()
            .and_then(|provider| MultiProvider::resolve_config_provider_selection(provider, self.config))
    }

    pub(crate) fn preferred_active_provider(&self) -> Option<ActiveProvider> {
        self.default_provider_selection()
            .map(|selection| selection.active_provider())
    }

    pub(crate) fn preferred_provider_display_label(&self) -> Option<String> {
        self.default_provider_selection()
            .map(|selection| selection.display_label())
    }

    pub(crate) fn preferred_provider_is_configured(
        &self,
        availability: ProviderAvailability,
    ) -> Option<bool> {
        self.preferred_active_provider()
            .map(|provider| availability.is_configured(provider))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_state_resolves_default_provider_through_canonical_selection() {
        let mut cfg = Config::default();
        cfg.provider.default_provider = Some("kimi".to_string());
        cfg.provider.default_model = Some("moonshot-v1-8k".to_string());
        let auth = AuthStatus::default();
        let state = ProviderState::from_parts(&cfg, &auth);

        assert_eq!(state.default_provider_key(), Some("kimi"));
        assert_eq!(state.default_model(), Some("moonshot-v1-8k"));
        assert_eq!(state.preferred_active_provider(), Some(ActiveProvider::OpenRouter));
        assert_eq!(
            state.preferred_provider_is_configured(ProviderAvailability {
                openrouter: true,
                ..ProviderAvailability::default()
            }),
            Some(true)
        );
    }
}
