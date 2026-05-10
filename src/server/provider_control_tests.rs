use super::*;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, ModelRoute, Provider};
use crate::tool::Registry;
use async_trait::async_trait;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::RwLock as StdRwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex as StdMutex, MutexGuard as StdMutexGuard, OnceLock};

#[derive(Default)]
struct AuthChangeMockState {
    logged_in: StdRwLock<bool>,
    selected_model: StdRwLock<Option<String>>,
    complete_calls: AtomicUsize,
}

struct AuthChangeMockProvider {
    state: Arc<AuthChangeMockState>,
}

impl AuthChangeMockProvider {
    fn new() -> Self {
        Self {
            state: Arc::new(AuthChangeMockState::default()),
        }
    }
}

#[async_trait]
impl Provider for AuthChangeMockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        self.state.complete_calls.fetch_add(1, Ordering::SeqCst);
        let stream = futures::stream::empty::<anyhow::Result<StreamEvent>>();
        Ok(Box::pin(stream) as Pin<Box<dyn futures::Stream<Item = _> + Send>>)
    }

    fn name(&self) -> &str {
        "mock-auth"
    }

    fn model(&self) -> String {
        if let Some(model) = self.state.selected_model.read().unwrap().clone() {
            return model;
        }

        if *self.state.logged_in.read().unwrap() {
            "logged-in-model".to_string()
        } else {
            "logged-out-model".to_string()
        }
    }

    fn available_models_display(&self) -> Vec<String> {
        let mut models = if *self.state.logged_in.read().unwrap() {
            vec!["logged-in-model".to_string(), "second-model".to_string()]
        } else {
            vec!["logged-out-model".to_string()]
        };

        if let Some(model) = self.state.selected_model.read().unwrap().clone()
            && !models.iter().any(|candidate| candidate == &model)
        {
            models.insert(0, model);
        }

        models
    }

    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        let model = model
            .trim()
            .strip_prefix("openrouter:")
            .unwrap_or_else(|| model.trim())
            .trim();
        if model.is_empty() {
            anyhow::bail!("model cannot be empty");
        }

        *self.state.selected_model.write().unwrap() = Some(model.to_string());
        Ok(())
    }

    fn model_routes(&self) -> Vec<ModelRoute> {
        self.available_models_display()
            .into_iter()
            .map(|model| ModelRoute {
                model,
                provider: "MockAuth".to_string(),
                api_method: "mock-auth".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            })
            .collect()
    }

    fn on_auth_changed(&self) {
        *self.state.logged_in.write().unwrap() = true;
        crate::bus::Bus::global().publish_models_updated();
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            state: Arc::clone(&self.state),
        })
    }
}

fn lock_env() -> StdMutexGuard<'static, ()> {
    static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| StdMutex::new(())).lock().unwrap()
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
    _lock: StdMutexGuard<'static, ()>,
}

impl EnvGuard {
    fn save(keys: &[&'static str]) -> Self {
        let lock = lock_env();
        let saved = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        for key in keys {
            crate::env::remove_var(key);
        }
        Self { saved, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            if let Some(value) = value {
                crate::env::set_var(key, value);
            } else {
                crate::env::remove_var(key);
            }
        }
    }
}

#[tokio::test]
async fn notify_auth_changed_emits_available_models_updated_after_provider_update() {
    let _guard = EnvGuard::save(&[]);
    crate::bus::reset_models_updated_publish_state_for_tests();
    let provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let registry = Registry::empty();
    let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), registry)));
    let session_id = { agent.lock().await.session_id().to_string() };
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        "test-session".to_string(),
        Arc::clone(&agent),
    )])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();
    let mut bus_rx = crate::bus::Bus::global().subscribe();
    while bus_rx.try_recv().is_ok() {}

    handle_notify_auth_changed(
        42,
        None,
        None,
        &provider,
        &provider,
        &sessions,
        &agent,
        &client_event_tx,
    )
    .await;

    let mut saw_done = false;
    let mut saw_models = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(remaining, client_event_rx.recv())
            .await
            .expect("receive server event before timeout");
        match event.expect("channel open") {
            ServerEvent::Done { id } => {
                assert_eq!(id, 42);
                saw_done = true;
            }
            ServerEvent::AvailableModelsUpdated {
                provider_name,
                provider_model,
                available_models,
                available_model_routes,
            } => {
                saw_models = Some((
                    provider_name,
                    provider_model,
                    available_models,
                    available_model_routes,
                ));
                break;
            }
            _ => {}
        }
    }

    assert!(saw_done, "expected immediate Done ack");
    let (provider_name, provider_model, available_models, available_model_routes) =
        saw_models.expect("expected AvailableModelsUpdated event");
    assert_eq!(provider_name.as_deref(), Some("mock-auth"));
    assert_eq!(provider_model.as_deref(), Some("logged-in-model"));
    assert_eq!(
        available_models,
        vec!["logged-in-model".to_string(), "second-model".to_string()]
    );
    assert!(available_model_routes.iter().any(|route| {
        route.model == "logged-in-model"
            && route.provider == "MockAuth"
            && route.api_method == "mock-auth"
    }));

    let final_activity = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match bus_rx.recv().await.expect("bus should stay open") {
                crate::bus::BusEvent::UiActivity(activity)
                    if activity.kind == crate::bus::UiActivityKind::Catalog
                        && activity.session_id.as_deref() == Some(session_id.as_str())
                        && activity.message.contains("Auth Model Catalog Updated") =>
                {
                    break activity;
                }
                _ => continue,
            }
        }
    })
    .await
    .expect("expected final auth catalog activity");
    assert!(final_activity.message.contains("Added models:"));
    assert!(final_activity.message.contains("`logged-in-model`"));
    assert!(final_activity.message.contains("`second-model`"));
    assert!(
        final_activity
            .message
            .contains("Selected model: `logged-in-model`")
    );
    assert!(final_activity.message.contains("Use `/model`"));
}

