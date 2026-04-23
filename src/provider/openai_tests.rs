use super::*;
use crate::auth::codex::CodexCredentials;
use crate::message::{ContentBlock, Role};
use anyhow::Result;
use futures::{SinkExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
const BRIGHT_PEARL_WRAPPED_TOOL_CALL_FIXTURE: &str =
    include_str!("../../tests/fixtures/openai/bright_pearl_wrapped_tool_call.txt");
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        crate::env::set_var(key, value);
        Self { key, previous }
    }

    fn set_path(key: &'static str, value: &std::path::Path) -> Self {
        let previous = std::env::var_os(key);
        crate::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            crate::env::set_var(self.key, previous);
        } else {
            crate::env::remove_var(self.key);
        }
    }
}

async fn test_persistent_ws_state() -> (PersistentWsState, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test websocket listener");
    let addr = listener.local_addr().expect("listener local addr");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept websocket client");
        let mut ws = tokio_tungstenite::accept_async(stream)
            .await
            .expect("accept websocket handshake");
        while let Some(message) = ws.next().await {
            match message {
                Ok(WsMessage::Ping(payload)) => {
                    let _ = ws.send(WsMessage::Pong(payload)).await;
                }
                Ok(WsMessage::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    let (client_ws, _) = connect_async(format!("ws://{}", addr))
        .await
        .expect("connect websocket client");
    (
        PersistentWsState {
            ws_stream: client_ws,
            last_response_id: "resp_test".to_string(),
            connected_at: Instant::now(),
            last_activity_at: Instant::now(),
            message_count: 1,
            last_input_item_count: 1,
        },
        server,
    )
}

struct LiveOpenAITestEnv {
    _lock: MutexGuard<'static, ()>,
    _jcode_home: EnvVarGuard,
    _transport: EnvVarGuard,
    _temp: tempfile::TempDir,
}

impl LiveOpenAITestEnv {
    fn new() -> Result<Option<Self>> {
        let lock = ENV_LOCK.lock().unwrap();
        let Some(source_auth) = real_codex_auth_path() else {
            return Ok(None);
        };

        let temp = tempfile::Builder::new()
            .prefix("jcode-openai-live-")
            .tempdir()?;
        let target_auth = temp
            .path()
            .join("external")
            .join(".codex")
            .join("auth.json");
        std::fs::create_dir_all(
            target_auth
                .parent()
                .expect("temp auth target should have a parent"),
        )?;
        std::fs::copy(source_auth, &target_auth)?;

        let jcode_home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
        let transport = EnvVarGuard::set("JCODE_OPENAI_TRANSPORT", "https");

        Ok(Some(Self {
            _lock: lock,
            _jcode_home: jcode_home,
            _transport: transport,
            _temp: temp,
        }))
    }
}

fn real_codex_auth_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let path = home.join(".codex").join("auth.json");
    path.exists().then_some(path)
}

async fn live_openai_catalog() -> Result<Option<crate::provider::OpenAIModelCatalog>> {
    let Some(_env) = LiveOpenAITestEnv::new()? else {
        return Ok(None);
    };
    let creds = crate::auth::codex::load_credentials()?;
    if !OpenAIProvider::is_chatgpt_mode(&creds) {
        return Ok(None);
    }

    let token = openai_access_token(&Arc::new(RwLock::new(creds))).await?;
    Ok(Some(
        crate::provider::fetch_openai_model_catalog(&token).await?,
    ))
}

async fn live_openai_smoke(model: &str, sentinel: &str) -> Result<Option<String>> {
    let Some(_env) = LiveOpenAITestEnv::new()? else {
        return Ok(None);
    };
    let creds = crate::auth::codex::load_credentials()?;
    if !OpenAIProvider::is_chatgpt_mode(&creds) {
        return Ok(None);
    }

    let provider = OpenAIProvider::new(creds);
    provider.set_model(model)?;
    let response = provider
        .complete_simple(&format!("Reply with exactly {}.", sentinel), "")
        .await?;
    Ok(Some(response))
}

#[test]
fn test_openai_supports_codex_models() {
    let _guard = crate::storage::lock_test_env();
    crate::auth::codex::set_active_account_override(Some(
        "openai-supports-codex-models".to_string(),
    ));
    crate::provider::populate_account_models(vec![
        "gpt-5.1-codex".to_string(),
        "gpt-5.1-codex-mini".to_string(),
        "gpt-5.2-codex".to_string(),
    ]);

    let creds = CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    };

    let provider = OpenAIProvider::new(creds);
    assert!(provider.available_models().contains(&"gpt-5.2-codex"));
    assert!(provider.available_models().contains(&"gpt-5.1-codex-mini"));

    provider.set_model("gpt-5.1-codex").unwrap();
    assert_eq!(provider.model(), "gpt-5.1-codex");

    provider.set_model("gpt-5.1-codex-mini").unwrap();
    assert_eq!(provider.model(), "gpt-5.1-codex-mini");

    crate::auth::codex::set_active_account_override(None);
}

#[test]
fn test_openai_switching_models_include_dynamic_catalog_entries() {
    let _guard = crate::storage::lock_test_env();
    let dynamic_model = "gpt-5.9-switching-test";
    crate::auth::codex::set_active_account_override(Some("switching-test".to_string()));
    crate::provider::populate_account_models(vec![
        "gpt-5.4".to_string(),
        dynamic_model.to_string(),
    ]);

    let provider = OpenAIProvider::new(CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    });

    let models = provider.available_models_for_switching();
    assert!(models.contains(&"gpt-5.4".to_string()));
    assert!(models.contains(&dynamic_model.to_string()));

    crate::auth::codex::set_active_account_override(None);
}

#[test]
fn test_summarize_ws_input_counts_tool_outputs() {
    let items = vec![
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "hello"}]
        }),
        serde_json::json!({
            "type": "function_call",
            "call_id": "call_1",
            "name": "bash",
            "arguments": "{}"
        }),
        serde_json::json!({
            "type": "function_call_output",
            "call_id": "call_1",
            "output": "ok"
        }),
        serde_json::json!({"type": "unknown"}),
    ];

    assert_eq!(
        summarize_ws_input(&items),
        WsInputStats {
            total_items: 4,
            message_items: 1,
            function_call_items: 1,
            function_call_output_items: 1,
            other_items: 1,
        }
    );
}

#[test]
fn test_persistent_ws_idle_policy_thresholds() {
    assert!(!persistent_ws_idle_needs_healthcheck(Duration::from_secs(
        5
    )));
    assert!(persistent_ws_idle_needs_healthcheck(Duration::from_secs(
        WEBSOCKET_PERSISTENT_HEALTHCHECK_IDLE_SECS
    )));
    assert!(!persistent_ws_idle_requires_reconnect(Duration::from_secs(
        30
    )));
    assert!(persistent_ws_idle_requires_reconnect(Duration::from_secs(
        WEBSOCKET_PERSISTENT_IDLE_RECONNECT_SECS
    )));
}

