//! Live OpenAI-compatible provider probes shared by the auth lifecycle driver
//! and the provider doctor. These are pure HTTP/JSON checks with no test-only
//! dependencies, so they compile into the shipping binary.
//!
//! The OpenAI-compatible probes hit `/v1/chat/completions` directly. The native
//! Claude probes ([`run_live_claude_native_*`]) instead drive the production
//! [`AnthropicProvider`] runtime end-to-end (auth, OAuth preflight, request
//! shaping, SSE translation, tool-name mapping), so `provider-doctor claude`
//! exercises the exact code path a real subscription session uses rather than a
//! re-implementation of the Messages API.

use anyhow::{Context, anyhow, ensure};
use serde::Deserialize;

use crate::message::{ContentBlock, Message, Role, StreamEvent, ToolDefinition};
use crate::provider::Provider;
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::antigravity::AntigravityProvider;
use crate::provider_catalog::{OpenAiCompatibleProfile, ResolvedOpenAiCompatibleProfile};

/// Apply the right auth headers for a resolved OpenAI-compatible profile.
///
/// Most providers use `Authorization: Bearer <key>`. Anthropic's
/// OpenAI-compatible endpoints authenticate with `x-api-key` plus a required
/// `anthropic-version` header and reject Bearer auth (401), so key off the
/// resolved host.
fn apply_provider_auth(
    request: reqwest::RequestBuilder,
    resolved: &ResolvedOpenAiCompatibleProfile,
    api_key: &str,
) -> reqwest::RequestBuilder {
    if resolved
        .api_base
        .to_ascii_lowercase()
        .contains("api.anthropic.com")
    {
        return request
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01");
    }
    request.bearer_auth(api_key)
}

/// Set an output-token cap on a chat-completions body using the parameter name
/// the provider accepts. OpenAI's newer models (gpt-5.x) reject the legacy
/// `max_tokens` and require `max_completion_tokens`; most OpenAI-compatible and
/// Anthropic endpoints still take `max_tokens`. Keying off the resolved host
/// keeps the live probes one round-trip without provider-specific retries.
fn set_output_token_cap(
    body: &mut serde_json::Value,
    resolved: &ResolvedOpenAiCompatibleProfile,
    cap: u32,
) {
    let key = if resolved
        .api_base
        .to_ascii_lowercase()
        .contains("api.openai.com")
    {
        "max_completion_tokens"
    } else {
        "max_tokens"
    };
    body[key] = serde_json::json!(cap);
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

pub async fn fetch_live_openai_compatible_models(
    profile: OpenAiCompatibleProfile,
    api_key: &str,
) -> anyhow::Result<Vec<String>> {
    let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
    let url = format!("{}/models", resolved.api_base.trim_end_matches('/'));
    let request = crate::provider::shared_http_client().get(&url);
    let request = apply_provider_auth(request, &resolved, api_key);
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
        .map(|model| normalize_openai_compatible_model_id(&resolved, model.id.trim()))
        .filter(|model| {
            !model.is_empty()
                && crate::provider_catalog::openai_compatible_profile_model_supports_chat(
                    resolved.id.as_str(),
                    model,
                )
        })
        .collect::<Vec<_>>();
    ensure!(
        !models.is_empty(),
        "{} live model catalog returned no models",
        resolved.display_name
    );
    Ok(models)
}

/// Normalize a model id returned by a provider's `/models` endpoint into the
/// bare id jcode uses for routing and coverage keys.
///
/// Google's OpenAI-compatible Gemini surface returns ids prefixed with
/// `models/` (e.g. `models/gemini-2.5-flash`); chat/stream/tool calls accept
/// either form, but the coverage ledger and picker want the bare name so the
/// pair lines up with the native `gemini` provider's models.
fn normalize_openai_compatible_model_id(
    resolved: &ResolvedOpenAiCompatibleProfile,
    model: &str,
) -> String {
    if resolved
        .api_base
        .to_ascii_lowercase()
        .contains("generativelanguage.googleapis.com")
    {
        return model.trim_start_matches("models/").to_string();
    }
    model.to_string()
}

pub async fn run_live_openai_compatible_smoke(
    profile: OpenAiCompatibleProfile,
    api_key: &str,
    model: &str,
) -> anyhow::Result<crate::live_tests::LiveVerificationStage> {
    let started = std::time::Instant::now();
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
        "stream": false
    });
    let request = crate::provider::shared_http_client().post(&url).json(&body);
    let request = apply_provider_auth(request, &resolved, api_key);
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
    let mut stage = crate::live_tests::LiveVerificationStage::passed(
        crate::live_tests::checkpoints::NON_STREAMING_CHAT_COMPLETION,
    )
    .with_duration_ms(started.elapsed().as_millis() as u64)
    .with_evidence("http_status", serde_json::json!(status.as_u16()))
    .with_evidence("matched_expected_content", serde_json::json!(true));
    for key in ["id", "model", "usage", "cost"] {
        if let Some(value) = parsed.get(key) {
            stage = stage.with_evidence(key, value.clone());
        }
    }
    Ok(stage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_catalog::resolve_openai_compatible_profile;
    use jcode_provider_metadata::{
        GEMINI_OPENAI_COMPAT_PROFILE, OPENAI_NATIVE_OPENAI_COMPAT_PROFILE,
    };

    #[test]
    fn gemini_openai_compat_strips_models_prefix_from_catalog_ids() {
        let resolved = resolve_openai_compatible_profile(GEMINI_OPENAI_COMPAT_PROFILE);
        assert_eq!(
            normalize_openai_compatible_model_id(&resolved, "models/gemini-2.5-flash"),
            "gemini-2.5-flash"
        );
        // Already-bare ids pass through unchanged.
        assert_eq!(
            normalize_openai_compatible_model_id(&resolved, "gemini-2.5-pro"),
            "gemini-2.5-pro"
        );
    }

    #[test]
    fn non_gemini_openai_compat_leaves_model_ids_untouched() {
        let resolved = resolve_openai_compatible_profile(OPENAI_NATIVE_OPENAI_COMPAT_PROFILE);
        // A leading `models/` segment on a non-Gemini host is not stripped.
        assert_eq!(
            normalize_openai_compatible_model_id(&resolved, "models/gpt-5.1"),
            "models/gpt-5.1"
        );
        assert_eq!(
            normalize_openai_compatible_model_id(&resolved, "gpt-5.1"),
            "gpt-5.1"
        );
    }
}

