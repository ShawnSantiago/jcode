//! Background task management tool
//!
//! Allows the agent to list, check status, get output, and cancel background tasks.

use super::{Tool, ToolContext, ToolOutput};
use crate::background;
use crate::bus::BackgroundTaskStatus;
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

fn default_watch_notify() -> bool {
    true
}

fn default_watch_wake() -> bool {
    true
}

pub struct BgTool;

impl BgTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct BgInput {
    /// Action to perform: "list", "status", "output", "cancel", "cleanup", "watch"
    action: String,
    /// Task ID (required for status, output, cancel, watch)
    #[serde(default)]
    task_id: Option<String>,
    /// Max age in hours for cleanup (default: 24)
    #[serde(default)]
    max_age_hours: Option<u64>,
    /// Whether to notify on completion when using watch (default: true)
    #[serde(default = "default_watch_notify")]
    notify: bool,
    /// Whether to wake on completion when using watch (default: true)
    #[serde(default = "default_watch_wake")]
    wake: bool,
}

#[async_trait]
impl Tool for BgTool {
    fn name(&self) -> &str {
        "bg"
    }

    fn description(&self) -> &str {
        "Manage background tasks."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "status", "output", "cancel", "cleanup", "watch"],
                    "description": "Action."
                },
                "task_id": {
                    "type": "string",
                    "description": "Task ID."
                },
                "max_age_hours": {
                    "type": "integer",
                    "description": "Cleanup age in hours."
                },
                "notify": {
                    "type": "boolean",
                    "description": "When using action='watch', whether to notify on completion. Defaults to true."
                },
                "wake": {
                    "type": "boolean",
                    "description": "When using action='watch', whether to wake on completion. Defaults to true."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let params: BgInput = serde_json::from_value(input)?;
        let manager = background::global();

        match params.action.as_str() {
            "list" => {
                let tasks: Vec<background::TaskStatusFile> = manager.list().await;
                if tasks.is_empty() {
                    return Ok(ToolOutput::new("No background tasks found.").with_title("bg list"));
                }

                let mut output = String::from("Background Tasks:\n\n");
                output.push_str(&format!(
                    "{:<12} {:<28} {:<10} {:<12} {:<10} {:<28} {}\n",
                    "TASK_ID", "NAME", "TOOL", "STATUS", "DURATION", "PROGRESS", "SESSION"
                ));
                output.push_str(&"-".repeat(121));
                output.push('\n');

                for task in tasks {
                    let duration = task
                        .duration_secs
                        .map(|d| format!("{:.1}s", d))
                        .unwrap_or_else(|| "running".to_string());
                    let status = match task.status {
                        BackgroundTaskStatus::Running => "running",
                        BackgroundTaskStatus::Completed => "completed",
                        BackgroundTaskStatus::Superseded => "superseded",
                        BackgroundTaskStatus::Failed => "failed",
                    };
                    let progress = task
                        .progress
                        .as_ref()
                        .map(|progress| crate::background::format_progress_display(progress, 10))
                        .unwrap_or_else(|| "-".to_string());
                    let display_name = crate::message::background_task_display_label(
                        &task.tool_name,
                        task.display_name.as_deref(),
                    );
                    output.push_str(&format!(
                        "{:<12} {:<28} {:<10} {:<12} {:<10} {:<28} {}\n",
                        task.task_id,
                        crate::util::truncate_str(&display_name, 28),
                        task.tool_name,
                        status,
                        duration,
                        crate::util::truncate_str(&progress, 28),
                        &task.session_id[..8.min(task.session_id.len())]
                    ));
                }

                Ok(ToolOutput::new(output).with_title("bg list"))
            }

            "status" => {
                let task_id = params
                    .task_id
                    .ok_or_else(|| anyhow::anyhow!("task_id is required for status action"))?;

                match manager.status(&task_id).await {
                    Some(task) => {
                        let status_str = match task.status {
                            BackgroundTaskStatus::Running => "running",
                            BackgroundTaskStatus::Completed => "completed",
                            BackgroundTaskStatus::Superseded => "superseded",
                            BackgroundTaskStatus::Failed => "failed",
                        };

                        let mut output = format!(
                            "Task: {}\n\
                             Name: {}\n\
                             Tool: {}\n\
                             Status: {}\n\
                             Session: {}\n\
                             Started: {}\n",
                            task.task_id,
                            crate::message::background_task_display_label(
                                &task.tool_name,
                                task.display_name.as_deref(),
                            ),
                            task.tool_name,
                            status_str,
                            task.session_id,
                            task.started_at,
                        );

                        if let Some(completed) = task.completed_at {
                            output.push_str(&format!("Completed: {}\n", completed));
                        }
                        if let Some(duration) = task.duration_secs {
                            output.push_str(&format!("Duration: {:.2}s\n", duration));
                        }
                        if let Some(exit_code) = task.exit_code {
                            output.push_str(&format!("Exit code: {}\n", exit_code));
                        }
                        if let Some(progress) = task.progress.as_ref() {
                            output.push_str(&format!(
                                "Progress: {}\n",
                                crate::background::format_progress_display(progress, 18)
                            ));
                            output
                                .push_str(&format!("Progress updated: {}\n", progress.updated_at));
                        }
                        output.push_str(&format!("Notify: {}\n", task.notify));
                        output.push_str(&format!("Wake: {}\n", task.wake));
                        if let Some(error) = task.error.clone() {
                            output.push_str(&format!("Error: {}\n", error));
                        }

                        if matches!(task.status, BackgroundTaskStatus::Failed) {
                            crate::logging::warn(&format!(
                                "[tool:bg] task {} ({}) failed in session {} exit_code={:?} error={}",
                                task.task_id,
                                task.tool_name,
                                task.session_id,
                                task.exit_code,
                                task.error.as_deref().unwrap_or("<none>")
                            ));
                        }

                        Ok(ToolOutput::new(output)
                            .with_title(format!("bg status {}", task_id))
                            .with_metadata(json!({
                                "task_id": task.task_id,
                                "display_name": task.display_name,
                                "status": status_str,
                                "exit_code": task.exit_code,
                                "progress": task.progress,
                            })))
                    }
                    None => Err(anyhow::anyhow!("Task not found: {}", task_id)),
                }
            }

            "output" => {
                let task_id = params
                    .task_id
                    .ok_or_else(|| anyhow::anyhow!("task_id is required for output action"))?;

                let output_result: Option<String> = manager.output(&task_id).await;
                match output_result {
                    Some(output) => {
                        let truncated: String = if output.len() > 50000 {
                            crate::logging::warn(&format!(
                                "[tool:bg] truncated output for task {} at 50000 bytes",
                                task_id
                            ));
                            format!(
                                "{}...\n\n(Output truncated. Use `read` tool on the output file for full content)",
                                crate::util::truncate_str(&output, 50000)
                            )
                        } else {
                            output
                        };
                        Ok(ToolOutput::new(truncated).with_title(format!("bg output {}", task_id)))
                    }
                    None => Err(anyhow::anyhow!(
                        "Output not found for task: {}. Task may not exist or output file was deleted.",
                        task_id
                    )),
                }
            }

            "cancel" => {
                let task_id = params
                    .task_id
                    .ok_or_else(|| anyhow::anyhow!("task_id is required for cancel action"))?;

                match manager.cancel(&task_id).await? {
                    true => Ok(ToolOutput::new(format!("Task {} cancelled.", task_id))
                        .with_title(format!("bg cancel {}", task_id))),
                    false => Err(anyhow::anyhow!(
                        "Task {} not found or already completed.",
                        task_id
                    )),
                }
            }

            "cleanup" => {
                let max_age = params.max_age_hours.unwrap_or(24);
                let removed = manager.cleanup(max_age).await?;
                Ok(ToolOutput::new(format!(
                    "Cleaned up {} old task files (older than {} hours).",
                    removed, max_age
                ))
                .with_title("bg cleanup"))
            }

            "watch" => {
                let task_id = params
                    .task_id
                    .ok_or_else(|| anyhow::anyhow!("task_id is required for watch action"))?;

                match manager
                    .update_delivery(&task_id, params.notify, params.wake)
                    .await?
                {
                    Some(task) => {
                        let status_str = match task.status {
                            BackgroundTaskStatus::Running => "running",
                            BackgroundTaskStatus::Completed => "completed",
                            BackgroundTaskStatus::Superseded => "superseded",
                            BackgroundTaskStatus::Failed => "failed",
                        };
                        Ok(ToolOutput::new(format!(
                            "Updated background task delivery for {}.\nStatus: {}\nNotify: {}\nWake: {}",
                            task_id, status_str, task.notify, task.wake
                        ))
                        .with_title(format!("bg watch {}", task_id))
                        .with_metadata(json!({
                            "task_id": task.task_id,
                            "status": status_str,
                            "notify": task.notify,
                            "wake": task.wake,
                        })))
                    }
                    None => Err(anyhow::anyhow!("Task not found: {}", task_id)),
                }
            }

            _ => Err(anyhow::anyhow!(
                "Unknown action: {}. Valid actions: list, status, output, cancel, cleanup, watch",
                params.action
            )),
        }
    }
}
