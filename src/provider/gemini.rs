use super::{EventStream, Provider};
use crate::auth::gemini as gemini_auth;
use crate::message::{ConnectionPhase, Message, Role, StreamEvent, ToolDefinition};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

const DEFAULT_MODEL: &str = "gemini-2.5-pro";
const AVAILABLE_MODELS: &[&str] = &[
    "gemini-2.5-pro",
    "gemini-2.5-flash",
    "gemini-2.0-flash",
    "gemini-1.5-pro",
    "gemini-1.5-flash",
];
const CODE_ASSIST_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com";
const CODE_ASSIST_API_VERSION: &str = "v1internal";
const USER_TIER_FREE: &str = "free-tier";
const USER_TIER_LEGACY: &str = "legacy-tier";

#[derive(Debug, Clone)]
struct GeminiRuntimeState {
    project_id: String,
    session_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientMetadata {
    ide_type: &'static str,
    platform: &'static str,
    plugin_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    duet_project: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadCodeAssistRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    cloudaicompanion_project: Option<String>,
    metadata: ClientMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'static str>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadCodeAssistResponse {
    #[serde(default)]
    current_tier: Option<GeminiUserTier>,
    #[serde(default)]
    allowed_tiers: Option<Vec<GeminiUserTier>>,
    #[serde(default)]
    ineligible_tiers: Option<Vec<IneligibleTier>>,
    #[serde(default)]
    cloudaicompanion_project: Option<String>,
    #[serde(default)]
    paid_tier: Option<GeminiUserTier>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUserTier {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    is_default: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IneligibleTier {
    #[serde(default)]
    reason_code: Option<String>,
    #[serde(default)]
    reason_message: Option<String>,
    #[serde(default)]
    validation_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OnboardUserRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    tier_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cloudaicompanion_project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<ClientMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LongRunningOperationResponse {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    done: Option<bool>,
    #[serde(default)]
    response: Option<OnboardUserResponse>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OnboardUserResponse {
    #[serde(default)]
    cloudaicompanion_project: Option<ProjectRef>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectRef {
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CodeAssistGenerateRequest {
    model: String,
    project: String,
    user_prompt_id: String,
    request: VertexGenerateContentRequest,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VertexGenerateContentRequest {
    contents: Vec<GeminiContent>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiTool>>,
    #[serde(rename = "toolConfig", skip_serializing_if = "Option::is_none")]
    tool_config: Option<GeminiToolConfig>,
    #[serde(rename = "session_id", skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_data: Option<InlineData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<GeminiFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_response: Option<GeminiFunctionResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InlineData {
    mime_type: String,
    data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    #[serde(default)]
    args: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiFunctionResponse {
    name: String,
    response: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct GeminiTool {
    #[serde(rename = "functionDeclarations")]
    function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Clone, Serialize)]
struct GeminiFunctionDeclaration {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiToolConfig {
    function_calling_config: GeminiFunctionCallingConfig,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiFunctionCallingConfig {
    mode: &'static str,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodeAssistGenerateResponse {
    #[serde(default)]
    trace_id: Option<String>,
    #[serde(default)]
    response: Option<VertexGenerateContentResponse>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexGenerateContentResponse {
    #[serde(default)]
    candidates: Option<Vec<GeminiCandidate>>,
    #[serde(default)]
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiContent>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    #[serde(default)]
    prompt_token_count: Option<u64>,
    #[serde(default)]
    candidates_token_count: Option<u64>,
    #[serde(default)]
    cached_content_token_count: Option<u64>,
}

pub struct GeminiProvider {
    client: reqwest::Client,
    model: Arc<RwLock<String>>,
    state: Arc<Mutex<Option<GeminiRuntimeState>>>,
}

impl GeminiProvider {
    pub fn new() -> Self {
        let model = std::env::var("JCODE_GEMINI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
        Self {
            client: crate::provider::shared_http_client(),
            model: Arc::new(RwLock::new(model)),
            state: Arc::new(Mutex::new(None)),
        }
    }

    fn base_url() -> String {
        let endpoint = std::env::var("CODE_ASSIST_ENDPOINT")
            .unwrap_or_else(|_| CODE_ASSIST_ENDPOINT.to_string());
        let version = std::env::var("CODE_ASSIST_API_VERSION")
            .unwrap_or_else(|_| CODE_ASSIST_API_VERSION.to_string());
        format!("{endpoint}/{version}")
    }

    async fn ensure_state(&self) -> Result<GeminiRuntimeState> {
        let mut guard = self.state.lock().await;
        if let Some(state) = guard.clone() {
            return Ok(state);
        }

        let state = self.setup_runtime_state().await?;
        *guard = Some(state.clone());
        Ok(state)
    }

    async fn setup_runtime_state(&self) -> Result<GeminiRuntimeState> {
        let project_id_env = std::env::var("GOOGLE_CLOUD_PROJECT")
            .ok()
            .or_else(|| std::env::var("GOOGLE_CLOUD_PROJECT_ID").ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let metadata = client_metadata(project_id_env.clone());
        let load_req = LoadCodeAssistRequest {
            cloudaicompanion_project: project_id_env.clone(),
            metadata: metadata.clone(),
            mode: None,
        };
        let load_res: LoadCodeAssistResponse = match self.post_json("loadCodeAssist", &load_req).await {
            Ok(response) => response,
            Err(err) if is_vpc_sc_error(&err) => LoadCodeAssistResponse {
                current_tier: Some(GeminiUserTier {
                    id: Some("standard-tier".to_string()),
                    name: None,
                    is_default: None,
                }),
                allowed_tiers: None,
                ineligible_tiers: None,
                cloudaicompanion_project: None,
                paid_tier: None,
            },
            Err(err) => {
                return Err(err).context("Gemini Code Assist setup failed during loadCodeAssist")
            }
        };

        validate_load_code_assist_response(&load_res)?;

        let project_id = if load_res.current_tier.is_some() {
            if let Some(project_id) = load_res.cloudaicompanion_project.clone() {
                project_id
            } else if let Some(project_id) = project_id_env.clone() {
                project_id
            } else {
                return Err(ineligible_or_project_error(&load_res));
            }
        } else {
            let tier = choose_onboard_tier(&load_res);
            let onboard_req = if tier.id.as_deref() == Some(USER_TIER_FREE) {
                OnboardUserRequest {
                    tier_id: tier.id.clone(),
                    cloudaicompanion_project: None,
                    metadata: Some(ClientMetadata {
                        ide_type: "IDE_UNSPECIFIED",
                        platform: "PLATFORM_UNSPECIFIED",
                        plugin_type: "GEMINI",
                        duet_project: None,
                    }),
                }
            } else {
                OnboardUserRequest {
                    tier_id: tier.id.clone(),
                    cloudaicompanion_project: project_id_env.clone(),
                    metadata: Some(metadata.clone()),
                }
            };
            let mut lro: LongRunningOperationResponse = self
                .post_json("onboardUser", &onboard_req)
                .await
                .context("Gemini Code Assist onboarding failed")?;
            while !lro.done.unwrap_or(false) {
                let op_name = lro.name.clone().ok_or_else(|| {
                    anyhow::anyhow!("Gemini onboarding returned no operation name")
                })?;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                lro = self
                    .get_operation(&op_name)
                    .await
                    .context("Gemini onboarding polling failed")?;
            }

            if let Some(project_id) = lro
                .response
                .and_then(|response| response.cloudaicompanion_project)
                .and_then(|project| project.id)
            {
                project_id
            } else if let Some(project_id) = project_id_env.clone() {
                project_id
            } else {
                return Err(ineligible_or_project_error(&load_res));
            }
        };

        Ok(GeminiRuntimeState {
            project_id,
            session_id: Uuid::new_v4().to_string(),
        })
    }

    async fn post_json<T: DeserializeOwned>(
        &self,
        method: &str,
        body: &impl Serialize,
    ) -> Result<T> {
        let tokens = gemini_auth::load_or_refresh_tokens().await?;
        let url = format!("{}:{method}", Self::base_url());
        let resp = self
            .client
            .post(&url)
            .bearer_auth(tokens.access_token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .await
            .with_context(|| format!("Gemini request to {} failed", url))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Gemini request {} failed (HTTP {}): {}",
                method,
                status,
                body.trim()
            );
        }

        resp.json()
            .await
            .with_context(|| format!("Failed to parse Gemini {} response", method))
    }

    async fn get_operation<T: DeserializeOwned>(&self, name: &str) -> Result<T> {
        let tokens = gemini_auth::load_or_refresh_tokens().await?;
        let url = format!("{}/{}", Self::base_url(), name.trim_start_matches('/'));
        let resp = self
            .client
            .get(&url)
            .bearer_auth(tokens.access_token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .send()
            .await
            .with_context(|| format!("Gemini request to {} failed", url))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Gemini operation lookup failed (HTTP {}): {}",
                status,
                body.trim()
            );
        }

        resp.json()
            .await
            .context("Failed to parse Gemini operation response")
    }

    async fn generate_content(
        &self,
        state: &GeminiRuntimeState,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
    ) -> Result<CodeAssistGenerateResponse> {
        let request = CodeAssistGenerateRequest {
            model: model.to_string(),
            project: state.project_id.clone(),
            user_prompt_id: Uuid::new_v4().to_string(),
            request: VertexGenerateContentRequest {
                contents: build_contents(messages),
                system_instruction: build_system_instruction(system),
                tools: build_tools(tools),
                tool_config: if tools.is_empty() {
                    None
                } else {
                    Some(GeminiToolConfig {
                        function_calling_config: GeminiFunctionCallingConfig { mode: "AUTO" },
                    })
                },
                session_id: Some(
                    resume_session_id
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or(&state.session_id)
                        .to_string(),
                ),
            },
        };

        self.post_json("generateContent", &request)
            .await
            .context("Gemini generateContent failed")
    }
}

impl Default for GeminiProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let model = self.model();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let system = system.to_string();
        let resume_session_id = resume_session_id.map(|value| value.to_string());
        let state_cache = self.state.clone();
        let provider = self.clone();
        let (tx, rx) = mpsc::channel::<Result<StreamEvent>>(100);

        tokio::spawn(async move {
            let _ = tx
                .send(Ok(StreamEvent::ConnectionType {
                    connection: "https".to_string(),
                }))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::ConnectionPhase {
                    phase: ConnectionPhase::Authenticating,
                }))
                .await;

            let state = {
                let provider = GeminiProvider {
                    client: provider.client.clone(),
                    model: provider.model.clone(),
                    state: state_cache.clone(),
                };
                match provider.ensure_state().await {
                    Ok(state) => state,
                    Err(err) => {
                        let _ = tx.send(Err(err)).await;
                        return;
                    }
                }
            };

            let _ = tx
                .send(Ok(StreamEvent::SessionId(
                    resume_session_id
                        .clone()
                        .unwrap_or_else(|| state.session_id.clone()),
                )))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::ConnectionPhase {
                    phase: ConnectionPhase::Connecting,
                }))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::ConnectionPhase {
                    phase: ConnectionPhase::WaitingForResponse,
                }))
                .await;

            let response = match provider
                .generate_content(
                    &state,
                    &model,
                    &messages,
                    &tools,
                    &system,
                    resume_session_id.as_deref(),
                )
                .await
            {
                Ok(response) => response,
                Err(err) => {
                    let _ = tx.send(Err(err)).await;
                    return;
                }
            };

            let _ = tx
                .send(Ok(StreamEvent::ConnectionPhase {
                    phase: ConnectionPhase::Streaming,
                }))
                .await;

            if let Some(usage) = response
                .response
                .as_ref()
                .and_then(|response| response.usage_metadata.as_ref())
            {
                let _ = tx
                    .send(Ok(StreamEvent::TokenUsage {
                        input_tokens: usage.prompt_token_count,
                        output_tokens: usage.candidates_token_count,
                        cache_read_input_tokens: usage.cached_content_token_count,
                        cache_creation_input_tokens: None,
                    }))
                    .await;
            }

            let candidate = response
                .response
                .and_then(|response| response.candidates)
                .and_then(|candidates| candidates.into_iter().next());

            let mut stop_reason = None;
            if let Some(candidate) = candidate {
                stop_reason = candidate
                    .finish_reason
                    .clone()
                    .map(|reason| reason.to_lowercase());
                if let Some(content) = candidate.content {
                    for part in content.parts {
                        if let Some(text) = part.text {
                            if !text.is_empty() {
                                let _ = tx.send(Ok(StreamEvent::TextDelta(text))).await;
                            }
                        }
                        if let Some(function_call) = part.function_call {
                            let raw_call_id = function_call
                                .id
                                .clone()
                                .unwrap_or_else(|| Uuid::new_v4().to_string());
                            let call_id = crate::message::sanitize_tool_id(&raw_call_id);
                            let _ = tx
                                .send(Ok(StreamEvent::ToolUseStart {
                                    id: call_id,
                                    name: function_call.name,
                                }))
                                .await;
                            let _ = tx
                                .send(Ok(StreamEvent::ToolInputDelta(
                                    function_call.args.to_string(),
                                )))
                                .await;
                            let _ = tx.send(Ok(StreamEvent::ToolUseEnd)).await;
                        }
                    }
                }
            }

            let _ = tx.send(Ok(StreamEvent::MessageEnd { stop_reason })).await;
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &'static str {
        "gemini"
    }

    fn model(&self) -> String {
        self.model.read().unwrap().clone()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        let trimmed = model.trim();
        if trimmed.is_empty() {
            anyhow::bail!("Gemini model cannot be empty");
        }
        *self.model.write().unwrap() = trimmed.to_string();
        Ok(())
    }

    fn available_models(&self) -> Vec<&'static str> {
        AVAILABLE_MODELS.to_vec()
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            client: self.client.clone(),
            model: Arc::new(RwLock::new(self.model())),
            state: self.state.clone(),
        })
    }

    async fn invalidate_credentials(&self) {
        let mut guard = self.state.lock().await;
        *guard = None;
    }
}

impl Clone for GeminiProvider {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            model: self.model.clone(),
            state: self.state.clone(),
        }
    }
}

fn is_vpc_sc_error(err: &anyhow::Error) -> bool {
    err.to_string().contains("SECURITY_POLICY_VIOLATED")
}

fn client_metadata(project_id: Option<String>) -> ClientMetadata {
    ClientMetadata {
        ide_type: "IDE_UNSPECIFIED",
        platform: "PLATFORM_UNSPECIFIED",
        plugin_type: "GEMINI",
        duet_project: project_id,
    }
}

fn validate_load_code_assist_response(res: &LoadCodeAssistResponse) -> Result<()> {
    if res.current_tier.is_none() {
        if let Some(validation) = res.ineligible_tiers.as_ref().and_then(|tiers| {
            tiers.iter().find(|tier| {
                tier.reason_code.as_deref() == Some("VALIDATION_REQUIRED")
                    && tier.validation_url.is_some()
            })
        }) {
            let description = validation
                .reason_message
                .clone()
                .unwrap_or_else(|| "Account validation required".to_string());
            let url = validation.validation_url.clone().unwrap_or_default();
            anyhow::bail!("{description}. Complete account validation: {url}");
        }
    }
    Ok(())
}

fn ineligible_or_project_error(res: &LoadCodeAssistResponse) -> anyhow::Error {
    if let Some(reasons) = res
        .ineligible_tiers
        .as_ref()
        .filter(|tiers| !tiers.is_empty())
    {
        let joined = reasons
            .iter()
            .filter_map(|tier| tier.reason_message.as_deref())
            .collect::<Vec<_>>()
            .join(", ");
        return anyhow::anyhow!(joined);
    }

    anyhow::anyhow!(
        "This Google account requires setting GOOGLE_CLOUD_PROJECT or GOOGLE_CLOUD_PROJECT_ID. See Gemini Code Assist Workspace auth docs."
    )
}

fn choose_onboard_tier(res: &LoadCodeAssistResponse) -> GeminiUserTier {
    if let Some(default_tier) = res.allowed_tiers.as_ref().and_then(|tiers| {
        tiers
            .iter()
            .find(|tier| tier.is_default.unwrap_or(false))
            .cloned()
    }) {
        return default_tier;
    }

    GeminiUserTier {
        id: Some(USER_TIER_LEGACY.to_string()),
        name: Some(String::new()),
        is_default: None,
    }
}

fn build_system_instruction(system: &str) -> Option<GeminiContent> {
    let trimmed = system.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(GeminiContent {
            role: "user".to_string(),
            parts: vec![GeminiPart {
                text: Some(trimmed.to_string()),
                ..Default::default()
            }],
        })
    }
}