pub async fn run_live_openai_compatible_stream_smoke(
    profile: OpenAiCompatibleProfile,
    api_key: &str,
    model: &str,
) -> anyhow::Result<crate::live_tests::LiveVerificationStage> {
    let started = std::time::Instant::now();
    let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
    let url = format!(
        "{}/chat/completions",
        resolved.api_base.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "user", "content": "Reply with exactly STREAM_TEST_OK and nothing else."}
        ],
        "stream": true,
        "stream_options": {"include_usage": true}
    });
    let request = crate::provider::shared_http_client().post(&url).json(&body);
    let request = apply_provider_auth(request, &resolved, api_key);
    let response = tokio::time::timeout(std::time::Duration::from_secs(45), request.send())
        .await
        .context("timed out running live stream smoke completion")?
        .with_context(|| format!("run live {} stream smoke completion", resolved.display_name))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    ensure!(
        status.is_success(),
        "{} live stream smoke failed (HTTP {}): {}",
        resolved.display_name,
        status,
        text.trim()
    );

    let mut content = String::new();
    let mut chunk_count = 0usize;
    let mut finish_reason = serde_json::Value::Null;
    let mut usage = serde_json::Value::Null;
    for line in text.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        if data.is_empty() {
            continue;
        }
        let parsed: serde_json::Value = serde_json::from_str(data)
            .with_context(|| format!("parse live {} stream chunk", resolved.display_name))?;
        chunk_count += 1;
        if let Some(reported) = parsed.get("usage").filter(|usage| !usage.is_null()) {
            usage = reported.clone();
        }
        if let Some(delta) = parsed
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("delta"))
            && let Some(part) = delta.get("content").and_then(|content| content.as_str())
        {
            content.push_str(part);
        }
        if let Some(reason) = parsed
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("finish_reason"))
            .filter(|reason| !reason.is_null())
        {
            finish_reason = reason.clone();
        }
    }
    ensure!(
        content.contains("STREAM_TEST_OK"),
        "{} live stream smoke returned unexpected content: {:?}",
        resolved.display_name,
        content
    );
    let mut stage = crate::live_tests::LiveVerificationStage::passed(
        crate::live_tests::checkpoints::STREAMING_CHAT_COMPLETION,
    )
    .with_duration_ms(started.elapsed().as_millis() as u64)
    .with_evidence("http_status", serde_json::json!(status.as_u16()))
    .with_evidence("chunk_count", serde_json::json!(chunk_count))
    .with_evidence("finish_reason", finish_reason)
    .with_evidence("matched_expected_content", serde_json::json!(true));
    if !usage.is_null() {
        stage = stage.with_evidence("usage", usage);
    }
    Ok(stage)
}