#[tokio::test]
#[allow(
    clippy::await_holding_lock,
    reason = "test intentionally serializes process-wide active OpenAI account model cache across async websocket state setup"
)]
async fn test_set_model_clears_persistent_ws_state() {
    let _guard = crate::storage::lock_test_env();
    crate::auth::codex::set_active_account_override(Some("openai-set-model-clears-ws".to_string()));
    crate::provider::populate_account_models(vec!["gpt-5.3-codex".to_string()]);

    let provider = OpenAIProvider::new(CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    });
    let (state, server) = test_persistent_ws_state().await;
    *provider.persistent_ws.lock().await = Some(state);

    provider.set_model("gpt-5.3-codex").expect("set model");

    assert!(
        provider.persistent_ws.lock().await.is_none(),
        "changing models should reset the persistent websocket chain"
    );
    server.abort();
    crate::auth::codex::set_active_account_override(None);
}

#[tokio::test]
async fn test_switching_to_https_clears_persistent_ws_state() {
    let provider = OpenAIProvider::new(CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    });
    let (state, server) = test_persistent_ws_state().await;
    *provider.persistent_ws.lock().await = Some(state);

    provider
        .set_transport("https")
        .expect("switch transport to https");

    assert!(
        provider.persistent_ws.lock().await.is_none(),
        "switching to HTTPS should drop the websocket continuation chain"
    );
    server.abort();
}

#[test]
fn test_service_tier_can_be_changed_while_a_request_snapshot_is_held() {
    let provider = Arc::new(OpenAIProvider::new(CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    }));

    let read_guard = provider
        .service_tier
        .read()
        .expect("service tier read lock should be available");

    let (tx, rx) = std::sync::mpsc::channel();
    let provider_for_write = Arc::clone(&provider);
    let handle = std::thread::spawn(move || {
        let result = provider_for_write.set_service_tier("priority");
        tx.send(result).expect("send result from setter thread");
    });

    std::thread::sleep(Duration::from_millis(20));
    assert!(
        rx.try_recv().is_err(),
        "writer should wait for the in-flight snapshot to finish"
    );

    drop(read_guard);

    rx.recv()
        .expect("receive service tier setter result")
        .expect("service tier update should succeed once read lock is released");
    handle.join().expect("join setter thread");

    assert_eq!(provider.service_tier(), Some("priority".to_string()));
}

#[test]
fn test_build_responses_input_injects_missing_tool_output() {
    let expected_missing = format!("[Error] {}", TOOL_OUTPUT_MISSING_TEXT);
    let messages = vec![
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let items = build_responses_input(&messages);
    let mut saw_call = false;
    let mut saw_output = false;

    for item in &items {
        let item_type = item.get("type").and_then(|v| v.as_str());
        match item_type {
            Some("function_call") => {
                if item.get("call_id").and_then(|v| v.as_str()) == Some("call_1") {
                    saw_call = true;
                }
            }
            Some("function_call_output") => {
                if item.get("call_id").and_then(|v| v.as_str()) == Some("call_1") {
                    let output = item.get("output").and_then(|v| v.as_str());
                    assert_eq!(output, Some(expected_missing.as_str()));
                    saw_output = true;
                }
            }
            _ => {}
        }
    }

    assert!(saw_call);
    assert!(saw_output);
}

#[test]
fn test_build_responses_input_preserves_tool_output() {
    let messages = vec![
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        ChatMessage::tool_result("call_1", "ok", false),
    ];

    let items = build_responses_input(&messages);
    let mut outputs = Vec::new();

    for item in &items {
        if item.get("type").and_then(|v| v.as_str()) == Some("function_call_output")
            && item.get("call_id").and_then(|v| v.as_str()) == Some("call_1")
            && let Some(output) = item.get("output").and_then(|v| v.as_str())
        {
            outputs.push(output.to_string());
        }
    }

    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0], "ok");
}

#[test]
fn test_build_responses_input_reorders_early_tool_output() {
    let messages = vec![
        ChatMessage::tool_result("call_1", "ok", false),
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let items = build_responses_input(&messages);
    let mut call_pos = None;
    let mut output_pos = None;
    let mut outputs = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let item_type = item.get("type").and_then(|v| v.as_str());
        match item_type {
            Some("function_call") => {
                if item.get("call_id").and_then(|v| v.as_str()) == Some("call_1") {
                    call_pos = Some(idx);
                }
            }
            Some("function_call_output") => {
                if item.get("call_id").and_then(|v| v.as_str()) == Some("call_1") {
                    output_pos = Some(idx);
                    if let Some(output) = item.get("output").and_then(|v| v.as_str()) {
                        outputs.push(output.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    assert!(call_pos.is_some());
    assert!(output_pos.is_some());
    assert!(output_pos.unwrap() > call_pos.unwrap());
    assert_eq!(outputs, vec!["ok".to_string()]);
}

#[test]
fn test_build_responses_input_keeps_image_context_after_tool_output() {
    let messages = vec![
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"file_path": "screenshot.png"}),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "Image: screenshot.png\nImage sent to model for vision analysis."
                        .to_string(),
                    is_error: None,
                },
                ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: "ZmFrZQ==".to_string(),
                },
                ContentBlock::Text {
                    text:
                        "[Attached image associated with the preceding tool result: screenshot.png]"
                            .to_string(),
                    cache_control: None,
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let items = build_responses_input(&messages);
    let mut output_pos = None;
    let mut image_msg_pos = None;

    for (idx, item) in items.iter().enumerate() {
        match item.get("type").and_then(|v| v.as_str()) {
            Some("function_call_output")
                if item.get("call_id").and_then(|v| v.as_str()) == Some("call_1") =>
            {
                output_pos = Some(idx);
                assert_eq!(
                    item.get("output").and_then(|v| v.as_str()),
                    Some("Image: screenshot.png\nImage sent to model for vision analysis.")
                );
            }
            Some("message") if item.get("role").and_then(|v| v.as_str()) == Some("user") => {
                let Some(content) = item.get("content").and_then(|v| v.as_array()) else {
                    continue;
                };
                let has_image = content
                    .iter()
                    .any(|part| part.get("type").and_then(|v| v.as_str()) == Some("input_image"));
                let has_label = content.iter().any(|part| {
                    part.get("type").and_then(|v| v.as_str()) == Some("input_text")
                        && part
                            .get("text")
                            .and_then(|v| v.as_str())
                            .map(|text| text.contains("screenshot.png"))
                            .unwrap_or(false)
                });
                if has_image && has_label {
                    image_msg_pos = Some(idx);
                }
            }
            _ => {}
        }
    }

    assert!(output_pos.is_some(), "expected function call output item");
    assert!(
        image_msg_pos.is_some(),
        "expected follow-up user image message"
    );
    assert!(
        image_msg_pos.unwrap() > output_pos.unwrap(),
        "image context should stay after the tool output"
    );
}

#[test]
fn test_build_responses_input_injects_only_missing_outputs() {
    let expected_missing = format!("[Error] {}", TOOL_OUTPUT_MISSING_TEXT);
    let messages = vec![
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_a".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "pwd"}),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_b".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "whoami"}),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        ChatMessage::tool_result("call_b", "done", false),
    ];

    let items = build_responses_input(&messages);
    let mut output_a = None;
    let mut output_b = None;

    for item in &items {
        if item.get("type").and_then(|v| v.as_str()) == Some("function_call_output") {
            match item.get("call_id").and_then(|v| v.as_str()) {
                Some("call_a") => {
                    output_a = item
                        .get("output")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string());
                }
                Some("call_b") => {
                    output_b = item
                        .get("output")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string());
                }
                _ => {}
            }
        }
    }

    assert_eq!(output_a.as_deref(), Some(expected_missing.as_str()));
    assert_eq!(output_b.as_deref(), Some("done"));
}

