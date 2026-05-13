use super::{Tool, ToolContext, ToolOutput};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use jcode_pr_watch_core::{PrTarget, PrWatchState, normalize_watch_state_json};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

pub struct PrWatchTool;

impl PrWatchTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PrWatchAction {
    Start,
    Status,
    List,
    PollNow,
    Stop,
    Readiness,
    Handoff,
}

#[derive(Debug, Deserialize)]
struct PrWatchInput {
    action: PrWatchAction,
    repo: Option<String>,
    pr: Option<u64>,
    watch_id: Option<String>,
    dry_run: Option<bool>,
}

#[async_trait]
impl Tool for PrWatchTool {
    fn name(&self) -> &str {
        "pr_watch"
    }

    fn description(&self) -> &str {
        "Read-only PR feedback watch state. Start a local watch state, list watches, show status, or compute readiness. No GitHub network calls, scheduling, pushes, comments, thread resolution, or merges are performed in this phase."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["start", "status", "list", "poll_now", "stop", "readiness", "handoff"],
                    "description": "Action. This phase is read-only/local-state only; poll_now does not contact GitHub yet."
                },
                "repo": {"type": "string", "description": "Repository in owner/name form."},
                "pr": {"type": "integer", "description": "Pull request number."},
                "watch_id": {"type": "string", "description": "Existing watch ID, e.g. owner-repo-pr-123."},
                "dry_run": {"type": "boolean", "description": "Preview changes without writing state."}
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: PrWatchInput = serde_json::from_value(input)?;
        let root = ctx
            .working_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        let store = watch_dir(&root);
        match params.action {
            PrWatchAction::Start => start_watch(&store, params),
            PrWatchAction::List => list_watches(&store),
            PrWatchAction::Status
            | PrWatchAction::Readiness
            | PrWatchAction::Handoff
            | PrWatchAction::PollNow => status_like(&store, params),
            PrWatchAction::Stop => stop_watch(&store, params),
        }
    }
}

fn watch_dir(root: &Path) -> PathBuf {
    root.join(".jcode").join("pr-feedback-watch")
}

fn state_path(store: &Path, watch_id: &str) -> PathBuf {
    store.join(format!("{watch_id}-state.json"))
}

fn target_from_params(params: &PrWatchInput) -> Result<PrTarget> {
    let repo = params.repo.clone().context("repo is required")?;
    let number = params.pr.context("pr is required")?;
    Ok(PrTarget { repo, number })
}

fn start_watch(store: &Path, params: PrWatchInput) -> Result<ToolOutput> {
    let target = target_from_params(&params)?;
    let state = PrWatchState::new(target);
    let path = state_path(store, &state.watch_id);
    let would_write = !params.dry_run.unwrap_or(false);
    if would_write {
        fs::create_dir_all(store)?;
        if path.exists() {
            bail!("watch state already exists: {}", path.display());
        }
        fs::write(&path, serde_json::to_vec_pretty(&state)?)?;
    }
    Ok(ToolOutput::new(format!(
        "PR watch initialized: {}\nPath: {}\nMode: local state only, no GitHub polling yet{}",
        state.watch_id,
        path.display(),
        if would_write {
            ""
        } else {
            "\nDry run: no file written"
        }
    ))
    .with_title(format!("watch {}", state.watch_id))
    .with_metadata(json!({"watch": state, "path": path, "written": would_write})))
}

fn list_watches(store: &Path) -> Result<ToolOutput> {
    let states = load_all_states(store)?;
    let mut lines = vec![format!("{} PR watches", states.len())];
    for (path, state) in &states {
        lines.push(format!(
            "- {}: {}/#{} quiet={}/{} actionable={} status={:?} path={}",
            state.watch_id,
            state.pr.repo,
            state.pr.number,
            state.polling.quiet_cycles,
            state.polling.required_quiet_cycles,
            state.pending_actionable.len(),
            state.last_cycle.status,
            path.display()
        ));
    }
    Ok(ToolOutput::new(lines.join("\n"))
        .with_title(format!("{} watches", states.len()))
        .with_metadata(json!({"watches": states.into_iter().map(|(_, s)| s).collect::<Vec<_>>() })))
}