pub async fn run_live_openai_compatible_tool_smoke(
    profile: OpenAiCompatibleProfile,
    api_key: &str,
    model: &str,
) -> anyhow::Result<crate::live_tests::LiveVerificationStage> {
    let started = std::time::Instant::now();
    let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
    let url = format!(
        "{}/chat/completions",
        resolved.api_base.trim_end_matches('/')
    );
    let tool_name = "auth_tool_probe";
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "user", "content": "Call the auth_tool_probe tool now. Do not answer in text."}
        ],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": tool_name,
                    "description": "A no-op live auth/tool-call smoke-test tool.",
                    "parameters": {
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false
                    }
                }
            }
        ],
        "stream": false
    });
    set_output_token_cap(&mut body, &resolved, 256);
    if !resolved.api_base.contains("fptcloud.com") {
        body["tool_choice"] = serde_json::json!("auto");
    }
    let request = crate::provider::shared_http_client().post(&url).json(&body);
    let request = apply_provider_auth(request, &resolved, api_key);
    let response = tokio::time::timeout(std::time::Duration::from_secs(45), request.send())
        .await
        .context("timed out running live tool-call smoke completion")?
        .with_context(|| format!("run live {} tool-call smoke", resolved.display_name))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    ensure!(
        status.is_success(),
        "{} live tool-call smoke failed (HTTP {}): {}",
        resolved.display_name,
        status,
        text.trim()
    );
    let parsed: serde_json::Value = serde_json::from_str(&text).with_context(|| {
        format!(
            "parse live {} tool-call smoke response",
            resolved.display_name
        )
    })?;
    let tool_calls = parsed
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("tool_calls"))
        .and_then(|tool_calls| tool_calls.as_array())
        .cloned()
        .unwrap_or_default();
    ensure!(
        !tool_calls.is_empty(),
        "{} live tool-call smoke returned no tool calls: {}",
        resolved.display_name,
        crate::util::truncate_str(text.trim(), 1200)
    );
    let function = tool_calls[0]
        .get("function")
        .and_then(|function| function.as_object())
        .context("live tool-call smoke response missing function object")?;
    let returned_name = function
        .get("name")
        .and_then(|name| name.as_str())
        .unwrap_or_default();
    ensure!(
        returned_name == tool_name,
        "{} live tool-call smoke returned unexpected tool name {:?}",
        resolved.display_name,
        returned_name
    );
    let arguments = function
        .get("arguments")
        .and_then(|arguments| arguments.as_str())
        .context("live tool-call smoke response missing string arguments")?;
    let parsed_arguments = crate::message::ToolCall::parse_streamed_input_to_object(arguments);
    ensure!(
        parsed_arguments.is_object(),
        "{} live tool-call smoke returned non-object tool arguments: {:?}",
        resolved.display_name,
        arguments
    );
    let choice = parsed
        .get("choices")
        .and_then(|choices| choices.get(0))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let mut stage = crate::live_tests::LiveVerificationStage::passed(
        crate::live_tests::checkpoints::TOOL_CALL_PARSE,
    )
    .with_duration_ms(started.elapsed().as_millis() as u64)
    .with_evidence("http_status", serde_json::json!(status.as_u16()))
    .with_evidence("tool_name", serde_json::json!(returned_name))
    .with_evidence("tool_arguments", parsed_arguments)
    .with_evidence(
        "finish_reason",
        choice
            .get("finish_reason")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    );
    for key in ["id", "model", "usage", "cost"] {
        if let Some(value) = parsed.get(key) {
            stage = stage.with_evidence(key, value.clone());
        }
    }
    Ok(stage)
}

// ---------------------------------------------------------------------------
// Native Claude (Anthropic Messages API) probes
// ---------------------------------------------------------------------------
//
// Unlike the OpenAI-compatible probes above, these drive the production
// `AnthropicProvider` runtime directly. That runtime resolves OAuth/API-key
// credentials, runs the Claude Code OAuth preflight, shapes the Messages-API
// request (system identity, thinking config, tool-name remapping), and
// translates the SSE stream into `StreamEvent`s. Exercising it here means
// `provider-doctor claude` validates the real subscription path instead of a
// parallel HTTP re-implementation that could silently drift.

/// A small wrapper so the doctor can build a provider once and reuse it across
/// the chat/stream/tool stages (each stage opens its own request).
fn build_native_claude_provider(model: &str) -> anyhow::Result<AnthropicProvider> {
    let provider = AnthropicProvider::new();
    provider
        .set_model(model)
        .with_context(|| format!("select Claude model `{model}` for native probe"))?;
    Ok(provider)
}

/// Convert the provider's streamed token-usage event into the OpenAI-style
/// `usage` evidence object the ledger/spend accounting already understands
/// (`input_tokens`/`output_tokens`, mirrored into `prompt_tokens`/
/// `completion_tokens`).
fn usage_evidence(
    input_tokens: u64,
    output_tokens: u64,
    cache_read: u64,
    cache_creation: u64,
) -> serde_json::Value {
    serde_json::json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "prompt_tokens": input_tokens,
        "completion_tokens": output_tokens,
        "total_tokens": input_tokens + output_tokens,
        "cache_read_input_tokens": cache_read,
        "cache_creation_input_tokens": cache_creation,
    })
}

/// Outcome of consuming a native Claude stream for a single probe.
#[derive(Default)]
struct NativeClaudeStreamOutcome {
    text: String,
    chunk_count: usize,
    /// Number of thinking deltas seen (extended/adaptive thinking). Useful when
    /// a turn is consumed entirely by reasoning and emits no visible text.
    thinking_chunk_count: usize,
    /// Total stream events observed, for diagnosing empty/odd streams.
    total_events: usize,
    saw_message_end: bool,
    stop_reason: Option<String>,
    tool_calls: Vec<NativeClaudeToolCall>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read: u64,
    cache_creation: u64,
}