#[test]
fn test_openai_retryable_error_patterns() {
    assert!(is_retryable_error(
        "stream disconnected before completion: transport error"
    ));
    assert!(is_retryable_error(
        "falling back from websockets to https transport. stream disconnected before completion"
    ));
    assert!(is_retryable_error(
        "OpenAI HTTPS stream ended before message completion marker"
    ));
}

#[test]
fn test_parse_max_output_tokens_defaults_to_safe_value() {
    assert_eq!(
        OpenAIProvider::parse_max_output_tokens(None),
        Some(DEFAULT_MAX_OUTPUT_TOKENS)
    );
    assert_eq!(
        OpenAIProvider::parse_max_output_tokens(Some("")),
        Some(DEFAULT_MAX_OUTPUT_TOKENS)
    );
}

#[test]
fn test_parse_max_output_tokens_allows_disable_and_override() {
    assert_eq!(OpenAIProvider::parse_max_output_tokens(Some("0")), None);
    assert_eq!(
        OpenAIProvider::parse_max_output_tokens(Some("32768")),
        Some(32768)
    );
    assert_eq!(
        OpenAIProvider::parse_max_output_tokens(Some("not-a-number")),
        Some(DEFAULT_MAX_OUTPUT_TOKENS)
    );
}

#[test]
fn test_build_response_request_for_gpt_5_4_1m_uses_base_model_without_extra_flags() {
    let request = OpenAIProvider::build_response_request(
        "gpt-5.4",
        "system".to_string(),
        &[],
        &[],
        true,
        Some(DEFAULT_MAX_OUTPUT_TOKENS),
        Some("xhigh"),
        Some("unused"),
        Some("unused"),
        None,
        None,
    );

    assert_eq!(request["model"], serde_json::json!("gpt-5.4"));
    assert!(request.get("model_context_window").is_none());
    assert!(request.get("max_output_tokens").is_none());
    assert!(request.get("prompt_cache_key").is_none());
    assert!(request.get("prompt_cache_retention").is_none());
}

#[test]
fn test_build_response_request_omits_long_context_for_plain_gpt_5_4() {
    let request = OpenAIProvider::build_response_request(
        "gpt-5.4",
        "system".to_string(),
        &[],
        &[],
        true,
        Some(DEFAULT_MAX_OUTPUT_TOKENS),
        None,
        None,
        None,
        None,
        None,
    );

    assert!(request.get("model_context_window").is_none());
}

