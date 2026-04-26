use super::*;
use crate::bus::{
    BackgroundTaskCompleted, BackgroundTaskProgress, BackgroundTaskProgressEvent,
    BackgroundTaskProgressKind, BackgroundTaskProgressSource, BackgroundTaskStatus, BusEvent,
    ClientMaintenanceAction, InputShellCompleted, SessionUpdateStatus,
};
use crate::tui::TuiState;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc as StdArc, Mutex as StdMutex};
use std::time::{Duration, Instant};

fn cleanup_background_task_files(task_id: &str) {
    let task_dir = std::env::temp_dir().join("jcode-bg-tasks");
    let _ = std::fs::remove_file(task_dir.join(format!("{}.status.json", task_id)));
    let _ = std::fs::remove_file(task_dir.join(format!("{}.output", task_id)));
}

pub(super) fn cleanup_reload_context_file(session_id: &str) {
    if let Ok(path) = crate::tool::selfdev::ReloadContext::path_for_session(session_id) {
        let _ = std::fs::remove_file(path);
    }
}

// Mock provider for testing
struct MockProvider;

#[derive(Clone)]
struct RefreshSummaryProvider {
    summary: crate::provider::ModelCatalogRefreshSummary,
}

#[derive(Clone)]
struct OpenRouterSpecCaptureProvider {
    set_model_calls: StdArc<StdMutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("Mock provider")
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(MockProvider)
    }
}

#[async_trait::async_trait]
impl Provider for RefreshSummaryProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("RefreshSummaryProvider")
    }

    fn name(&self) -> &str {
        "refresh-summary"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    async fn refresh_model_catalog(&self) -> Result<crate::provider::ModelCatalogRefreshSummary> {
        Ok(self.summary.clone())
    }
}