#[derive(Clone)]
struct NativeClaudeToolCall {
    id: String,
    name: String,
    input_json: String,
    /// Gemini 3 "thought signature" replayed back on the matching `functionCall`
    /// part in later turns. The Antigravity/Cloud Code backend rejects a
    /// follow-up turn whose tool_use omits it. `None` for providers (e.g.
    /// Claude) that do not emit thought signatures.
    thought_signature: Option<String>,
}

impl NativeClaudeStreamOutcome {
    fn usage_evidence(&self) -> Option<serde_json::Value> {
        if self.input_tokens == 0 && self.output_tokens == 0 {
            return None;
        }
        Some(usage_evidence(
            self.input_tokens,
            self.output_tokens,
            self.cache_read,
            self.cache_creation,
        ))
    }

    /// Compact, secret-free description of what the stream produced, for failure
    /// messages (`stop_reason`, event/text/thinking counts).
    fn diagnostics(&self) -> String {
        format!(
            "stop_reason={:?}, events={}, text_deltas={}, thinking_deltas={}, tool_calls={}",
            self.stop_reason,
            self.total_events,
            self.chunk_count,
            self.thinking_chunk_count,
            self.tool_calls.len()
        )
    }
}

/// Drive any native [`Provider`] runtime's `complete` and fold the resulting
/// stream into a single outcome, surfacing any provider-emitted error as a hard
/// failure.
///
/// Shared by the native Claude and native Antigravity probes: both drive the
/// production provider runtime (auth, request shaping, SSE/stream translation,
/// tool-name mapping) rather than re-implementing the wire protocol, so the
/// doctor exercises the exact code path a real session uses.
async fn consume_native_stream(
    provider: &dyn Provider,
    messages: &[Message],
    tools: &[ToolDefinition],
    system: &str,
    timeout: std::time::Duration,
) -> anyhow::Result<NativeClaudeStreamOutcome> {
    use futures::StreamExt;

    let mut stream = provider
        .complete(messages, tools, system, None)
        .await
        .context("open native provider stream")?;

    tokio::time::timeout(timeout, async move {
        let mut outcome = NativeClaudeStreamOutcome::default();
        let mut pending_tool: Option<NativeClaudeToolCall> = None;
        while let Some(event) = stream.next().await {
            outcome.total_events += 1;
            match event.context("native Claude stream event error")? {
                StreamEvent::TextDelta(text) => {
                    outcome.chunk_count += 1;
                    outcome.text.push_str(&text);
                }
                StreamEvent::ThinkingDelta(_) => {
                    outcome.thinking_chunk_count += 1;
                }
                StreamEvent::ToolUseStart { id, name } => {
                    pending_tool = Some(NativeClaudeToolCall {
                        id,
                        name,
                        input_json: String::new(),
                        thought_signature: None,
                    });
                }
                StreamEvent::ToolInputDelta(fragment) => {
                    if let Some(tool) = pending_tool.as_mut() {
                        tool.input_json.push_str(&fragment);
                    }
                }
                StreamEvent::ToolUseEnd => {
                    if let Some(tool) = pending_tool.take() {
                        outcome.tool_calls.push(tool);
                    }
                }
                // Emitted after the matching `ToolUseEnd`; attach it to the most
                // recent tool call so probes can replay it on the next turn.
                StreamEvent::ToolUseSignature(signature) => {
                    if let Some(tool) = outcome.tool_calls.last_mut() {
                        if !signature.is_empty() {
                            tool.thought_signature = Some(signature);
                        }
                    }
                }
                StreamEvent::TokenUsage {
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                } => {
                    if let Some(value) = input_tokens {
                        outcome.input_tokens = value;
                    }
                    if let Some(value) = output_tokens {
                        outcome.output_tokens = value;
                    }
                    if let Some(value) = cache_read_input_tokens {
                        outcome.cache_read = value;
                    }
                    if let Some(value) = cache_creation_input_tokens {
                        outcome.cache_creation = value;
                    }
                }
                StreamEvent::MessageEnd { stop_reason } => {
                    outcome.saw_message_end = true;
                    outcome.stop_reason = stop_reason;
                    // Do NOT break here: the Anthropic runtime emits the final
                    // `TokenUsage` event *after* `MessageEnd`, so we keep draining
                    // until the stream ends to capture token accounting for spend.
                }
                StreamEvent::Error { message, .. } => {
                    return Err(anyhow!(message));
                }
                _ => {}
            }
        }
        Ok(outcome)
    })
    .await
    .context("native Claude stream timed out")?
}