#[tokio::test]
#[ignore = "requires real OpenAI OAuth credentials"]
async fn live_openai_catalog_lists_gpt_5_4_family() -> Result<()> {
    let Some(catalog) = live_openai_catalog().await? else {
        eprintln!("skipping live OpenAI catalog test: no real OAuth credentials");
        return Ok(());
    };

    crate::provider::populate_context_limits(catalog.context_limits.clone());
    crate::provider::populate_account_models(catalog.available_models.clone());

    assert!(
        catalog
            .available_models
            .iter()
            .any(|model| model.starts_with("gpt-5.4")),
        "expected GPT-5.4 family in live catalog, got {:?}",
        catalog.available_models
    );
    assert!(
        crate::provider::known_openai_model_ids()
            .iter()
            .any(|model| model == "gpt-5.4"),
        "expected GPT-5.4 in display model list"
    );

    let reports_long_context = catalog
        .context_limits
        .get("gpt-5.4")
        .copied()
        .unwrap_or_default()
        >= 1_000_000;
    assert_eq!(
        crate::provider::known_openai_model_ids()
            .iter()
            .any(|model| model == "gpt-5.4[1m]"),
        reports_long_context,
        "displayed 1m alias should follow the live catalog"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires real OpenAI OAuth credentials"]
async fn live_openai_gpt_5_4_and_fast_requests_succeed() -> Result<()> {
    let Some(catalog) = live_openai_catalog().await? else {
        eprintln!("skipping live OpenAI response test: no real OAuth credentials");
        return Ok(());
    };
    crate::provider::populate_context_limits(catalog.context_limits.clone());
    crate::provider::populate_account_models(catalog.available_models.clone());

    let Some(plain_response) = live_openai_smoke("gpt-5.4", "JCODE_GPT54_OK").await? else {
        eprintln!("skipping live OpenAI response test: no real OAuth credentials");
        return Ok(());
    };
    assert!(
        plain_response.contains("JCODE_GPT54_OK"),
        "unexpected GPT-5.4 response: {}",
        plain_response
    );

    if catalog
        .available_models
        .iter()
        .any(|model| model == "gpt-5.3-codex-spark")
    {
        let Some(fast_response) =
            live_openai_smoke("gpt-5.3-codex-spark", "JCODE_GPT53_SPARK_OK").await?
        else {
            eprintln!("skipping live OpenAI fast-model test: no real OAuth credentials");
            return Ok(());
        };
        assert!(
            fast_response.contains("JCODE_GPT53_SPARK_OK"),
            "unexpected gpt-5.3-codex-spark response: {}",
            fast_response
        );
    }

    if crate::provider::known_openai_model_ids()
        .iter()
        .any(|model| model == "gpt-5.4[1m]")
    {
        let Some(long_context_response) =
            live_openai_smoke("gpt-5.4[1m]", "JCODE_GPT54_1M_OK").await?
        else {
            eprintln!("skipping live OpenAI 1m test: no real OAuth credentials");
            return Ok(());
        };
        assert!(
            long_context_response.contains("JCODE_GPT54_1M_OK"),
            "unexpected GPT-5.4[1m] response: {}",
            long_context_response
        );
    }

    Ok(())
}

#[test]
fn test_should_prefer_websocket_enabled_for_named_models() {
    assert!(OpenAIProvider::should_prefer_websocket(
        "gpt-5.3-codex-spark"
    ));
    assert!(OpenAIProvider::should_prefer_websocket("gpt-5.3-codex"));
    assert!(OpenAIProvider::should_prefer_websocket("gpt-5"));
    assert!(OpenAIProvider::should_prefer_websocket("codex-mini"));
    assert!(!OpenAIProvider::should_prefer_websocket(""));
}

#[test]
fn test_openai_transport_mode_defaults_to_auto() {
    let mode = OpenAITransportMode::from_config(None);
    assert_eq!(mode.as_str(), "auto");
}

#[test]
fn test_openai_transport_mode_auto_prefers_websocket_for_openai_models() {
    let mode = OpenAITransportMode::from_config(Some("auto"));
    assert_eq!(mode.as_str(), "auto");
    assert!(OpenAIProvider::should_prefer_websocket("gpt-5.4"));
}

#[tokio::test]
async fn test_record_websocket_fallback_sets_cooldown_for_auto_default_models() {
    let cooldowns = Arc::new(RwLock::new(HashMap::new()));
    let streaks = Arc::new(RwLock::new(HashMap::new()));
    let model = "gpt-5.4";

    let (streak, cooldown) = record_websocket_fallback(
        &cooldowns,
        &streaks,
        model,
        WebsocketFallbackReason::StreamTimeout,
    )
    .await;
    assert_eq!(streak, 1);
    assert_eq!(
        cooldown,
        Duration::from_secs(WEBSOCKET_MODEL_COOLDOWN_BASE_SECS)
    );
    assert!(
        websocket_cooldown_remaining(&cooldowns, model)
            .await
            .is_some(),
        "auto websocket default must still be guarded by cooldown after fallback"
    );
}

#[tokio::test]
async fn test_websocket_cooldown_helpers_set_clear_and_expire() {
    let cooldowns = Arc::new(RwLock::new(HashMap::new()));
    let model = "gpt-5.3-codex";

    assert!(
        websocket_cooldown_remaining(&cooldowns, model)
            .await
            .is_none()
    );

    set_websocket_cooldown(&cooldowns, model).await;
    let remaining = websocket_cooldown_remaining(&cooldowns, model).await;
    assert!(remaining.is_some());

    clear_websocket_cooldown(&cooldowns, model).await;
    assert!(
        websocket_cooldown_remaining(&cooldowns, model)
            .await
            .is_none()
    );

    {
        let mut guard = cooldowns.write().await;
        guard.insert(model.to_string(), Instant::now() - Duration::from_secs(1));
    }
    assert!(
        websocket_cooldown_remaining(&cooldowns, model)
            .await
            .is_none()
    );
    assert!(!cooldowns.read().await.contains_key(model));
}

#[test]
fn test_websocket_cooldown_for_streak_scales_and_caps() {
    assert_eq!(
        websocket_cooldown_for_streak(1, WebsocketFallbackReason::StreamTimeout),
        Duration::from_secs(WEBSOCKET_MODEL_COOLDOWN_BASE_SECS)
    );
    assert_eq!(
        websocket_cooldown_for_streak(2, WebsocketFallbackReason::StreamTimeout),
        Duration::from_secs(WEBSOCKET_MODEL_COOLDOWN_BASE_SECS * 2)
    );
    assert_eq!(
        websocket_cooldown_for_streak(3, WebsocketFallbackReason::StreamTimeout),
        Duration::from_secs(WEBSOCKET_MODEL_COOLDOWN_BASE_SECS * 4)
    );
    assert_eq!(
        websocket_cooldown_for_streak(32, WebsocketFallbackReason::StreamTimeout),
        Duration::from_secs(WEBSOCKET_MODEL_COOLDOWN_MAX_SECS)
    );
}

#[test]
fn test_websocket_cooldown_for_reason_adjusts_by_failure_type() {
    assert_eq!(
        websocket_cooldown_for_streak(1, WebsocketFallbackReason::ConnectTimeout),
        Duration::from_secs((WEBSOCKET_MODEL_COOLDOWN_BASE_SECS / 2).max(1))
    );
    assert_eq!(
        websocket_cooldown_for_streak(1, WebsocketFallbackReason::ServerRequestedHttps),
        Duration::from_secs(WEBSOCKET_MODEL_COOLDOWN_BASE_SECS * 5)
    );
    assert_eq!(
        websocket_cooldown_for_streak(32, WebsocketFallbackReason::ServerRequestedHttps),
        Duration::from_secs(WEBSOCKET_MODEL_COOLDOWN_MAX_SECS * 3)
    );
}

#[tokio::test]
async fn test_record_websocket_fallback_tracks_streak_and_cooldown() {
    let cooldowns = Arc::new(RwLock::new(HashMap::new()));
    let streaks = Arc::new(RwLock::new(HashMap::new()));
    let model = "gpt-5.3-codex-spark";

    let (streak1, cooldown1) = record_websocket_fallback(
        &cooldowns,
        &streaks,
        model,
        WebsocketFallbackReason::StreamTimeout,
    )
    .await;
    assert_eq!(streak1, 1);
    assert_eq!(
        cooldown1,
        Duration::from_secs(WEBSOCKET_MODEL_COOLDOWN_BASE_SECS)
    );
    let remaining1 = websocket_cooldown_remaining(&cooldowns, model)
        .await
        .expect("cooldown should be set");
    assert!(remaining1 <= cooldown1);

    let (streak2, cooldown2) = record_websocket_fallback(
        &cooldowns,
        &streaks,
        model,
        WebsocketFallbackReason::StreamTimeout,
    )
    .await;
    assert_eq!(streak2, 2);
    assert_eq!(
        cooldown2,
        Duration::from_secs(WEBSOCKET_MODEL_COOLDOWN_BASE_SECS * 2)
    );
    let remaining2 = websocket_cooldown_remaining(&cooldowns, model)
        .await
        .expect("cooldown should be set");
    assert!(remaining2 <= cooldown2);

    record_websocket_success(&cooldowns, &streaks, model).await;
    assert!(
        websocket_cooldown_remaining(&cooldowns, model)
            .await
            .is_none()
    );
    let normalized = normalize_transport_model(model).expect("normalized model");
    assert!(!streaks.read().await.contains_key(&normalized));
}

#[test]
fn test_websocket_activity_payload_detection() {
    assert!(is_websocket_activity_payload(
        r#"{"type":"response.created","response":{"id":"resp_1"}}"#
    ));
    assert!(is_websocket_activity_payload(
        r#"{"type":"response.reasoning.delta","delta":"thinking"}"#
    ));
    assert!(!is_websocket_activity_payload("not json"));
    assert!(!is_websocket_activity_payload(r#"{"foo":"bar"}"#));
}

#[test]
fn test_websocket_first_activity_payload_counts_typed_control_events() {
    assert!(is_websocket_first_activity_payload(
        r#"{"type":"rate_limits.updated"}"#
    ));
    assert!(is_websocket_first_activity_payload(
        r#"{"type":"session.created","session":{}}"#
    ));
    assert!(!is_websocket_first_activity_payload(r#"{"foo":"bar"}"#));
    assert!(!is_websocket_first_activity_payload("not json"));
}

#[test]
fn test_websocket_completion_timeout_is_long_enough_for_reasoning() {
    let timeout = std::hint::black_box(WEBSOCKET_COMPLETION_TIMEOUT_SECS);
    assert!(
        timeout >= 120,
        "completion timeout regressed to {}s; reasoning models may need several minutes",
        timeout
    );
}

#[test]
fn test_stream_activity_event_treats_any_stream_event_as_activity() {
    assert!(is_stream_activity_event(&StreamEvent::ThinkingStart));
    assert!(is_stream_activity_event(&StreamEvent::ThinkingDelta(
        "working".to_string()
    )));
    assert!(is_stream_activity_event(&StreamEvent::TextDelta(
        "hello".to_string()
    )));
    assert!(is_stream_activity_event(&StreamEvent::MessageEnd {
        stop_reason: None
    }));
}

#[test]
fn test_websocket_activity_payload_counts_response_completed() {
    assert!(is_websocket_activity_payload(
        r#"{"type":"response.completed","response":{"status":"completed"}}"#
    ));
}

#[test]
fn test_websocket_activity_payload_counts_in_progress_events() {
    assert!(is_websocket_activity_payload(
        r#"{"type":"response.in_progress","response":{"status":"in_progress"}}"#
    ));
}

#[test]
fn test_websocket_activity_payload_ignores_non_response_events() {
    assert!(!is_websocket_activity_payload(
        r#"{"type":"session.created","session":{}}"#
    ));
    assert!(!is_websocket_activity_payload(
        r#"{"type":"rate_limits.updated"}"#
    ));
    assert!(!is_websocket_activity_payload(r#"not json at all"#));
}

#[test]
fn test_websocket_remaining_timeout_secs_uses_idle_time_budget() {
    let recent = Instant::now() - Duration::from_secs(2);
    let remaining = websocket_remaining_timeout_secs(recent, 8).expect("still within budget");
    assert!(
        (6..=7).contains(&remaining),
        "expected remaining idle budget near 6-7s, got {remaining}"
    );
}

#[test]
fn test_websocket_remaining_timeout_secs_expires_after_budget() {
    let expired = Instant::now() - Duration::from_secs(9);
    assert!(websocket_remaining_timeout_secs(expired, 8).is_none());
}

#[test]
fn test_websocket_next_activity_timeout_uses_request_start_before_first_event() {
    let ws_started_at = Instant::now() - Duration::from_secs(3);
    let last_api_activity_at = Instant::now() - Duration::from_secs(1);
    let remaining =
        websocket_next_activity_timeout_secs(ws_started_at, last_api_activity_at, false)
            .expect("first-event timeout should still be active");
    assert!(
        (5..=6).contains(&remaining),
        "expected first-event timeout near 5-6s, got {remaining}"
    );
}

#[test]
fn test_websocket_next_activity_timeout_resets_after_api_activity() {
    let ws_started_at = Instant::now() - Duration::from_secs(299);
    let last_api_activity_at = Instant::now() - Duration::from_secs(2);
    let remaining = websocket_next_activity_timeout_secs(ws_started_at, last_api_activity_at, true)
        .expect("idle timeout should use last activity, not total request age");
    assert!(
        remaining >= WEBSOCKET_COMPLETION_TIMEOUT_SECS.saturating_sub(3),
        "expected full idle budget to reset after activity, got {remaining}"
    );
}

#[test]
fn test_websocket_activity_timeout_kind_labels_first_and_next() {
    assert_eq!(websocket_activity_timeout_kind(false), "first");
    assert_eq!(websocket_activity_timeout_kind(true), "next");
}

#[test]
fn test_format_status_duration_uses_compact_human_labels() {
    assert_eq!(format_status_duration(Duration::from_secs(9)), "9s");
    assert_eq!(format_status_duration(Duration::from_secs(125)), "2m 5s");
    assert_eq!(format_status_duration(Duration::from_secs(7260)), "2h 1m");
}

#[test]
fn test_summarize_websocket_fallback_reason_classifies_common_failures() {
    assert_eq!(
        summarize_websocket_fallback_reason("WebSocket connect timed out after 8s"),
        "connect timeout"
    );
    assert_eq!(
        summarize_websocket_fallback_reason(
            "WebSocket stream timed out waiting for first websocket activity (8s)"
        ),
        "first response timeout"
    );
    assert_eq!(
        summarize_websocket_fallback_reason(
            "WebSocket stream timed out waiting for next websocket activity (300s)"
        ),
        "stream timeout"
    );
    assert_eq!(
        summarize_websocket_fallback_reason("server requested fallback"),
        "server requested https"
    );
    assert_eq!(
        summarize_websocket_fallback_reason("WebSocket stream closed before response.completed"),
        "stream closed early"
    );
}

#[test]
fn test_normalize_transport_model_trims_and_lowercases() {
    assert_eq!(
        normalize_transport_model("  GPT-5.4  "),
        Some("gpt-5.4".to_string())
    );
    assert_eq!(normalize_transport_model("   \t\n  "), None);
}

#[tokio::test]
async fn test_record_websocket_success_clears_normalized_keys() {
    let cooldowns = Arc::new(RwLock::new(HashMap::new()));
    let streaks = Arc::new(RwLock::new(HashMap::new()));
    let canonical = "gpt-5.4";

    record_websocket_fallback(
        &cooldowns,
        &streaks,
        canonical,
        WebsocketFallbackReason::StreamTimeout,
    )
    .await;
    assert!(
        websocket_cooldown_remaining(&cooldowns, canonical)
            .await
            .is_some()
    );

    record_websocket_success(&cooldowns, &streaks, " GPT-5.4 ").await;

    assert!(
        websocket_cooldown_remaining(&cooldowns, canonical)
            .await
            .is_none(),
        "success should clear normalized cooldown entries"
    );
    assert!(
        !streaks.read().await.contains_key(canonical),
        "success should clear normalized failure streak entries"
    );
}

#[test]
fn test_build_response_request_includes_stream_for_http() {
    let request = OpenAIProvider::build_response_request(
        "gpt-5.4",
        "system".to_string(),
        &[],
        &[],
        false,
        Some(DEFAULT_MAX_OUTPUT_TOKENS),
        None,
        None,
        None,
        None,
        None,
    );
    assert_eq!(request["stream"], serde_json::json!(true));
    assert_eq!(request["store"], serde_json::json!(false));
}

#[test]
fn test_websocket_payload_strips_stream_and_background() {
    let mut request = OpenAIProvider::build_response_request(
        "gpt-5.4",
        "system".to_string(),
        &[serde_json::json!({"role": "user", "content": "hello"})],
        &[],
        false,
        Some(DEFAULT_MAX_OUTPUT_TOKENS),
        None,
        None,
        None,
        None,
        None,
    );

    assert_eq!(request["stream"], serde_json::json!(true));

    request["background"] = serde_json::json!(true);

    let obj = request.as_object_mut().expect("request is object");
    obj.insert(
        "type".to_string(),
        serde_json::Value::String("response.create".to_string()),
    );
    obj.remove("stream");
    obj.remove("background");

    assert!(
        request.get("stream").is_none(),
        "stream must be stripped for WebSocket payloads"
    );
    assert!(
        request.get("background").is_none(),
        "background must be stripped for WebSocket payloads"
    );
    assert_eq!(request["type"], serde_json::json!("response.create"));
}

#[test]
fn test_websocket_payload_preserves_required_fields() {
    let mut request = OpenAIProvider::build_response_request(
        "gpt-5.4",
        "system prompt".to_string(),
        &[serde_json::json!({"role": "user", "content": "hello"})],
        &[serde_json::json!({"type": "function", "name": "bash"})],
        false,
        Some(16384),
        Some("high"),
        None,
        None,
        None,
        None,
    );

    let obj = request.as_object_mut().expect("request is object");
    obj.insert(
        "type".to_string(),
        serde_json::Value::String("response.create".to_string()),
    );
    obj.remove("stream");
    obj.remove("background");

    assert_eq!(request["type"], "response.create");
    assert_eq!(request["model"], "gpt-5.4");
    assert_eq!(request["instructions"], "system prompt");
    assert!(request["input"].is_array());
    assert!(request["tools"].is_array());
    assert_eq!(request["max_output_tokens"], serde_json::json!(16384));
    assert_eq!(request["reasoning"], serde_json::json!({"effort": "high"}));
    assert_eq!(request["tool_choice"], "auto");
}

#[test]
fn test_websocket_continuation_request_excludes_transport_fields() {
    let base_request = OpenAIProvider::build_response_request(
        "gpt-5.4",
        "system".to_string(),
        &[],
        &[serde_json::json!({"type": "function", "name": "bash"})],
        false,
        Some(DEFAULT_MAX_OUTPUT_TOKENS),
        None,
        None,
        None,
        None,
        Some(160_000),
    );

    let mut continuation = serde_json::json!({
        "type": "response.create",
        "previous_response_id": "resp_abc123",
        "input": [{"role": "user", "content": "follow up"}],
    });

    if let Some(model) = base_request.get("model") {
        continuation["model"] = model.clone();
    }
    if let Some(tools) = base_request.get("tools") {
        continuation["tools"] = tools.clone();
    }
    if let Some(instructions) = base_request.get("instructions") {
        continuation["instructions"] = instructions.clone();
    }
    if let Some(context_management) = base_request.get("context_management") {
        continuation["context_management"] = context_management.clone();
    }
    continuation["store"] = serde_json::json!(false);
    continuation["parallel_tool_calls"] = serde_json::json!(false);

    assert!(
        continuation.get("stream").is_none(),
        "continuation request must not include stream"
    );
    assert!(
        continuation.get("background").is_none(),
        "continuation request must not include background"
    );
    assert_eq!(continuation["type"], "response.create");
    assert_eq!(continuation["previous_response_id"], "resp_abc123");
    assert_eq!(continuation["model"], "gpt-5.4");
    assert_eq!(
        continuation["context_management"],
        serde_json::json!([
            {
                "type": "compaction",
                "compact_threshold": 160_000,
            }
        ])
    );
}

#[test]
fn test_parse_openai_response_completed_captures_incomplete_stop_reason() {
    let data = r#"{"type":"response.completed","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}"#;
    let mut saw_text_delta = false;
    let mut streaming_tool_calls = HashMap::new();
    let mut completed_tool_items = HashSet::new();
    let mut pending = VecDeque::new();

    let event = parse_openai_response_event(
        data,
        &mut saw_text_delta,
        &mut streaming_tool_calls,
        &mut completed_tool_items,
        &mut pending,
    )
    .expect("expected message end");
    match event {
        StreamEvent::MessageEnd { stop_reason } => {
            assert_eq!(stop_reason.as_deref(), Some("max_output_tokens"));
        }
        other => panic!("expected MessageEnd, got {:?}", other),
    }
}

#[test]
fn test_parse_openai_response_completed_without_stop_reason() {
    let data = r#"{"type":"response.completed","response":{"status":"completed"}}"#;
    let mut saw_text_delta = false;
    let mut streaming_tool_calls = HashMap::new();
    let mut completed_tool_items = HashSet::new();
    let mut pending = VecDeque::new();

    let event = parse_openai_response_event(
        data,
        &mut saw_text_delta,
        &mut streaming_tool_calls,
        &mut completed_tool_items,
        &mut pending,
    )
    .expect("expected message end");
    match event {
        StreamEvent::MessageEnd { stop_reason } => {
            assert!(stop_reason.is_none());
        }
        other => panic!("expected MessageEnd, got {:?}", other),
    }
}

#[test]
fn test_parse_openai_response_completed_commentary_phase_sets_stop_reason() {
    let data = r#"{"type":"response.completed","response":{"status":"completed","output":[{"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"Still working"}]}]}}"#;
    let mut saw_text_delta = false;
    let mut streaming_tool_calls = HashMap::new();
    let mut completed_tool_items = HashSet::new();
    let mut pending = VecDeque::new();

    let event = parse_openai_response_event(
        data,
        &mut saw_text_delta,
        &mut streaming_tool_calls,
        &mut completed_tool_items,
        &mut pending,
    )
    .expect("expected message end");
    match event {
        StreamEvent::MessageEnd { stop_reason } => {
            assert_eq!(stop_reason.as_deref(), Some("commentary"));
        }
        other => panic!("expected MessageEnd, got {:?}", other),
    }
}

#[test]
fn test_parse_openai_response_incomplete_emits_message_end_with_reason() {
    let data = r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"content_filter"}}}"#;
    let mut saw_text_delta = false;
    let mut streaming_tool_calls = HashMap::new();
    let mut completed_tool_items = HashSet::new();
    let mut pending = VecDeque::new();

    let event = parse_openai_response_event(
        data,
        &mut saw_text_delta,
        &mut streaming_tool_calls,
        &mut completed_tool_items,
        &mut pending,
    )
    .expect("expected message end");
    match event {
        StreamEvent::MessageEnd { stop_reason } => {
            assert_eq!(stop_reason.as_deref(), Some("content_filter"));
        }
        other => panic!("expected MessageEnd, got {:?}", other),
    }
}

#[test]
fn test_parse_openai_response_function_call_arguments_streaming() {
    let mut saw_text_delta = false;
    let mut streaming_tool_calls = HashMap::new();
    let mut completed_tool_items = HashSet::new();
    let mut pending = VecDeque::new();

    let added = r#"{"type":"response.output_item.added","item":{"id":"fc_123","type":"function_call","call_id":"call_123","name":"batch","arguments":""}}"#;
    assert!(
        parse_openai_response_event(
            added,
            &mut saw_text_delta,
            &mut streaming_tool_calls,
            &mut completed_tool_items,
            &mut pending,
        )
        .is_none(),
        "output_item.added should just seed tool state"
    );

    let delta = r#"{"type":"response.function_call_arguments.delta","item_id":"fc_123","delta":"{\"tool_calls\":[{\"tool\":\"read\"}]"}"#;
    assert!(
        parse_openai_response_event(
            delta,
            &mut saw_text_delta,
            &mut streaming_tool_calls,
            &mut completed_tool_items,
            &mut pending,
        )
        .is_none(),
        "argument delta should accumulate state only"
    );

    let done = r#"{"type":"response.function_call_arguments.done","item_id":"fc_123","arguments":"{\"tool_calls\":[{\"tool\":\"read\"}]}"}"#;
    let first = parse_openai_response_event(
        done,
        &mut saw_text_delta,
        &mut streaming_tool_calls,
        &mut completed_tool_items,
        &mut pending,
    )
    .expect("expected tool start");

    match first {
        StreamEvent::ToolUseStart { id, name } => {
            assert_eq!(id, "call_123");
            assert_eq!(name, "batch");
        }
        other => panic!("expected ToolUseStart, got {:?}", other),
    }

    match pending.pop_front() {
        Some(StreamEvent::ToolInputDelta(delta)) => {
            let parsed: Value = serde_json::from_str(&delta).expect("valid args json");
            let tool_calls = parsed
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .expect("tool_calls array");
            assert_eq!(tool_calls.len(), 1);
        }
        other => panic!("expected ToolInputDelta, got {:?}", other),
    }

    assert!(matches!(pending.pop_front(), Some(StreamEvent::ToolUseEnd)));
    assert!(streaming_tool_calls.is_empty());
    assert!(completed_tool_items.contains("fc_123"));
}

#[test]
fn test_parse_openai_response_output_item_done_skips_duplicate_after_arguments_done() {
    let mut saw_text_delta = false;
    let mut streaming_tool_calls = HashMap::new();
    let mut completed_tool_items = HashSet::from(["fc_123".to_string()]);
    let mut pending = VecDeque::new();

    let duplicate_done = r#"{"type":"response.output_item.done","item":{"id":"fc_123","type":"function_call","call_id":"call_123","name":"batch","arguments":"{\"tool_calls\":[]}"}}"#;
    let event = parse_openai_response_event(
        duplicate_done,
        &mut saw_text_delta,
        &mut streaming_tool_calls,
        &mut completed_tool_items,
        &mut pending,
    );

    assert!(event.is_none(), "duplicate function call should be skipped");
    assert!(pending.is_empty());
    assert!(!completed_tool_items.contains("fc_123"));
}

#[test]
fn test_parse_openai_response_output_item_done_emits_native_compaction() {
    let mut saw_text_delta = false;
    let mut streaming_tool_calls = HashMap::new();
    let mut completed_tool_items = HashSet::new();
    let mut pending = VecDeque::new();

    let compaction_done = r#"{"type":"response.output_item.done","item":{"id":"cmp_123","type":"compaction","encrypted_content":"enc_abc"}}"#;
    let event = parse_openai_response_event(
        compaction_done,
        &mut saw_text_delta,
        &mut streaming_tool_calls,
        &mut completed_tool_items,
        &mut pending,
    )
    .expect("expected compaction event");

    match event {
        StreamEvent::Compaction {
            trigger,
            pre_tokens,
            openai_encrypted_content,
        } => {
            assert_eq!(trigger, "openai_native_auto");
            assert_eq!(pre_tokens, None);
            assert_eq!(openai_encrypted_content.as_deref(), Some("enc_abc"));
        }
        other => panic!("expected Compaction, got {:?}", other),
    }
    assert!(pending.is_empty());
}

#[test]
fn test_build_tools_sets_strict_true() {
    let defs = vec![ToolDefinition {
        name: "bash".to_string(),
        description: "run shell".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["command"],
            "properties": { "command": { "type": "string" } }
        }),
    }];
    let api_tools = build_tools(&defs);
    assert_eq!(api_tools.len(), 1);
    assert_eq!(api_tools[0]["strict"], serde_json::json!(true));
}

#[test]
fn test_build_tools_disables_strict_for_free_form_object_nodes() {
    let defs = vec![ToolDefinition {
        name: "batch".to_string(),
        description: "batch calls".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["tool_calls"],
            "properties": {
                "tool_calls": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["tool", "parameters"],
                        "properties": {
                            "tool": { "type": "string" },
                            "parameters": { "type": "object" }
                        }
                    }
                }
            }
        }),
    }];
    let api_tools = build_tools(&defs);
    assert_eq!(api_tools.len(), 1);
    assert_eq!(api_tools[0]["strict"], serde_json::json!(false));
    assert_eq!(
        api_tools[0]["parameters"]["properties"]["tool_calls"]["items"]["properties"]["parameters"]
            ["type"],
        serde_json::json!("object")
    );
}