fn build_contents(messages: &[Message]) -> Vec<GeminiContent> {
    messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                Role::User => "user",
                Role::Assistant => "model",
            };
            let mut parts = Vec::new();
            for block in &message.content {
                match block {
                    crate::message::ContentBlock::Text { text, .. } => {
                        parts.push(GeminiPart {
                            text: Some(text.clone()),
                            ..Default::default()
                        });
                    }
                    crate::message::ContentBlock::Reasoning { .. } => {}
                    crate::message::ContentBlock::ToolUse { id, name, input } => {
                        parts.push(GeminiPart {
                            function_call: Some(GeminiFunctionCall {
                                name: name.clone(),
                                args: input.clone(),
                                id: Some(id.clone()),
                            }),
                            ..Default::default()
                        });
                    }
                    crate::message::ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        parts.push(GeminiPart {
                            function_response: Some(GeminiFunctionResponse {
                                name: tool_name_from_tool_result(tool_use_id, messages),
                                response: if is_error.unwrap_or(false) {
                                    json!({ "error": content })
                                } else {
                                    json!({ "content": content })
                                },
                                id: Some(tool_use_id.clone()),
                            }),
                            ..Default::default()
                        });
                    }
                    crate::message::ContentBlock::Image { media_type, data } => {
                        parts.push(GeminiPart {
                            inline_data: Some(InlineData {
                                mime_type: media_type.clone(),
                                data: data.clone(),
                            }),
                            ..Default::default()
                        });
                    }
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(GeminiContent {
                    role: role.to_string(),
                    parts,
                })
            }
        })
        .collect()
}