#[tokio::test]
async fn notify_auth_changed_defers_busy_session_refresh_until_idle() {
    let _guard = EnvGuard::save(&[]);
    crate::bus::reset_models_updated_publish_state_for_tests();
    let current_provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let busy_provider = Arc::new(AuthChangeMockProvider::new());
    let busy_state = Arc::clone(&busy_provider.state);
    let busy_provider: Arc<dyn Provider> = busy_provider;
    let registry = Registry::empty();
    let current_agent = Arc::new(Mutex::new(Agent::new(
        Arc::clone(&current_provider),
        registry.clone(),
    )));
    let busy_agent = Arc::new(Mutex::new(Agent::new(busy_provider, registry)));
    let busy_guard = busy_agent.lock().await;
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        "busy-session".to_string(),
        Arc::clone(&busy_agent),
    )])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    handle_notify_auth_changed(
        43,
        None,
        None,
        &current_provider,
        &current_provider,
        &sessions,
        &current_agent,
        &client_event_tx,
    )
    .await;

    assert!(
        matches!(
            client_event_rx.recv().await,
            Some(ServerEvent::Done { id: 43 })
        ),
        "expected immediate Done ack before waiting for the busy session"
    );
    assert!(
        !*busy_state.logged_in.read().unwrap(),
        "busy session provider should not refresh until its agent lock is released"
    );

    drop(busy_guard);

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if *busy_state.logged_in.read().unwrap() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    panic!("busy session provider was not refreshed after it became idle");
}