#[test]
fn test_build_tools_normalizes_object_schema_additional_properties() {
    let defs = vec![ToolDefinition {
        name: "edit".to_string(),
        description: "apply edit".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "options": {
                    "type": "object",
                    "properties": {
                        "force": { "type": "boolean" }
                    }
                },
                "description": {
                    "type": "string"
                }
            },
            "required": ["path"]
        }),
    }];
    let api_tools = build_tools(&defs);
    assert_eq!(
        api_tools[0]["parameters"]["additionalProperties"],
        serde_json::json!(false)
    );
    assert_eq!(
        api_tools[0]["parameters"]["properties"]["options"]["additionalProperties"],
        serde_json::json!(false)
    );
    assert_eq!(
        api_tools[0]["parameters"]["required"],
        serde_json::json!(["description", "options", "path"])
    );
    assert_eq!(
        api_tools[0]["parameters"]["properties"]["description"]["type"],
        serde_json::json!(["string", "null"])
    );
}

#[test]
fn test_build_tools_rewrites_oneof_to_anyof_for_openai() {
    let defs = vec![ToolDefinition {
        name: "batch".to_string(),
        description: "batch calls".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["tool_calls"],
            "properties": {
                "tool_calls": {
                    "type": "array",
                    "items": {
                        "oneOf": [
                            {
                                "type": "object",
                                "required": ["tool"],
                                "properties": {
                                    "tool": { "type": "string" }
                                }
                            }
                        ]
                    }
                }
            }
        }),
    }];
    let api_tools = build_tools(&defs);
    assert!(api_tools[0]["parameters"]["properties"]["tool_calls"]["items"]["oneOf"].is_null());
    assert_eq!(
        api_tools[0]["parameters"]["properties"]["tool_calls"]["items"]["anyOf"][0]["type"],
        serde_json::json!("object")
    );
}