/// Stage: non-streaming chat completion.
///
/// The native runtime always streams, so "non-streaming" here means "a single
/// turn that produces a coherent final answer". We assert the model returned
/// text and reached a clean end-of-message.
pub async fn run_live_claude_native_smoke(
    model: &str,
) -> anyhow::Result<crate::live_tests::LiveVerificationStage> {
    let started = std::time::Instant::now();
    let provider = build_native_claude_provider(model)?;
    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Reply with exactly AUTH_TEST_OK and nothing else.".to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }];
    let system = "You are a live provider smoke test. Answer with the exact requested token only.";
    let outcome = consume_native_stream(
        &provider,
        &messages,
        &[],
        system,
        std::time::Duration::from_secs(60),
    )
    .await?;

    ensure!(
        outcome.saw_message_end,
        "native Claude smoke ended without a message_end event ({})",
        outcome.diagnostics()
    );
    ensure!(
        outcome.text.contains("AUTH_TEST_OK"),
        "native Claude smoke returned unexpected content: {:?} ({})",
        crate::util::truncate_str(outcome.text.trim(), 200),
        outcome.diagnostics()
    );

    let mut stage = crate::live_tests::LiveVerificationStage::passed(
        crate::live_tests::checkpoints::NON_STREAMING_CHAT_COMPLETION,
    )
    .with_duration_ms(started.elapsed().as_millis() as u64)
    .with_evidence("model", serde_json::json!(model))
    .with_evidence("matched_expected_content", serde_json::json!(true))
    .with_evidence(
        "stop_reason",
        serde_json::json!(outcome.stop_reason.clone()),
    );
    if let Some(usage) = outcome.usage_evidence() {
        stage = stage.with_evidence("usage", usage);
    }
    Ok(stage)
}

/// Stage: streaming chat completion.
///
/// Asserts the runtime delivered the answer incrementally (multiple text
/// deltas) rather than as a single blob, which is the property the streaming
/// checkpoint exists to guard.
pub async fn run_live_claude_native_stream_smoke(
    model: &str,
) -> anyhow::Result<crate::live_tests::LiveVerificationStage> {
    let started = std::time::Instant::now();
    let provider = build_native_claude_provider(model)?;
    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Without using any tools, write the numbers 1 through 5, each on its own \
                   line, then write STREAM_TEST_OK on the final line. Respond with plain text \
                   only and do not call any tool."
                .to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }];
    let system = "You are a live provider streaming smoke test. Follow the instructions exactly \
                  and never call a tool; reply with plain streamed text only.";

    // The OAuth runtime always injects the Claude Code tool set, so a task-like
    // prompt can occasionally make the model emit a `tool_use` turn (0 text
    // deltas) instead of streamed text. The prompt forbids tools, but the model
    // is non-deterministic, so retry a few times before declaring failure.
    const MAX_ATTEMPTS: usize = 3;
    let mut outcome = NativeClaudeStreamOutcome::default();
    let mut attempts = 0usize;
    let mut last_err: Option<String> = None;
    while attempts < MAX_ATTEMPTS {
        attempts += 1;
        let candidate = consume_native_stream(
            &provider,
            &messages,
            &[],
            system,
            std::time::Duration::from_secs(90),
        )
        .await?;

        let ok = candidate.saw_message_end
            && candidate.chunk_count > 0
            && candidate.text.contains("STREAM_TEST_OK");
        outcome = candidate;
        if ok {
            break;
        }
        last_err = Some(format!(
            "attempt {attempts}/{MAX_ATTEMPTS}: {}",
            outcome.diagnostics()
        ));
    }

    ensure!(
        outcome.saw_message_end,
        "native Claude stream smoke ended without a message_end event ({})",
        outcome.diagnostics()
    );
    ensure!(
        outcome.chunk_count > 0,
        "native Claude stream smoke produced no streamed text deltas after {attempts} attempt(s) ({}); last: {}",
        outcome.diagnostics(),
        last_err.as_deref().unwrap_or("n/a")
    );
    ensure!(
        outcome.text.contains("STREAM_TEST_OK"),
        "native Claude stream smoke returned unexpected content: {:?} ({})",
        crate::util::truncate_str(outcome.text.trim(), 200),
        outcome.diagnostics()
    );

    let mut stage = crate::live_tests::LiveVerificationStage::passed(
        crate::live_tests::checkpoints::STREAMING_CHAT_COMPLETION,
    )
    .with_duration_ms(started.elapsed().as_millis() as u64)
    .with_evidence("model", serde_json::json!(model))
    .with_evidence("chunk_count", serde_json::json!(outcome.chunk_count))
    .with_evidence("attempts", serde_json::json!(attempts))
    .with_evidence(
        "thinking_chunk_count",
        serde_json::json!(outcome.thinking_chunk_count),
    )
    .with_evidence("total_events", serde_json::json!(outcome.total_events))
    .with_evidence("matched_expected_content", serde_json::json!(true))
    .with_evidence(
        "stop_reason",
        serde_json::json!(outcome.stop_reason.clone()),
    );
    if let Some(usage) = outcome.usage_evidence() {
        stage = stage.with_evidence("usage", usage);
    }
    Ok(stage)
}