#[tokio::test]
async fn notify_auth_changed_with_azure_hint_applies_runtime_model_without_completion() {
    let _guard = EnvGuard::save(&[
        "AZURE_OPENAI_ENDPOINT",
        "AZURE_OPENAI_MODEL",
        "AZURE_OPENAI_API_KEY",
        "AZURE_OPENAI_USE_ENTRA",
        "JCODE_OPENROUTER_API_BASE",
        "JCODE_OPENROUTER_API_KEY_NAME",
        "JCODE_OPENROUTER_ENV_FILE",
        "JCODE_OPENROUTER_CACHE_NAMESPACE",
        "JCODE_OPENROUTER_PROVIDER_FEATURES",
        "JCODE_OPENROUTER_MODEL_CATALOG",
        "JCODE_OPENROUTER_AUTH_HEADER",
        "JCODE_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
        "JCODE_OPENROUTER_MODEL",
        "JCODE_RUNTIME_PROVIDER",
        "JCODE_ACTIVE_PROVIDER",
        "JCODE_FORCE_PROVIDER",
    ]);
    crate::env::set_var("AZURE_OPENAI_ENDPOINT", "https://example.openai.azure.com");
    crate::env::set_var("AZURE_OPENAI_MODEL", "azure-deployment");
    crate::env::set_var("AZURE_OPENAI_API_KEY", "test-key");
    crate::env::set_var("AZURE_OPENAI_USE_ENTRA", "0");

    crate::bus::reset_models_updated_publish_state_for_tests();
    let provider = Arc::new(AuthChangeMockProvider::new());
    let state = Arc::clone(&provider.state);
    let provider: Arc<dyn Provider> = provider;
    let registry = Registry::empty();
    let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), registry)));
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        "test-session".to_string(),
        Arc::clone(&agent),
    )])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    handle_notify_auth_changed(
        44,
        Some("Azure OpenAI".to_string()),
        None,
        &provider,
        &provider,
        &sessions,
        &agent,
        &client_event_tx,
    )
    .await;

    let mut saw_done = false;
    let mut saw_models = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(remaining, client_event_rx.recv())
            .await
            .expect("receive server event before timeout");
        match event.expect("channel open") {
            ServerEvent::Done { id } => {
                assert_eq!(id, 44);
                saw_done = true;
            }
            ServerEvent::AvailableModelsUpdated {
                provider_model,
                available_models,
                ..
            } => {
                saw_models = Some((provider_model, available_models));
                break;
            }
            _ => {}
        }
    }

    assert!(saw_done, "expected immediate Done ack");
    let (provider_model, available_models) = saw_models.expect("expected model refresh event");
    assert_eq!(provider_model.as_deref(), Some("azure-deployment"));
    assert!(
        available_models
            .iter()
            .any(|model| model == "azure-deployment")
    );
    assert_eq!(
        std::env::var("JCODE_RUNTIME_PROVIDER").as_deref(),
        Ok("azure-openai")
    );
    assert_eq!(
        std::env::var("JCODE_ACTIVE_PROVIDER").as_deref(),
        Ok("openrouter")
    );
    assert_eq!(
        state.complete_calls.load(Ordering::SeqCst),
        0,
        "auth refresh must not issue a completion with the old prompt/model"
    );
}

#[test]
fn cerebras_auth_hint_applies_openai_compatible_runtime_profile() {
    let _guard = EnvGuard::save(&[
        "JCODE_OPENROUTER_API_BASE",
        "JCODE_OPENROUTER_API_KEY_NAME",
        "JCODE_OPENROUTER_ENV_FILE",
        "JCODE_OPENROUTER_CACHE_NAMESPACE",
        "JCODE_OPENROUTER_PROVIDER_FEATURES",
        "JCODE_OPENROUTER_MODEL_CATALOG",
        "JCODE_OPENROUTER_AUTH_HEADER",
        "JCODE_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
        "JCODE_OPENROUTER_MODEL",
        "JCODE_RUNTIME_PROVIDER",
        "JCODE_ACTIVE_PROVIDER",
        "JCODE_FORCE_PROVIDER",
    ]);

    let request =
        crate::auth::lifecycle::AuthActivationRequest::new(Some("Cerebras".to_string()), None);
    assert_eq!(request.provider_id().as_deref(), Some("cerebras"));

    let activation = crate::auth::lifecycle::activate_auth_change(&request);
    let default_model = activation.activated_model.as_deref();
    assert_eq!(default_model, Some("qwen-3-235b-a22b-instruct-2507"));
    assert_eq!(
        std::env::var("JCODE_RUNTIME_PROVIDER").as_deref(),
        Ok("openai-compatible")
    );
    assert_eq!(
        std::env::var("JCODE_ACTIVE_PROVIDER").as_deref(),
        Ok("openrouter")
    );
    assert_eq!(
        std::env::var("JCODE_OPENROUTER_API_BASE").as_deref(),
        Ok("https://api.cerebras.ai/v1")
    );
    assert_eq!(
        std::env::var("JCODE_OPENROUTER_API_KEY_NAME").as_deref(),
        Ok("CEREBRAS_API_KEY")
    );
    assert_eq!(
        std::env::var("JCODE_OPENROUTER_ENV_FILE").as_deref(),
        Ok("cerebras.env")
    );
    assert_eq!(
        std::env::var("JCODE_OPENROUTER_CACHE_NAMESPACE").as_deref(),
        Ok("cerebras")
    );
    assert_eq!(
        activation.model_switch_request("mock-auth", "llama3.1-8b"),
        "openrouter:llama3.1-8b"
    );
}