#[test]
fn test_build_tools_keeps_strict_for_anyof_object_branches_with_properties() {
    let defs = vec![ToolDefinition {
        name: "schedule".to_string(),
        description: "schedule work".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["task"],
            "anyOf": [
                {
                    "type": "object",
                    "required": ["wake_in_minutes"],
                    "properties": {
                        "wake_in_minutes": { "type": "integer" }
                    },
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "required": ["wake_at"],
                    "properties": {
                        "wake_at": { "type": "string" }
                    },
                    "additionalProperties": false
                }
            ],
            "properties": {
                "task": { "type": "string" },
                "wake_in_minutes": { "type": "integer" },
                "wake_at": { "type": "string" }
            }
        }),
    }];
    let api_tools = build_tools(&defs);
    assert_eq!(api_tools[0]["strict"], serde_json::json!(true));
    assert_eq!(
        api_tools[0]["parameters"]["anyOf"][0]["additionalProperties"],
        serde_json::json!(false)
    );
    assert_eq!(
        api_tools[0]["parameters"]["anyOf"][1]["additionalProperties"],
        serde_json::json!(false)
    );
}

#[test]
fn test_parse_text_wrapped_tool_call_prefers_trailing_json_object() {
    let text = "Status update\nassistant to=functions.batch commentary {}json\n{\"tool_calls\":[{\"tool\":\"read\",\"file_path\":\"src/main.rs\"}]}";
    let parsed = parse_text_wrapped_tool_call(text).expect("should parse wrapped tool call");
    assert_eq!(parsed.1, "batch");
    assert!(parsed.0.contains("Status update"));
    let args: Value = serde_json::from_str(&parsed.2).expect("valid args json");
    assert!(args.get("tool_calls").is_some());
}