/// Stage: tool-call parse + execution loop + result follow-up.
///
/// Runs a full two-turn round-trip:
///   1. Ask the model to call a tool; assert it emits a parseable `tool_use`.
///   2. Feed a synthetic `tool_result` back; assert the model consumes it and
///      produces a coherent final answer.
///
/// This single round-trip is the evidence for the `tool_call_parse`,
/// `tool_execution_loop`, `tool_result_followup`, and `real_jcode_tool_smoke`
/// checkpoints (mirroring how the OpenAI-compatible tool probe derives all
/// four from one exchange).
pub async fn run_live_claude_native_tool_smoke(
    model: &str,
) -> anyhow::Result<crate::live_tests::LiveVerificationStage> {
    let started = std::time::Instant::now();
    let provider = build_native_claude_provider(model)?;

    // The OAuth runtime replaces caller-supplied tools with the fixed Claude
    // Code tool set, so target a built-in tool (`read`) that exists in both the
    // OAuth and API-key tool surfaces. The API-key path uses the schema we send
    // here directly.
    let tool_name = "read";
    let tools = vec![ToolDefinition {
        name: tool_name.to_string(),
        description: "Reads a file from the local filesystem.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"file_path": {"type": "string"}},
            "required": ["file_path"],
            "additionalProperties": false
        }),
    }];
    let system = "You are a live provider tool smoke test. When asked to read a file, you MUST \
                  call the read tool with the given path. Do not answer in text first.";

    let first_turn = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Read the file at /tmp/auth_tool_probe.txt using the read tool. \
                   Call the tool now; do not answer in text."
                .to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }];

    let first = consume_native_stream(
        &provider,
        &first_turn,
        &tools,
        system,
        std::time::Duration::from_secs(90),
    )
    .await?;

    ensure!(
        !first.tool_calls.is_empty(),
        "native Claude tool smoke produced no tool call (stop_reason={:?}, text={:?})",
        first.stop_reason,
        crate::util::truncate_str(first.text.trim(), 200)
    );
    let tool_call = first.tool_calls[0].clone();
    ensure!(
        tool_call.name == tool_name,
        "native Claude tool smoke called unexpected tool {:?} (expected {tool_name})",
        tool_call.name
    );
    let parsed_arguments = crate::message::ToolCall::parse_streamed_input_to_object(
        if tool_call.input_json.trim().is_empty() {
            "{}"
        } else {
            tool_call.input_json.trim()
        },
    );
    ensure!(
        parsed_arguments.is_object(),
        "native Claude tool smoke produced non-object tool arguments: {:?}",
        tool_call.input_json
    );

    // Second turn: replay the assistant's tool_use and answer it with a
    // synthetic tool_result, then assert the model produces a final answer that
    // consumes the result. This is the `tool_result_followup` evidence.
    let mut followup = first_turn.clone();
    followup.push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            input: parsed_arguments.clone(),
            thought_signature: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    });
    followup.push(Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: tool_call.id.clone(),
            content: "TOOL_RESULT_TOKEN=42. Report this token back to confirm you read it."
                .to_string(),
            is_error: Some(false),
        }],
        timestamp: None,
        tool_duration_ms: None,
    });

    let second = consume_native_stream(
        &provider,
        &followup,
        &tools,
        system,
        std::time::Duration::from_secs(90),
    )
    .await?;

    ensure!(
        second.saw_message_end,
        "native Claude tool follow-up ended without a message_end event"
    );
    ensure!(
        second.text.contains("42"),
        "native Claude tool follow-up did not reflect the tool result token: {:?}",
        crate::util::truncate_str(second.text.trim(), 200)
    );

    // Total usage spans both turns so spend accounting reflects the full
    // round-trip.
    let total_input = first.input_tokens + second.input_tokens;
    let total_output = first.output_tokens + second.output_tokens;
    let mut stage = crate::live_tests::LiveVerificationStage::passed(
        crate::live_tests::checkpoints::TOOL_CALL_PARSE,
    )
    .with_duration_ms(started.elapsed().as_millis() as u64)
    .with_evidence("model", serde_json::json!(model))
    .with_evidence("tool_name", serde_json::json!(tool_call.name))
    .with_evidence("tool_arguments", parsed_arguments)
    .with_evidence(
        "followup_consumed_result",
        serde_json::json!(true),
    );
    if total_input != 0 || total_output != 0 {
        stage = stage.with_evidence("usage", usage_evidence(total_input, total_output, 0, 0));
    }
    Ok(stage)
}