#[tokio::test]
async fn notify_auth_changed_typed_cerebras_event_controls_user_visible_catalog_identity() {
    let _guard = EnvGuard::save(&[
        "JCODE_OPENROUTER_API_BASE",
        "JCODE_OPENROUTER_API_KEY_NAME",
        "JCODE_OPENROUTER_ENV_FILE",
        "JCODE_OPENROUTER_CACHE_NAMESPACE",
        "JCODE_OPENROUTER_PROVIDER_FEATURES",
        "JCODE_OPENROUTER_MODEL_CATALOG",
        "JCODE_OPENROUTER_AUTH_HEADER",
        "JCODE_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
        "JCODE_OPENROUTER_MODEL",
        "JCODE_RUNTIME_PROVIDER",
        "JCODE_ACTIVE_PROVIDER",
        "JCODE_FORCE_PROVIDER",
    ]);

    crate::bus::reset_models_updated_publish_state_for_tests();
    let provider = Arc::new(AuthChangeMockProvider::new());
    let provider: Arc<dyn Provider> = provider;
    let registry = Registry::empty();
    let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), registry)));
    let session_id = { agent.lock().await.session_id().to_string() };
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        "test-session".to_string(),
        Arc::clone(&agent),
    )])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();
    let mut bus_rx = crate::bus::Bus::global().subscribe();
    while bus_rx.try_recv().is_ok() {}

    let mut auth = crate::protocol::AuthChanged::new("cerebras");
    auth.credential_source = Some(crate::protocol::AuthCredentialSource::ApiKeyFile);
    auth.auth_method = Some(crate::protocol::AuthMethod::RemoteTuiPasteApiKey);
    auth.expected_runtime = Some(crate::protocol::RuntimeProviderKey::new(
        "openai-compatible",
    ));
    auth.expected_catalog_namespace = Some(crate::protocol::CatalogNamespace::new("cerebras"));

    handle_notify_auth_changed(
        45,
        Some("openai".to_string()),
        Some(auth),
        &provider,
        &provider,
        &sessions,
        &agent,
        &client_event_tx,
    )
    .await;

    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Done { id: 45 })
    ));

    let final_activity = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match bus_rx.recv().await.expect("bus should stay open") {
                crate::bus::BusEvent::UiActivity(activity)
                    if activity.kind == crate::bus::UiActivityKind::Catalog
                        && activity.session_id.as_deref() == Some(session_id.as_str())
                        && activity.message.contains("Auth Model Catalog Updated") =>
                {
                    break activity;
                }
                _ => continue,
            }
        }
    })
    .await
    .expect("expected final auth catalog activity");

    assert!(
        final_activity
            .message
            .contains("Cerebras credentials are active"),
        "typed auth event should control user-visible provider label, got: {}",
        final_activity.message
    );
    assert!(
        !final_activity
            .message
            .contains("OpenAI credentials are active"),
        "stale legacy provider identity leaked into user-visible auth message: {}",
        final_activity.message
    );
    assert!(
        final_activity
            .message
            .contains("Auth Model Catalog Warning"),
        "typed auth event should warn when matching provider routes are missing: {}",
        final_activity.message
    );
    assert!(
        final_activity
            .message
            .contains("Expected selectable Cerebras model routes"),
        "warning should identify the expected provider: {}",
        final_activity.message
    );
    assert_eq!(
        std::env::var("JCODE_OPENROUTER_CACHE_NAMESPACE").as_deref(),
        Ok("cerebras")
    );
}

#[tokio::test]
async fn refresh_models_emits_available_models_updated_after_prefetch() {
    crate::bus::reset_models_updated_publish_state_for_tests();
    let provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let registry = Registry::empty();
    let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), registry)));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    handle_refresh_models(7, &provider, &agent, &client_event_tx).await;

    let mut saw_done = false;
    let mut saw_models = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(remaining, client_event_rx.recv())
            .await
            .expect("receive server event before timeout");
        match event.expect("channel open") {
            ServerEvent::Done { id } => {
                assert_eq!(id, 7);
                saw_done = true;
            }
            ServerEvent::AvailableModelsUpdated {
                provider_name,
                provider_model,
                available_models,
                available_model_routes,
            } => {
                saw_models = Some((
                    provider_name,
                    provider_model,
                    available_models,
                    available_model_routes,
                ));
                break;
            }
            _ => {}
        }
    }

    assert!(saw_done, "expected immediate Done ack");
    let (provider_name, provider_model, available_models, available_model_routes) =
        saw_models.expect("expected AvailableModelsUpdated event");
    assert_eq!(provider_name.as_deref(), Some("mock-auth"));
    assert_eq!(provider_model.as_deref(), Some("logged-out-model"));
    assert_eq!(available_models, vec!["logged-out-model".to_string()]);
    assert!(available_model_routes.iter().any(|route| {
        route.model == "logged-out-model"
            && route.provider == "MockAuth"
            && route.api_method == "mock-auth"
    }));
}