#[test]
fn test_handle_openai_output_item_normalizes_null_arguments() {
    let item = serde_json::json!({
        "type": "function_call",
        "call_id": "call_1",
        "name": "bash",
        "arguments": "null",
    });
    let mut saw_text_delta = false;
    let mut pending = VecDeque::new();
    let first = handle_openai_output_item(item, &mut saw_text_delta, &mut pending)
        .expect("expected tool event");

    match first {
        StreamEvent::ToolUseStart { id, name } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "bash");
        }
        _ => panic!("expected ToolUseStart"),
    }
    match pending.pop_front() {
        Some(StreamEvent::ToolInputDelta(delta)) => assert_eq!(delta, "{}"),
        _ => panic!("expected ToolInputDelta"),
    }
    assert!(matches!(pending.pop_front(), Some(StreamEvent::ToolUseEnd)));
}

#[test]
fn test_handle_openai_output_item_recovers_bright_pearl_fixture() {
    let item = serde_json::json!({
        "type": "message",
        "content": [{
            "type": "output_text",
            "text": BRIGHT_PEARL_WRAPPED_TOOL_CALL_FIXTURE,
        }],
    });

    let mut saw_text_delta = false;
    let mut pending = VecDeque::new();
    let mut events = Vec::new();

    if let Some(first) = handle_openai_output_item(item, &mut saw_text_delta, &mut pending) {
        events.push(first);
    }
    while let Some(ev) = pending.pop_front() {
        events.push(ev);
    }

    let mut saw_prefix = false;
    let mut saw_tool = false;
    let mut saw_input = false;

    for event in events {
        match event {
            StreamEvent::TextDelta(text) => {
                if text.contains("Status: I detected pre-existing local edits") {
                    saw_prefix = true;
                }
            }
            StreamEvent::ToolUseStart { name, .. } => {
                if name == "batch" {
                    saw_tool = true;
                }
            }
            StreamEvent::ToolInputDelta(delta) => {
                let args: Value = serde_json::from_str(&delta).expect("valid tool args");
                let calls = args
                    .get("tool_calls")
                    .and_then(|v| v.as_array())
                    .expect("tool_calls array");
                assert_eq!(calls.len(), 3);
                saw_input = true;
            }
            _ => {}
        }
    }

    assert!(saw_prefix);
    assert!(saw_tool);
    assert!(saw_input);
}