// === Native Antigravity probes ============================================
//
// Antigravity is a Google OAuth login provider whose `generateContent` runtime
// multiplexes Gemini, Claude, and gpt-oss upstreams behind one Cloud Code
// endpoint. Like the native Claude probes, these drive the production
// [`AntigravityProvider`] runtime end-to-end (OAuth token load/refresh, project
// resolution, request shaping, the Gemini->StreamEvent translation, the
// per-model schema normalization, and Gemini-3 thought-signature replay) so
// `provider-doctor antigravity` exercises the exact path a real session uses.

/// Build a fresh native Antigravity provider pinned to `model`.
fn build_native_antigravity_provider(model: &str) -> anyhow::Result<AntigravityProvider> {
    let provider = AntigravityProvider::new();
    provider
        .set_model(model)
        .with_context(|| format!("select Antigravity model `{model}` for native probe"))?;
    Ok(provider)
}

/// Stage: non-streaming chat completion (a single coherent final answer).
pub async fn run_live_antigravity_native_smoke(
    model: &str,
) -> anyhow::Result<crate::live_tests::LiveVerificationStage> {
    let started = std::time::Instant::now();
    let provider = build_native_antigravity_provider(model)?;
    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Reply with exactly AUTH_TEST_OK and nothing else.".to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }];
    let system = "You are a live provider smoke test. Answer with the exact requested token only.";
    let outcome = consume_native_stream(
        &provider,
        &messages,
        &[],
        system,
        std::time::Duration::from_secs(90),
    )
    .await?;

    ensure!(
        outcome.saw_message_end,
        "native Antigravity smoke ended without a message_end event ({})",
        outcome.diagnostics()
    );
    ensure!(
        outcome.text.contains("AUTH_TEST_OK"),
        "native Antigravity smoke returned unexpected content: {:?} ({})",
        crate::util::truncate_str(outcome.text.trim(), 200),
        outcome.diagnostics()
    );

    let mut stage = crate::live_tests::LiveVerificationStage::passed(
        crate::live_tests::checkpoints::NON_STREAMING_CHAT_COMPLETION,
    )
    .with_duration_ms(started.elapsed().as_millis() as u64)
    .with_evidence("model", serde_json::json!(model))
    .with_evidence("matched_expected_content", serde_json::json!(true))
    .with_evidence(
        "stop_reason",
        serde_json::json!(outcome.stop_reason.clone()),
    );
    if let Some(usage) = outcome.usage_evidence() {
        stage = stage.with_evidence("usage", usage);
    }
    Ok(stage)
}

/// Stage: streaming chat completion.
///
/// The Antigravity runtime delivers `generateContent` as a single response that
/// jcode re-emits as text deltas, so we assert the runtime produced streamed
/// text and reached a clean end-of-message rather than requiring many deltas.
pub async fn run_live_antigravity_native_stream_smoke(
    model: &str,
) -> anyhow::Result<crate::live_tests::LiveVerificationStage> {
    let started = std::time::Instant::now();
    let provider = build_native_antigravity_provider(model)?;
    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Without using any tools, write the numbers 1 through 5, each on its own \
                   line, then write STREAM_TEST_OK on the final line. Respond with plain text \
                   only and do not call any tool."
                .to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }];
    let system = "You are a live provider streaming smoke test. Follow the instructions exactly \
                  and never call a tool; reply with plain streamed text only.";

    const MAX_ATTEMPTS: usize = 3;
    let mut outcome = NativeClaudeStreamOutcome::default();
    let mut attempts = 0usize;
    let mut last_err: Option<String> = None;
    while attempts < MAX_ATTEMPTS {
        attempts += 1;
        let candidate = consume_native_stream(
            &provider,
            &messages,
            &[],
            system,
            std::time::Duration::from_secs(120),
        )
        .await?;

        let ok = candidate.saw_message_end
            && candidate.chunk_count > 0
            && candidate.text.contains("STREAM_TEST_OK");
        outcome = candidate;
        if ok {
            break;
        }
        last_err = Some(format!(
            "attempt {attempts}/{MAX_ATTEMPTS}: {}",
            outcome.diagnostics()
        ));
    }

    ensure!(
        outcome.saw_message_end,
        "native Antigravity stream smoke ended without a message_end event ({})",
        outcome.diagnostics()
    );
    ensure!(
        outcome.chunk_count > 0,
        "native Antigravity stream smoke produced no streamed text deltas after {attempts} attempt(s) ({}); last: {}",
        outcome.diagnostics(),
        last_err.as_deref().unwrap_or("n/a")
    );
    ensure!(
        outcome.text.contains("STREAM_TEST_OK"),
        "native Antigravity stream smoke returned unexpected content: {:?} ({})",
        crate::util::truncate_str(outcome.text.trim(), 200),
        outcome.diagnostics()
    );

    let mut stage = crate::live_tests::LiveVerificationStage::passed(
        crate::live_tests::checkpoints::STREAMING_CHAT_COMPLETION,
    )
    .with_duration_ms(started.elapsed().as_millis() as u64)
    .with_evidence("model", serde_json::json!(model))
    .with_evidence("chunk_count", serde_json::json!(outcome.chunk_count))
    .with_evidence("attempts", serde_json::json!(attempts))
    .with_evidence("total_events", serde_json::json!(outcome.total_events))
    .with_evidence("matched_expected_content", serde_json::json!(true))
    .with_evidence(
        "stop_reason",
        serde_json::json!(outcome.stop_reason.clone()),
    );
    if let Some(usage) = outcome.usage_evidence() {
        stage = stage.with_evidence("usage", usage);
    }
    Ok(stage)
}

