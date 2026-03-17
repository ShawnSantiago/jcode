use super::{Tool, ToolContext, ToolOutput};
use crate::bus::{Bus, BusEvent, SidePanelUpdated};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

pub struct SidePanelTool;

impl SidePanelTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Deserialize)]
struct SidePanelInput {
    action: String,
    #[serde(default)]
    page_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    focus: Option<bool>,
}

#[async_trait]
impl Tool for SidePanelTool {
    fn name(&self) -> &str {
        "side_panel"
    }

    fn description(&self) -> &str {
        "Manage session-scoped side panel pages. Use it to create, update, append to, focus, or delete markdown pages rendered in the right-hand side panel. Markdown pages support inline mermaid code blocks."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "write", "append", "focus", "delete"],
                    "description": "What to do with the session side panel"
                },
                "page_id": {
                    "type": "string",
                    "description": "Stable page identifier (letters, digits, underscore, dash, dot). Required for write/append/focus/delete."
                },
                "title": {
                    "type": "string",
                    "description": "Optional page title shown in the side panel header. Used by write/append."
                },
                "content": {
                    "type": "string",
                    "description": "Markdown content to write or append. Required for write/append. Mermaid code blocks render inline in the side panel."
                },
                "focus": {
                    "type": "boolean",
                    "description": "Whether to focus/show the target page after write/append (default: true)."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: SidePanelInput = serde_json::from_value(input)?;
        let focus = params.focus.unwrap_or(true);

        let snapshot = match params.action.as_str() {
            "status" => crate::side_panel::snapshot_for_session(&ctx.session_id)?,
            "write" => crate::side_panel::write_markdown_page(
                &ctx.session_id,
                params
                    .page_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("page_id is required for write"))?,
                params.title.as_deref(),
                params
                    .content
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("content is required for write"))?,
                focus,
            )?,
            "append" => crate::side_panel::append_markdown_page(
                &ctx.session_id,
                params
                    .page_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("page_id is required for append"))?,
                params.title.as_deref(),
                params
                    .content
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("content is required for append"))?,
                focus,
            )?,
            "focus" => crate::side_panel::focus_page(
                &ctx.session_id,
                params
                    .page_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("page_id is required for focus"))?,
            )?,
            "delete" => crate::side_panel::delete_page(
                &ctx.session_id,
                params
                    .page_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("page_id is required for delete"))?,
            )?,
            other => anyhow::bail!("unknown side_panel action: {}", other),
        };

        if params.action != "status" {
            Bus::global().publish(BusEvent::SidePanelUpdated(SidePanelUpdated {
                session_id: ctx.session_id.clone(),
                snapshot: snapshot.clone(),
            }));
        }

        Ok(ToolOutput::new(crate::side_panel::status_output(&snapshot))
            .with_title("side_panel")
            .with_metadata(serde_json::to_value(&snapshot)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn side_panel_tool_writes_page() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", temp.path());

        let tool = SidePanelTool::new();
        let output = tool
            .execute(
                json!({
                    "action": "write",
                    "page_id": "notes",
                    "title": "Notes",
                    "content": "# Notes"
                }),
                ToolContext {
                    session_id: "ses_side_panel_tool".to_string(),
                    message_id: "msg1".to_string(),
                    tool_call_id: "tool1".to_string(),
                    working_dir: None,
                    stdin_request_tx: None,
                    graceful_shutdown_signal: None,
                    execution_mode: crate::tool::ToolExecutionMode::AgentTurn,
                },
            )
            .await
            .expect("tool execute");

        assert!(output.output.contains("notes"));

        if let Some(prev_home) = prev_home {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }
}