#[test]
fn test_build_responses_input_rewrites_orphan_tool_output_as_user_message() {
    let messages = vec![ChatMessage::tool_result(
        "call_orphan",
        "orphan result",
        false,
    )];

    let items = build_responses_input(&messages);
    let mut saw_rewritten_message = false;

    for item in &items {
        assert_ne!(
            item.get("type").and_then(|v| v.as_str()),
            Some("function_call_output")
        );
        if item.get("type").and_then(|v| v.as_str()) == Some("message")
            && item.get("role").and_then(|v| v.as_str()) == Some("user")
            && let Some(content) = item.get("content").and_then(|v| v.as_array())
        {
            for part in content {
                if part.get("type").and_then(|v| v.as_str()) == Some("input_text") {
                    let text = part.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    if text.contains("[Recovered orphaned tool output: call_orphan]")
                        && text.contains("orphan result")
                    {
                        saw_rewritten_message = true;
                    }
                }
            }
        }
    }

    assert!(saw_rewritten_message);
}

#[test]
fn test_extract_selfdev_section_missing_returns_none() {
    let system = "# Environment\nDate: 2026-01-01\n\n# Available Skills\n- test";
    assert!(extract_selfdev_section(system).is_none());
}

#[test]
fn test_extract_selfdev_section_stops_at_next_top_level_header() {
    let system = "# Environment\nDate: 2026-01-01\n\n# Self-Development Mode\nUse selfdev tool\n## selfdev Tool\nreload\n\n# Available Skills\n- test";
    let section = extract_selfdev_section(system).expect("expected self-dev section");
    assert!(section.starts_with("# Self-Development Mode"));
    assert!(section.contains("Use selfdev tool"));
    assert!(section.contains("## selfdev Tool"));
    assert!(!section.contains("# Available Skills"));
}

#[test]
fn test_chatgpt_instructions_with_selfdev_appends_selfdev_block() {
    let system = "# Environment\nDate: 2026-01-01\n\n# Self-Development Mode\nUse selfdev tool\n\n# Available Skills\n- test";

    let instructions = OpenAIProvider::chatgpt_instructions_with_selfdev(system);
    assert!(instructions.contains("Jcode Agent, in the Jcode harness"));
    assert!(instructions.contains("# Self-Development Mode"));
    assert!(instructions.contains("Use selfdev tool"));
}