fn tool_name_from_tool_result(tool_use_id: &str, messages: &[Message]) -> String {
    for message in messages.iter().rev() {
        for block in &message.content {
            if let crate::message::ContentBlock::ToolUse { id, name, .. } = block {
                if id == tool_use_id {
                    return name.clone();
                }
            }
        }
    }
    "tool".to_string()
}

fn build_tools(tools: &[ToolDefinition]) -> Option<Vec<GeminiTool>> {
    if tools.is_empty() {
        return None;
    }

    Some(vec![GeminiTool {
        function_declarations: tools
            .iter()
            .map(|tool| GeminiFunctionDeclaration {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
            })
            .collect(),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentBlock, Message, Role};

    #[test]
    fn available_models_include_gemini_defaults() {
        let provider = GeminiProvider::new();
        let models = provider.available_models();
        assert!(models.contains(&"gemini-2.5-pro"));
        assert!(models.contains(&"gemini-2.5-flash"));
    }

    #[test]
    fn set_model_accepts_gemini_models() {
        let provider = GeminiProvider::new();
        provider.set_model("gemini-2.5-flash").unwrap();
        assert_eq!(provider.model(), "gemini-2.5-flash");
    }

    #[test]
    fn build_contents_preserves_tool_calls_and_results() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "read".to_string(),
                    input: json!({"path":"README.md"}),
                }],
                timestamp: None,
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "ok".to_string(),
                    is_error: None,
                }],
                timestamp: None,
            },
        ];

        let contents = build_contents(&messages);
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].role, "model");
        assert_eq!(contents[1].role, "user");
        assert_eq!(
            contents[0].parts[0].function_call.as_ref().unwrap().name,
            "read"
        );
        assert_eq!(
            contents[1].parts[0]
                .function_response
                .as_ref()
                .unwrap()
                .name,
            "read"
        );
    }

    #[test]
    fn build_tools_uses_function_declarations() {
        let defs = vec![ToolDefinition {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
        }];

        let built = build_tools(&defs).unwrap();
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].function_declarations[0].name, "read");
    }
}