/// Stage: tool-call parse + execution loop + result follow-up.
///
/// Full two-turn round-trip: ask the model to call a tool (assert a parseable
/// tool_use), then feed a synthetic tool_result back (assert the model consumes
/// it). Gemini-3 attaches a `thought_signature` to its function call that the
/// Cloud Code backend requires replayed on the follow-up turn, so we carry it
/// onto the assistant tool_use block. Evidence for the `tool_call_parse`,
/// `tool_execution_loop`, `tool_result_followup`, and `real_jcode_tool_smoke`
/// checkpoints.
pub async fn run_live_antigravity_native_tool_smoke(
    model: &str,
) -> anyhow::Result<crate::live_tests::LiveVerificationStage> {
    let started = std::time::Instant::now();
    let provider = build_native_antigravity_provider(model)?;

    let tool_name = "read";
    let tools = vec![ToolDefinition {
        name: tool_name.to_string(),
        description: "Reads a file from the local filesystem.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"file_path": {"type": "string"}},
            "required": ["file_path"],
            "additionalProperties": false
        }),
    }];
    let system = "You are a live provider tool smoke test. When asked to read a file, you MUST \
                  call the read tool with the given path. Do not answer in text first.";

    let first_turn = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Read the file at /tmp/auth_tool_probe.txt using the read tool. \
                   Call the tool now; do not answer in text."
                .to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }];

    let first = consume_native_stream(
        &provider,
        &first_turn,
        &tools,
        system,
        std::time::Duration::from_secs(120),
    )
    .await?;

    ensure!(
        !first.tool_calls.is_empty(),
        "native Antigravity tool smoke produced no tool call (stop_reason={:?}, text={:?})",
        first.stop_reason,
        crate::util::truncate_str(first.text.trim(), 200)
    );
    let tool_call = first.tool_calls[0].clone();
    ensure!(
        tool_call.name == tool_name,
        "native Antigravity tool smoke called unexpected tool {:?} (expected {tool_name})",
        tool_call.name
    );
    let parsed_arguments = crate::message::ToolCall::parse_streamed_input_to_object(
        if tool_call.input_json.trim().is_empty() {
            "{}"
        } else {
            tool_call.input_json.trim()
        },
    );
    ensure!(
        parsed_arguments.is_object(),
        "native Antigravity tool smoke produced non-object tool arguments: {:?}",
        tool_call.input_json
    );

    // Second turn: replay the assistant's tool_use (carrying the Gemini-3
    // thought signature, required by the Cloud Code backend) and answer it with
    // a synthetic tool_result, then assert the model consumes the result.
    let mut followup = first_turn.clone();
    followup.push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            input: parsed_arguments.clone(),
            thought_signature: tool_call.thought_signature.clone(),
        }],
        timestamp: None,
        tool_duration_ms: None,
    });
    followup.push(Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: tool_call.id.clone(),
            content: "TOOL_RESULT_TOKEN=42. Report this token back to confirm you read it."
                .to_string(),
            is_error: Some(false),
        }],
        timestamp: None,
        tool_duration_ms: None,
    });

    let second = consume_native_stream(
        &provider,
        &followup,
        &tools,
        system,
        std::time::Duration::from_secs(120),
    )
    .await?;

    ensure!(
        second.saw_message_end,
        "native Antigravity tool follow-up ended without a message_end event"
    );
    ensure!(
        second.text.contains("42"),
        "native Antigravity tool follow-up did not reflect the tool result token: {:?}",
        crate::util::truncate_str(second.text.trim(), 200)
    );

    let total_input = first.input_tokens + second.input_tokens;
    let total_output = first.output_tokens + second.output_tokens;
    let mut stage = crate::live_tests::LiveVerificationStage::passed(
        crate::live_tests::checkpoints::TOOL_CALL_PARSE,
    )
    .with_duration_ms(started.elapsed().as_millis() as u64)
    .with_evidence("model", serde_json::json!(model))
    .with_evidence("tool_name", serde_json::json!(tool_call.name))
    .with_evidence("tool_arguments", parsed_arguments)
    .with_evidence(
        "thought_signature_present",
        serde_json::json!(tool_call.thought_signature.is_some()),
    )
    .with_evidence("followup_consumed_result", serde_json::json!(true));
    if total_input != 0 || total_output != 0 {
        stage = stage.with_evidence("usage", usage_evidence(total_input, total_output, 0, 0));
    }
    Ok(stage)
}