fn status_like(store: &Path, params: PrWatchInput) -> Result<ToolOutput> {
    let state = load_state_for_params(store, &params)?;
    let readiness = state.readiness();
    let text = format!(
        "PR watch: {}\nRepo: {}\nPR: #{}\nState: {:?}\nReadiness: {:?}\nQuiet cycles: {}/{}\nActionable: {}\nPending checks: {}\nFailed checks: {}\nPolicy: local_fix={}, commit={}, push={}, comment={}, resolve_threads={}\nNote: poll_now is a local-state placeholder in this phase and does not contact GitHub.",
        state.watch_id,
        state.pr.repo,
        state.pr.number,
        state.last_cycle.status,
        readiness,
        state.polling.quiet_cycles,
        state.polling.required_quiet_cycles,
        state.pending_actionable.len(),
        state.last_cycle.pending_check_count,
        state.last_cycle.failed_check_count,
        state.policy.local_fix,
        state.policy.commit,
        state.policy.push,
        state.policy.comment,
        state.policy.resolve_threads,
    );
    Ok(ToolOutput::new(text)
        .with_title(format!("{} {:?}", state.watch_id, readiness))
        .with_metadata(json!({"watch": state, "readiness": readiness})))
}

fn stop_watch(store: &Path, params: PrWatchInput) -> Result<ToolOutput> {
    let mut state = load_state_for_params(store, &params)?;
    state.terminal = true;
    state.stop_reason = Some("stopped_by_pr_watch_tool".to_string());
    let path = state_path(store, &state.watch_id);
    let would_write = !params.dry_run.unwrap_or(false);
    if would_write {
        fs::write(&path, serde_json::to_vec_pretty(&state)?)?;
    }
    Ok(ToolOutput::new(format!(
        "PR watch stopped: {}{}",
        state.watch_id,
        if would_write {
            ""
        } else {
            "\nDry run: no file written"
        }
    ))
    .with_title(format!("stopped {}", state.watch_id))
    .with_metadata(json!({"watch": state, "written": would_write})))
}

fn load_state_for_params(store: &Path, params: &PrWatchInput) -> Result<PrWatchState> {
    let watch_id = match &params.watch_id {
        Some(id) => id.clone(),
        None => target_from_params(params)?.watch_id(),
    };
    let path = state_path(store, &watch_id);
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    normalize_watch_state_json(&text).with_context(|| format!("failed to parse {}", path.display()))
}

fn load_all_states(store: &Path) -> Result<Vec<(PathBuf, PrWatchState)>> {
    let mut states = Vec::new();
    if !store.exists() {
        return Ok(states);
    }
    for entry in fs::read_dir(store)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if !path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .ends_with("-state.json")
        {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        if let Ok(state) = normalize_watch_state_json(&text) {
            states.push((path, state));
        }
    }
    states.sort_by(|a, b| a.1.watch_id.cmp(&b.1.watch_id));
    Ok(states)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_path_uses_watch_id() {
        assert_eq!(
            state_path(Path::new("/tmp/watch"), "owner-repo-pr-1"),
            PathBuf::from("/tmp/watch/owner-repo-pr-1-state.json")
        );
    }

    #[test]
    fn schema_lists_read_only_actions() {
        let schema = PrWatchTool::new().parameters_schema();
        let actions = schema
            .pointer("/properties/action/enum")
            .unwrap()
            .as_array()
            .unwrap();
        assert!(actions.iter().any(|value| value == "start"));
        assert!(actions.iter().any(|value| value == "poll_now"));
        assert!(!actions.iter().any(|value| value == "authorize"));
    }
}