#[async_trait::async_trait]
impl Provider for OpenRouterSpecCaptureProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("OpenRouterSpecCaptureProvider")
    }

    fn name(&self) -> &str {
        "openrouter-spec-capture"
    }

    fn model(&self) -> String {
        "gpt-5.4".to_string()
    }

    fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        vec![crate::provider::ModelRoute {
            model: "gpt-5.4".to_string(),
            provider: "OpenAI".to_string(),
            api_method: "openrouter".to_string(),
            available: true,
            detail: "cached route".to_string(),
            cheapness: None,
        }]
    }

    fn available_providers_for_model(&self, model: &str) -> Vec<String> {
        if model == "gpt-5.4" || model == "openai/gpt-5.4" {
            vec!["auto".to_string(), "OpenAI".to_string()]
        } else {
            Vec::new()
        }
    }

    fn available_efforts(&self) -> Vec<&'static str> {
        vec!["high"]
    }

    fn reasoning_effort(&self) -> Option<String> {
        Some("high".to_string())
    }

    fn set_reasoning_effort(&self, _effort: &str) -> Result<()> {
        Ok(())
    }

    fn set_model(&self, model: &str) -> Result<()> {
        self.set_model_calls.lock().unwrap().push(model.to_string());
        Ok(())
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn create_test_app() -> App {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app
}

fn create_refresh_summary_test_app(summary: crate::provider::ModelCatalogRefreshSummary) -> App {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let provider: Arc<dyn Provider> = Arc::new(RefreshSummaryProvider { summary });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app
}

fn create_openrouter_spec_capture_test_app() -> (App, StdArc<StdMutex<Vec<String>>>) {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let set_model_calls = StdArc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(OpenRouterSpecCaptureProvider {
        set_model_calls: set_model_calls.clone(),
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    (app, set_model_calls)
}

#[test]
fn local_add_provider_message_does_not_retain_local_provider_copy() {
    let mut app = create_test_app();
    app.add_provider_message(Message::user("hello"));
    assert!(app.messages.is_empty());
}

#[test]
fn remote_add_provider_message_retains_remote_provider_copy() {
    let mut app = create_test_app();
    app.is_remote = true;
    app.add_provider_message(Message::user("hello"));
    assert_eq!(app.messages.len(), 1);
}

#[test]
fn debug_memory_profile_includes_app_owned_summary_for_large_client_state() {
    let mut app = create_test_app();
    app.remote_side_pane_images
        .push(crate::session::RenderedImage {
            media_type: "image/png".to_string(),
            data: "x".repeat(32 * 1024),
            label: Some("preview.png".to_string()),
            source: crate::session::RenderedImageSource::UserInput,
        });
    app.observe_page_markdown = "# observe\n".repeat(256);
    app.input_undo_stack.push(("draft ".repeat(256), 12));

    let profile = app.debug_memory_profile();
    let app_owned = &profile["app_owned"];
    let summary = &profile["summary"];

    assert!(app_owned.is_object());
    assert!(summary.is_object());
    assert!(
        app_owned["images_and_views"]["remote_side_pane_images_bytes"]
            .as_u64()
            .unwrap_or(0)
            >= 32 * 1024
    );
    assert!(
        app_owned["input_history"]["undo_stack_bytes"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
    assert!(
        summary["total_app_owned_estimate_bytes"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
    assert!(
        !summary["top_buckets"]
            .as_array()
            .unwrap_or(&Vec::new())
            .is_empty()
    );
}

fn test_side_panel_snapshot(page_id: &str, title: &str) -> crate::side_panel::SidePanelSnapshot {
    crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some(page_id.to_string()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: page_id.to_string(),
            title: title.to_string(),
            file_path: format!("/tmp/{page_id}.md"),
            format: crate::side_panel::SidePanelPageFormat::Markdown,
            source: crate::side_panel::SidePanelPageSource::Managed,
            content: format!("# {title}"),
            updated_at_ms: 1,
        }],
    }
}

fn ensure_test_jcode_home_if_unset() {
    use std::sync::OnceLock;

    static TEST_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();

    if std::env::var_os("JCODE_HOME").is_some() {
        return;
    }

    let path = TEST_HOME.get_or_init(|| {
        let path = std::env::temp_dir().join(format!("jcode-test-home-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&path);
        path
    });
    crate::env::set_var("JCODE_HOME", path);
}

fn clear_persisted_test_ui_state() {
    if let Ok(home) = crate::storage::jcode_dir() {
        let ambient_dir = home.join("ambient");
        let _ = std::fs::remove_file(ambient_dir.join("queue.json"));
        let _ = std::fs::remove_file(ambient_dir.join("state.json"));
        let _ = std::fs::remove_file(ambient_dir.join("directives.json"));
        let _ = std::fs::remove_file(ambient_dir.join("visible_cycle.json"));
    }
    crate::tui::app::helpers::clear_ambient_info_cache_for_tests();
    crate::auth::AuthStatus::invalidate_cache();
}

fn with_temp_jcode_home<T>(f: impl FnOnce() -> T) -> T {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    crate::auth::claude::set_active_account_override(None);
    crate::auth::codex::set_active_account_override(None);
    crate::auth::AuthStatus::invalidate_cache();
    clear_persisted_test_ui_state();

    let result = f();

    crate::auth::claude::set_active_account_override(None);
    crate::auth::codex::set_active_account_override(None);
    crate::auth::AuthStatus::invalidate_cache();
    crate::tui::app::helpers::clear_ambient_info_cache_for_tests();
    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    result
}

fn create_jcode_repo_fixture() -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().expect("temp repo");
    std::fs::create_dir_all(temp.path().join(".git")).expect("git dir");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"jcode\"\nversion = \"0.1.0\"\n",
    )
    .expect("cargo toml");
    temp
}

fn create_real_git_repo_fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .expect("git init");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp.path())
        .output()
        .expect("git config email");
    std::process::Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(temp.path())
        .output()
        .expect("git config name");
    std::fs::write(temp.path().join("tracked.txt"), "before\n").expect("write tracked file");
    std::process::Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(temp.path())
        .output()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(temp.path())
        .output()
        .expect("git commit");
    temp
}

#[test]
fn test_handle_turn_error_failover_prompt_manual_mode_shows_system_notice() {
    with_temp_jcode_home(|| {
        write_test_config("[provider]\ncross_provider_failover = \"manual\"\n");
        let mut app = create_test_app();
        let prompt = crate::provider::ProviderFailoverPrompt {
            from_provider: "claude".to_string(),
            from_label: "Anthropic".to_string(),
            to_provider: "openai".to_string(),
            to_label: "OpenAI".to_string(),
            reason: "OAuth usage exhausted".to_string(),
            estimated_input_chars: 48_000,
            estimated_input_tokens: 12_000,
        };

        app.handle_turn_error(failover_error_message(&prompt));

        let last = app.display_messages.last().expect("display message");
        assert_eq!(last.role, "system");
        assert!(last.content.contains("did **not** resend your prompt"));
        assert!(last.content.contains("/model"));
        assert!(
            last.content
                .contains("cross_provider_failover = \"manual\"")
        );
        assert!(app.pending_provider_failover.is_none());
    });
}

#[test]
fn test_handle_turn_error_failover_prompt_countdown_can_switch_and_retry() {
    with_temp_jcode_home(|| {
        write_test_config("[provider]\ncross_provider_failover = \"countdown\"\n");
        let (mut app, active_provider) = create_switchable_test_app("claude");
        let prompt = crate::provider::ProviderFailoverPrompt {
            from_provider: "claude".to_string(),
            from_label: "Anthropic".to_string(),
            to_provider: "openai".to_string(),
            to_label: "OpenAI".to_string(),
            reason: "OAuth usage exhausted".to_string(),
            estimated_input_chars: 32_000,
            estimated_input_tokens: 8_000,
        };

        app.handle_turn_error(failover_error_message(&prompt));
        assert!(app.pending_provider_failover.is_some());

        if let Some(pending) = app.pending_provider_failover.as_mut() {
            pending.deadline = Instant::now() - Duration::from_secs(1);
        }
        app.maybe_progress_provider_failover_countdown();

        assert!(app.pending_provider_failover.is_none());
        assert!(app.pending_turn);
        assert_eq!(active_provider.lock().unwrap().as_str(), "openai");
        assert_eq!(app.session.model.as_deref(), Some("gpt-test"));
        let last = app.display_messages.last().expect("display message");
        assert!(
            last.content
                .contains("cross_provider_failover = \"manual\"")
        );
    });
}

#[test]
fn test_cancel_pending_provider_failover_clears_countdown() {
    with_temp_jcode_home(|| {
        write_test_config("[provider]\ncross_provider_failover = \"countdown\"\n");
        let (mut app, _active_provider) = create_switchable_test_app("claude");
        let prompt = crate::provider::ProviderFailoverPrompt {
            from_provider: "claude".to_string(),
            from_label: "Anthropic".to_string(),
            to_provider: "openai".to_string(),
            to_label: "OpenAI".to_string(),
            reason: "OAuth usage exhausted".to_string(),
            estimated_input_chars: 16_000,
            estimated_input_tokens: 4_000,
        };

        app.handle_turn_error(failover_error_message(&prompt));
        assert!(app.pending_provider_failover.is_some());

        app.cancel_pending_provider_failover("Provider auto-switch canceled");

        assert!(app.pending_provider_failover.is_none());
        let last = app.display_messages.last().expect("display message");
        assert_eq!(last.role, "system");
        assert!(last.content.contains("Canceled provider auto-switch"));
        assert!(
            last.content
                .contains("cross_provider_failover = \"manual\"")
        );
    });
}

#[derive(Clone)]
struct FastMockProvider {
    service_tier: StdArc<StdMutex<Option<String>>>,
}

#[async_trait::async_trait]
impl Provider for FastMockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("FastMockProvider")
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    fn service_tier(&self) -> Option<String> {
        self.service_tier.lock().unwrap().clone()
    }

    fn set_service_tier(&self, service_tier: &str) -> anyhow::Result<()> {
        let normalized = match service_tier.trim().to_ascii_lowercase().as_str() {
            "priority" | "fast" => Some("priority".to_string()),
            "off" | "default" | "auto" | "none" => None,
            other => anyhow::bail!("unsupported service tier {other}"),
        };
        *self.service_tier.lock().unwrap() = normalized;
        Ok(())
    }
}

#[derive(Clone)]
struct SwitchableMockProvider {
    active_provider: StdArc<StdMutex<String>>,
}

#[async_trait::async_trait]
impl Provider for SwitchableMockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("SwitchableMockProvider")
    }

    fn name(&self) -> &str {
        "switchable-mock"
    }

    fn model(&self) -> String {
        match self.active_provider.lock().unwrap().as_str() {
            "openai" => "gpt-test".to_string(),
            _ => "claude-test".to_string(),
        }
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    fn switch_active_provider_to(&self, provider: &str) -> Result<()> {
        *self.active_provider.lock().unwrap() = provider.to_string();
        Ok(())
    }
}

fn create_switchable_test_app(initial_provider: &str) -> (App, StdArc<StdMutex<String>>) {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let active_provider = StdArc::new(StdMutex::new(initial_provider.to_string()));
    let provider: Arc<dyn Provider> = Arc::new(SwitchableMockProvider {
        active_provider: active_provider.clone(),
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    (app, active_provider)
}

#[derive(Clone)]
struct AuthRefreshingMockProvider {
    logged_in: StdArc<StdMutex<bool>>,
}

#[async_trait::async_trait]
impl Provider for AuthRefreshingMockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("AuthRefreshingMockProvider")
    }

    fn name(&self) -> &str {
        "auth-refresh-mock"
    }

    fn model(&self) -> String {
        if *self.logged_in.lock().unwrap() {
            "claude-opus-4.6".to_string()
        } else {
            "gpt-5.4".to_string()
        }
    }

    fn available_models_display(&self) -> Vec<String> {
        if *self.logged_in.lock().unwrap() {
            vec![
                "claude-opus-4.6".to_string(),
                "grok-code-fast-1".to_string(),
            ]
        } else {
            vec!["gpt-5.4".to_string()]
        }
    }

    fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        if *self.logged_in.lock().unwrap() {
            vec![
                crate::provider::ModelRoute {
                    model: "claude-opus-4.6".to_string(),
                    provider: "Copilot".to_string(),
                    api_method: "copilot".to_string(),
                    available: true,
                    detail: String::new(),
                    cheapness: None,
                },
                crate::provider::ModelRoute {
                    model: "grok-code-fast-1".to_string(),
                    provider: "Copilot".to_string(),
                    api_method: "copilot".to_string(),
                    available: true,
                    detail: String::new(),
                    cheapness: None,
                },
            ]
        } else {
            vec![crate::provider::ModelRoute {
                model: "gpt-5.4".to_string(),
                provider: "OpenAI".to_string(),
                api_method: "openai-oauth".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            }]
        }
    }

    fn on_auth_changed(&self) {
        *self.logged_in.lock().unwrap() = true;
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[derive(Clone)]
struct AsyncAuthRefreshingMockProvider {
    started: StdArc<AtomicBool>,
    completed: StdArc<AtomicBool>,
    delay: Duration,
}

#[async_trait::async_trait]
impl Provider for AsyncAuthRefreshingMockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("AsyncAuthRefreshingMockProvider")
    }

    fn name(&self) -> &str {
        "async-auth-refresh-mock"
    }

    fn on_auth_changed(&self) {
        self.started.store(true, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        self.completed.store(true, Ordering::SeqCst);
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn create_auth_refresh_test_app() -> App {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let provider: Arc<dyn Provider> = Arc::new(AuthRefreshingMockProvider {
        logged_in: StdArc::new(StdMutex::new(false)),
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app
}

#[derive(Clone)]
struct AntigravityMockProvider {
    model: StdArc<StdMutex<String>>,
}

#[async_trait::async_trait]
impl Provider for AntigravityMockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("AntigravityMockProvider")
    }

    fn name(&self) -> &str {
        "Antigravity"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        let resolved = model
            .strip_prefix("antigravity:")
            .unwrap_or(model)
            .to_string();
        *self.model.lock().unwrap() = resolved;
        Ok(())
    }

    fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        vec![
            crate::provider::ModelRoute {
                model: "claude-sonnet-4-6".to_string(),
                provider: "Antigravity".to_string(),
                api_method: "cli".to_string(),
                available: true,
                detail: "cached catalog".to_string(),
                cheapness: None,
            },
            crate::provider::ModelRoute {
                model: "gpt-oss-120b-medium".to_string(),
                provider: "Antigravity".to_string(),
                api_method: "cli".to_string(),
                available: true,
                detail: "cached catalog".to_string(),
                cheapness: None,
            },
        ]
    }

    fn available_models_display(&self) -> Vec<String> {
        vec![
            "claude-sonnet-4-6".to_string(),
            "gpt-oss-120b-medium".to_string(),
        ]
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn create_antigravity_picker_test_app() -> App {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let provider: Arc<dyn Provider> = Arc::new(AntigravityMockProvider {
        model: StdArc::new(StdMutex::new("default".to_string())),
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app
}

fn render_model_picker_text(app: &mut App, width: u16, height: u16) -> String {
    let _render_lock = scroll_render_test_lock();
    if app.display_messages.is_empty() {
        app.display_messages = vec![DisplayMessage::system("seed render state")];
        app.bump_display_messages_version();
    }
    app.open_model_picker();
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create test terminal");
    render_and_snap(app, &mut terminal)
}

#[derive(Clone)]
struct FailingModelSwitchProvider;

#[async_trait::async_trait]
impl Provider for FailingModelSwitchProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("FailingModelSwitchProvider")
    }

    fn name(&self) -> &str {
        "failing-model-switch"
    }

    fn model(&self) -> String {
        "gpt-5.4".to_string()
    }

    fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        vec![crate::provider::ModelRoute {
            model: "claude-opus-4.6".to_string(),
            provider: "Copilot".to_string(),
            api_method: "copilot".to_string(),
            available: true,
            detail: String::new(),
            cheapness: None,
        }]
    }

    fn set_model(&self, _model: &str) -> Result<()> {
        anyhow::bail!("credentials expired")
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn create_failing_model_switch_test_app() -> App {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let provider: Arc<dyn Provider> = Arc::new(FailingModelSwitchProvider);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app
}

fn write_test_config(contents: &str) {
    let path = crate::config::Config::path().expect("config path");
    std::fs::create_dir_all(path.parent().expect("config dir")).expect("config dir");
    std::fs::write(path, contents).expect("write config");
}

fn failover_error_message(prompt: &crate::provider::ProviderFailoverPrompt) -> String {
    format!(
        "[jcode-provider-failover]{}\nignored",
        serde_json::to_string(prompt).expect("serialize failover prompt")
    )
}

fn create_fast_test_app() -> App {
    let provider: Arc<dyn Provider> = Arc::new(FastMockProvider {
        service_tier: StdArc::new(StdMutex::new(None)),
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app
}

fn create_gemini_test_app() -> App {
    struct GeminiMockProvider;

    #[async_trait::async_trait]
    impl Provider for GeminiMockProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[crate::message::ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<crate::provider::EventStream> {
            unimplemented!("Mock provider")
        }

        fn name(&self) -> &str {
            "gemini"
        }

        fn model(&self) -> String {
            "gemini-2.5-pro".to_string()
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(GeminiMockProvider)
        }
    }

    let provider: Arc<dyn Provider> = Arc::new(GeminiMockProvider);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app
}
