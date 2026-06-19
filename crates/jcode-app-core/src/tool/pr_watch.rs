use super::{Tool, ToolContext, ToolExecutionMode, ToolOutput};
use crate::ambient::{AmbientManager, Priority, ScheduleRequest, ScheduleTarget, ScheduledItem};
use crate::session::Session;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use jcode_pr_watch_core::{
    ActionRequiredHandoffStatus, ActionableItem, AuthorizationGrant, CheckRunState, CycleOutcome,
    Marker, PrTarget, PrWatchEventMode, PrWatchState, ResolutionAttemptStatus, SurfaceError,
    ThreadResolutionAttempt, ValidationEvidence, WatchEvent, WriteScope,
    normalize_watch_state_json, parse_gh_checks, parse_gh_issue_comments, parse_gh_pr_view,
    parse_gh_review_comments, parse_gh_review_threads, parse_gh_reviews,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

pub struct PrWatchTool;

impl PrWatchTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PrWatchAction {
    Start,
    Status,
    List,
    PollNow,
    Monitor,
    Authorize,
    Revoke,
    Reschedule,
    Stop,
    Readiness,
    Handoff,
    AckBaseline,
    ResolveAddressed,
    WebhookStatus,
    WebhookDoctor,
    WebhookHeartbeat,
}

#[derive(Debug, Deserialize)]
struct PrWatchInput {
    action: PrWatchAction,
    repo: Option<String>,
    pr: Option<u64>,
    watch_id: Option<String>,
    dry_run: Option<bool>,
    #[serde(default)]
    schedule_next: bool,
    #[serde(default)]
    poll_interval_seconds: Option<u64>,
    #[serde(default)]
    quiet_cycles_required: Option<u64>,
    #[serde(default)]
    max_runtime_seconds: Option<u64>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    expires_in_minutes: Option<u64>,
    #[serde(default)]
    single_use: Option<bool>,
    #[serde(default)]
    grant_id: Option<String>,
    #[serde(default)]
    thread_ids: Vec<String>,
    #[serde(default)]
    head_sha: Option<String>,
    #[serde(default)]
    commit_sha: Option<String>,
    #[serde(default)]
    validation: Vec<ValidationEvidence>,
    #[serde(default)]
    expected_fingerprint: Option<String>,
    #[serde(default)]
    expected_cycle_number: Option<u64>,
    #[serde(default)]
    no_code_resolution: bool,
    #[serde(default)]
    event_mode: Option<PrWatchEventMode>,
    #[serde(default)]
    fallback_heartbeat_seconds: Option<u64>,
    #[serde(default)]
    webhook_url_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolveReviewThreadOutcome {
    Resolved,
    AlreadyResolved,
    NotResolved,
    MalformedResponse(String),
}

const DEFAULT_MONITOR_MAX_RUNTIME_SECONDS: u64 = 540;
const MAX_MONITOR_MAX_RUNTIME_SECONDS: u64 = 900;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PrWatchSchedulePayload {
    tool: String,
    watch_id: String,
    repo: String,
    pr: u64,
    action: String,
    state_file: String,
    poll_interval_seconds: u64,
    quiet_cycles_required: u64,
    max_runtime_seconds: u64,
    readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PrWatchWebhookHeartbeatPayload {
    tool: String,
    watch_id: String,
    repo: String,
    pr: u64,
    action: String,
    state_file: String,
    heartbeat_seconds: u64,
    readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PrWatchHandoffPayload {
    tool: String,
    watch_id: String,
    repo: String,
    pr: u64,
    action: String,
    state_file: String,
    fingerprint: String,
    cycle_number: u64,
    readonly: bool,
}

impl PrWatchHandoffPayload {
    fn new(state: &PrWatchState, fingerprint: String) -> Self {
        Self {
            tool: "pr_watch".to_string(),
            watch_id: state.watch_id.clone(),
            repo: state.pr.repo.clone(),
            pr: state.pr.number,
            action: "handoff".to_string(),
            state_file: state_file_for_watch(&state.watch_id),
            fingerprint,
            cycle_number: state.polling.cycle_number,
            readonly: false,
        }
    }

    fn from_scheduled_item(item: &ScheduledItem) -> Result<Option<Self>> {
        let Some(value) = item.schedule_payload.clone() else {
            return Ok(None);
        };
        if value.get("tool").and_then(Value::as_str) != Some("pr_watch")
            || value.get("action").and_then(Value::as_str) != Some("handoff")
        {
            return Ok(None);
        }
        serde_json::from_value(value)
            .map(Some)
            .with_context(|| format!("invalid pr_watch handoff payload on {}", item.id))
    }
}

impl PrWatchSchedulePayload {
    fn for_action(state: &PrWatchState, action: &str, max_runtime_seconds: u64) -> Self {
        Self {
            tool: "pr_watch".to_string(),
            watch_id: state.watch_id.clone(),
            repo: state.pr.repo.clone(),
            pr: state.pr.number,
            action: action.to_string(),
            state_file: state_file_for_watch(&state.watch_id),
            poll_interval_seconds: state.polling.poll_interval_seconds,
            quiet_cycles_required: state.polling.required_quiet_cycles,
            max_runtime_seconds,
            readonly: true,
        }
    }

    fn validate_against_state(&self, state: &PrWatchState) -> Result<()> {
        if self.tool != "pr_watch" {
            bail!("scheduled payload tool must be pr_watch");
        }
        if !self.readonly {
            bail!("scheduled pr_watch payload must be read-only");
        }
        if !matches!(
            self.action.as_str(),
            "ack_baseline" | "poll_now" | "monitor" | "webhook_heartbeat"
        ) {
            bail!(
                "scheduled pr_watch action is not read-only: {}",
                self.action
            );
        }
        if self.watch_id != state.watch_id
            || self.repo != state.pr.repo
            || self.pr != state.pr.number
        {
            bail!("scheduled pr_watch payload does not match watch state");
        }
        Ok(())
    }

    fn from_scheduled_item(item: &ScheduledItem) -> Result<Option<Self>> {
        let Some(value) = item.schedule_payload.clone() else {
            return Ok(None);
        };
        if value.get("tool").and_then(Value::as_str) != Some("pr_watch") {
            return Ok(None);
        }
        let payload: Self = serde_json::from_value(value)
            .with_context(|| format!("invalid pr_watch schedule payload on {}", item.id))?;
        if !payload.readonly {
            bail!(
                "invalid pr_watch schedule payload on {}: readonly=false",
                item.id
            );
        }
        if !matches!(
            payload.action.as_str(),
            "ack_baseline" | "poll_now" | "monitor" | "webhook_heartbeat"
        ) {
            bail!(
                "invalid pr_watch schedule payload on {}: action={}",
                item.id,
                payload.action
            );
        }
        Ok(Some(payload))
    }
}

impl PrWatchWebhookHeartbeatPayload {
    fn new(state: &PrWatchState, heartbeat_seconds: u64) -> Self {
        Self {
            tool: "pr_watch".to_string(),
            watch_id: state.watch_id.clone(),
            repo: state.pr.repo.clone(),
            pr: state.pr.number,
            action: "webhook_heartbeat".to_string(),
            state_file: state_file_for_watch(&state.watch_id),
            heartbeat_seconds,
            readonly: true,
        }
    }

    fn validate_against_state(&self, state: &PrWatchState) -> Result<()> {
        if self.tool != "pr_watch" || self.action != "webhook_heartbeat" || !self.readonly {
            bail!("webhook heartbeat payload must be read-only pr_watch webhook_heartbeat");
        }
        if self.watch_id != state.watch_id
            || self.repo != state.pr.repo
            || self.pr != state.pr.number
        {
            bail!("webhook heartbeat payload does not match watch state");
        }
        if self.heartbeat_seconds < 300 {
            bail!("webhook heartbeat must be at least 300 seconds");
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for PrWatchTool {
    fn name(&self) -> &str {
        "pr_watch"
    }

    fn description(&self) -> &str {
        "PR feedback watch state. Start a local watch, run read-only gh collection, schedule follow-up polls, list watches, show status, compute readiness, or resolve addressed review threads only via the grant-gated resolve_addressed action. Polling and monitor actions remain read-only: no pushes, comments, thread resolution, or merges are performed by watch cycles."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["start", "status", "list", "poll_now", "monitor", "ack_baseline", "authorize", "revoke", "reschedule", "stop", "readiness", "handoff", "resolve_addressed", "webhook_status", "webhook_doctor", "webhook_heartbeat"],
                    "description": "Action. poll_now/monitor perform read-only gh CLI collection and update local state; no GitHub mutations are performed. authorize/revoke only record local grants for a separate explicit remediation workflow."
                },
                "repo": {"type": "string", "description": "Repository in owner/name form."},
                "pr": {"type": "integer", "description": "Pull request number."},
                "watch_id": {"type": "string", "description": "Existing watch ID, e.g. owner-repo-pr-123."},
                "dry_run": {"type": "boolean", "description": "Preview changes without writing state."},
                "schedule_next": {"type": "boolean", "description": "If true, schedule the next visible poll wakeup after start, poll_now, or ack_baseline."},
                "poll_interval_seconds": {"type": "integer", "description": "Interval for the next scheduled poll. Defaults to state polling interval."},
                "quiet_cycles_required": {"type": "integer", "description": "Quiet cycles required before the monitor stops as satisfied. Defaults to watch state or 3."},
                "max_runtime_seconds": {"type": "integer", "description": "Maximum monitor runtime budget. Single-cycle monitor caps this to 900 and records the bounded value."},
                "target": {"type": "string", "enum": ["resume", "spawn"], "description": "Schedule delivery target. Defaults to resuming the current session."},
                "scopes": {"type": "array", "items": {"type": "string", "enum": ["local_fix", "commit", "push", "comment", "resolve_threads"]}, "description": "Authorization scopes for action=authorize or action=revoke. merge is intentionally not supported."},
                "reason": {"type": "string", "description": "Required human/operator reason for action=authorize."},
                "expires_in_minutes": {"type": "integer", "description": "Grant lifetime for action=authorize. Defaults to 120, capped at 1440."},
                "single_use": {"type": "boolean", "description": "Whether the grant is intended for one remediation use."},
                "grant_id": {"type": "string", "description": "Specific grant id for action=revoke."}
                ,"thread_ids": {"type": "array", "items": {"type": "string"}, "description": "Review thread IDs to resolve for action=resolve_addressed."}
                ,"head_sha": {"type": "string", "description": "Expected current PR head SHA for action=resolve_addressed."}
                ,"commit_sha": {"type": "string", "description": "Commit SHA containing the addressed fix for action=resolve_addressed."}
                ,"expected_fingerprint": {"type": "string", "description": "Expected current actionable fingerprint for action=resolve_addressed; prevents resolving stale handoffs."}
                ,"expected_cycle_number": {"type": "integer", "description": "Expected watch polling cycle number for action=resolve_addressed; prevents resolving stale handoffs."}
                ,"no_code_resolution": {"type": "boolean", "description": "Set true only when resolving without a code commit; requires a non-empty reason and no commit_sha."}
                ,"event_mode": {"type": "string", "enum": ["polling", "webhook", "hybrid"], "description": "Event source mode. polling keeps existing scheduled monitor behavior; webhook suppresses normal monitor polling; hybrid keeps polling and also accepts webhook wakeups."}
                ,"fallback_heartbeat_seconds": {"type": "integer", "description": "Optional low-frequency read-only webhook heartbeat interval. Disabled when omitted."}
                ,"webhook_url_hint": {"type": "string", "description": "Optional public webhook URL hint for status/doctor display."}
                ,"validation": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["at", "command", "status"],
                        "properties": {
                            "at": {"type": "string", "description": "Timestamp for the validation run."},
                            "command": {"type": "string", "description": "Validation command or check name."},
                            "status": {"type": "string", "description": "Validation status, for example passed or failed."},
                            "summary": {"type": ["string", "null"], "description": "Optional concise validation result summary."}
                        },
                        "additionalProperties": false
                    },
                    "description": "Validation evidence for action=resolve_addressed."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: PrWatchInput = serde_json::from_value(input)?;
        validate_event_mode_change_action(&params)?;
        let root = ctx
            .working_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        let store = watch_dir(&root);
        match params.action {
            PrWatchAction::Start => start_watch(&root, &store, params, &ctx),
            PrWatchAction::List => list_watches(&store),
            PrWatchAction::PollNow => poll_now(&root, &store, params, &ctx).await,
            PrWatchAction::Monitor => monitor_once(&root, &store, params, &ctx).await,
            PrWatchAction::AckBaseline => ack_baseline(&root, &store, params, &ctx).await,
            PrWatchAction::Authorize => authorize_watch(&store, params, &ctx),
            PrWatchAction::Revoke => revoke_watch_grant(&store, params, &ctx),
            PrWatchAction::Reschedule => reschedule_watch(&store, params, &ctx),
            PrWatchAction::Status => status_like(&store, params),
            PrWatchAction::Readiness => readiness_report(&store, params),
            PrWatchAction::Handoff => handoff_report(&store, params),
            PrWatchAction::ResolveAddressed => resolve_addressed(&root, &store, params, &ctx).await,
            PrWatchAction::WebhookStatus => webhook_status(&store, params),
            PrWatchAction::WebhookDoctor => webhook_doctor(&root, &store, params).await,
            PrWatchAction::WebhookHeartbeat => webhook_heartbeat(&root, &store, params, &ctx).await,
            PrWatchAction::Stop => stop_watch(&store, params),
        }
    }
}

fn validate_event_mode_change_action(params: &PrWatchInput) -> Result<()> {
    if params.event_mode.is_some()
        && !matches!(
            params.action,
            PrWatchAction::Start | PrWatchAction::Reschedule
        )
    {
        bail!(
            "event_mode can only be changed by start or reschedule so the webhook index stays in sync"
        );
    }
    Ok(())
}

fn watch_dir(root: &Path) -> PathBuf {
    root.join(".jcode").join("pr-feedback-watch")
}

fn state_path(store: &Path, watch_id: &str) -> PathBuf {
    store.join(format!("{watch_id}-state.json"))
}

fn state_file_for_watch(watch_id: &str) -> String {
    format!(".jcode/pr-feedback-watch/{watch_id}-state.json")
}

fn webhook_runtime_dir() -> Result<PathBuf> {
    Ok(crate::storage::jcode_dir()?.join("pr-watch"))
}

fn webhook_index_path() -> Result<PathBuf> {
    Ok(webhook_runtime_dir()?.join("webhook-index.json"))
}

fn webhook_health_path() -> Result<PathBuf> {
    Ok(webhook_runtime_dir()?.join("webhook-daemon-health.json"))
}

fn webhook_pid_path() -> Result<PathBuf> {
    Ok(webhook_runtime_dir()?.join("webhook-daemon.pid"))
}

fn webhook_lock_path() -> Result<PathBuf> {
    Ok(webhook_runtime_dir()?.join("webhook-daemon.lock"))
}

fn webhook_index_lock_path() -> Result<PathBuf> {
    Ok(webhook_runtime_dir()?.join("webhook-index.lock"))
}

fn webhook_deliveries_path() -> Result<PathBuf> {
    Ok(webhook_runtime_dir()?.join("webhook-deliveries.json"))
}

fn webhook_delivery_log_path() -> Result<PathBuf> {
    Ok(webhook_runtime_dir()?.join("webhook-deliveries.jsonl"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebhookWatchIndexEntry {
    pub watch_id: String,
    pub repo: String,
    pub pr: u64,
    pub root_dir: String,
    pub state_path: String,
    pub event_mode: PrWatchEventMode,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct WebhookWatchIndex {
    #[serde(default)]
    entries: Vec<WebhookWatchIndexEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WebhookDaemonHealth {
    status: String,
    pid: u32,
    bind: String,
    port: u16,
    updated_at: String,
    #[serde(default)]
    last_delivery_id: Option<String>,
    #[serde(default)]
    last_event: Option<String>,
    #[serde(default)]
    last_result: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WebhookDeliveryRecord {
    delivery_id: String,
    event: String,
    seen_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct WebhookDeliveryStore {
    #[serde(default)]
    deliveries: Vec<WebhookDeliveryRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WebhookDeliveryLogRecord {
    at: String,
    delivery_id: String,
    event: String,
    repo: Option<String>,
    pr: Option<u64>,
    result: String,
    reason: String,
}

fn load_webhook_index() -> Result<WebhookWatchIndex> {
    let path = webhook_index_path()?;
    if !path.exists() {
        return Ok(WebhookWatchIndex::default());
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read webhook index {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("failed to parse webhook index {}", path.display()))
}

fn save_webhook_index(index: &WebhookWatchIndex) -> Result<()> {
    let path = webhook_index_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(index)?)?;
    fs::rename(&tmp, &path)
        .with_context(|| format!("failed to atomically replace {}", path.display()))?;
    Ok(())
}

fn acquire_webhook_index_lock() -> Result<WatchLock> {
    let path = webhook_index_lock_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    for _ in 0..50 {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let lock = WatchLock { path: path.clone() };
                writeln!(
                    file,
                    "pid={} at={} purpose=webhook_index",
                    std::process::id(),
                    now_iso()
                )?;
                return Ok(lock);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(err) => {
                return Err(err).with_context(|| format!("failed to create {}", path.display()));
            }
        }
    }
    bail!(
        "timed out waiting for webhook index lock {}",
        path.display()
    )
}

fn register_webhook_index_entry(root: &Path, store: &Path, state: &PrWatchState) -> Result<()> {
    if !matches!(
        state.webhook.mode,
        PrWatchEventMode::Webhook | PrWatchEventMode::Hybrid
    ) {
        return remove_webhook_index_entry(&state.watch_id);
    }
    let _lock = acquire_webhook_index_lock()?;
    let mut index = load_webhook_index()?;
    let entry = WebhookWatchIndexEntry {
        watch_id: state.watch_id.clone(),
        repo: state.pr.repo.clone(),
        pr: state.pr.number,
        root_dir: root.display().to_string(),
        state_path: state_path(store, &state.watch_id).display().to_string(),
        event_mode: state.webhook.mode.clone(),
        active: !state.terminal,
        updated_at: now_iso(),
    };
    index
        .entries
        .retain(|candidate| candidate.watch_id != state.watch_id);
    index.entries.push(entry);
    index.entries.sort_by(|a, b| a.watch_id.cmp(&b.watch_id));
    save_webhook_index(&index)
}

fn remove_webhook_index_entry(watch_id: &str) -> Result<()> {
    let _lock = acquire_webhook_index_lock()?;
    let mut index = load_webhook_index()?;
    let before = index.entries.len();
    index.entries.retain(|entry| entry.watch_id != watch_id);
    if index.entries.len() != before {
        save_webhook_index(&index)?;
    }
    Ok(())
}

fn repos_match(index_repo: &str, payload_repo: &str) -> bool {
    index_repo.eq_ignore_ascii_case(payload_repo)
}

fn active_webhook_entries_for_repo<'a>(
    index: &'a WebhookWatchIndex,
    repo: &str,
) -> Vec<&'a WebhookWatchIndexEntry> {
    index
        .entries
        .iter()
        .filter(|entry| entry.active && repos_match(&entry.repo, repo))
        .collect()
}

fn write_webhook_health(health: &WebhookDaemonHealth) -> Result<()> {
    let path = webhook_health_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_vec_pretty(health)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn process_is_alive(pid: u32) -> bool {
    jcode_base::platform::is_process_running(pid)
}

fn read_webhook_pid() -> Result<Option<u32>> {
    let path = webhook_pid_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    Ok(text.trim().parse::<u32>().ok())
}

fn load_webhook_deliveries() -> Result<WebhookDeliveryStore> {
    let path = webhook_deliveries_path()?;
    if !path.exists() {
        return Ok(WebhookDeliveryStore::default());
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read webhook deliveries {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("failed to parse webhook deliveries {}", path.display()))
}

fn delivery_already_seen(delivery: &VerifiedGithubDelivery) -> Result<bool> {
    Ok(load_webhook_deliveries()?
        .deliveries
        .iter()
        .any(|record| record.delivery_id == delivery.delivery_id))
}

fn remember_webhook_delivery(delivery: &VerifiedGithubDelivery) -> Result<bool> {
    let mut store = load_webhook_deliveries()?;
    if store
        .deliveries
        .iter()
        .any(|record| record.delivery_id == delivery.delivery_id)
    {
        return Ok(false);
    }
    let cutoff = Utc::now() - Duration::days(7);
    store.deliveries.retain(|record| {
        chrono::DateTime::parse_from_rfc3339(&record.seen_at)
            .map(|at| at.with_timezone(&Utc) >= cutoff)
            .unwrap_or(false)
    });
    store.deliveries.push(WebhookDeliveryRecord {
        delivery_id: delivery.delivery_id.clone(),
        event: delivery.event.clone(),
        seen_at: now_iso(),
    });
    if store.deliveries.len() > 10_000 {
        let drop_count = store.deliveries.len() - 10_000;
        store.deliveries.drain(0..drop_count);
    }
    let path = webhook_deliveries_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_vec_pretty(&store)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

fn append_webhook_delivery_log(
    delivery: &VerifiedGithubDelivery,
    result: &str,
    reason: &str,
) -> Result<()> {
    let path = webhook_delivery_log_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let record = WebhookDeliveryLogRecord {
        at: now_iso(),
        delivery_id: delivery.delivery_id.clone(),
        event: delivery.event.clone(),
        repo: delivery.repo.clone(),
        pr: delivery.pr,
        result: result.to_string(),
        reason: reason.to_string(),
    };
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{}", serde_json::to_string(&record)?)?;
    Ok(())
}

fn append_rejected_webhook_delivery_log(
    delivery_id: Option<&str>,
    event: Option<&str>,
    result: &str,
    reason: &str,
) -> Result<()> {
    let path = webhook_delivery_log_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let record = WebhookDeliveryLogRecord {
        at: now_iso(),
        delivery_id: delivery_id.unwrap_or("unknown").to_string(),
        event: event.unwrap_or("unknown").to_string(),
        repo: None,
        pr: None,
        result: result.to_string(),
        reason: reason.to_string(),
    };
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{}", serde_json::to_string(&record)?)?;
    Ok(())
}

fn read_webhook_health() -> Result<Option<WebhookDaemonHealth>> {
    let path = webhook_health_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read webhook health {}", path.display()))?;
    serde_json::from_str(&text)
        .map(Some)
        .with_context(|| format!("failed to parse webhook health {}", path.display()))
}

fn schedule_key_for_watch(watch_id: &str) -> String {
    format!("pr_watch:{watch_id}:monitor")
}

fn webhook_heartbeat_schedule_key_for_watch(watch_id: &str) -> String {
    format!("pr_watch:{watch_id}:webhook_heartbeat")
}

fn webhook_followup_schedule_key_for_watch(watch_id: &str) -> String {
    format!("pr_watch:{watch_id}:webhook_followup")
}

fn handoff_schedule_key_for_watch(watch_id: &str) -> String {
    format!("pr_watch:{watch_id}:action_required_handoff")
}

fn lock_path(store: &Path, watch_id: &str) -> PathBuf {
    store.join(format!("{watch_id}.lock"))
}

fn handoff_lock_path(store: &Path, watch_id: &str) -> PathBuf {
    store.join(format!("{watch_id}-handoff.lock"))
}

struct WatchLock {
    path: PathBuf,
}

impl Drop for WatchLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn acquire_watch_lock(store: &Path, watch_id: &str) -> Result<Option<WatchLock>> {
    fs::create_dir_all(store)?;
    let path = lock_path(store, watch_id);
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            let lock = WatchLock { path: path.clone() };
            writeln!(file, "pid={} at={}", std::process::id(), now_iso())?;
            Ok(Some(lock))
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to create {}", path.display())),
    }
}

fn acquire_handoff_lock(store: &Path, watch_id: &str) -> Result<Option<WatchLock>> {
    fs::create_dir_all(store)?;
    let path = handoff_lock_path(store, watch_id);
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            let lock = WatchLock { path: path.clone() };
            writeln!(
                file,
                "pid={} at={} purpose=action_required_handoff",
                std::process::id(),
                now_iso()
            )?;
            Ok(Some(lock))
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to create {}", path.display())),
    }
}

fn watch_locked_output(store: &Path, state: &PrWatchState, action: &str) -> ToolOutput {
    ToolOutput::new(format!(
        "PR watch {action} already running or locked: {}\nRepo: {}\nPR: #{}\nLock: {}\nNo state was changed.",
        state.watch_id,
        state.pr.repo,
        state.pr.number,
        lock_path(store, &state.watch_id).display()
    ))
    .with_title(format!("{} locked", state.watch_id))
    .with_metadata(json!({
        "watch": state,
        "watch_locked": true,
        "action": action,
        "written": false,
    }))
}

fn write_state_atomic(path: &Path, state: &PrWatchState) -> Result<()> {
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, serde_json::to_vec_pretty(state)?)?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to atomically replace {}", path.display()))?;
    Ok(())
}

fn target_from_params(params: &PrWatchInput) -> Result<PrTarget> {
    let repo = params.repo.clone().context("repo is required")?;
    let number = params.pr.context("pr is required")?;
    Ok(PrTarget { repo, number })
}

fn start_watch(
    root: &Path,
    store: &Path,
    params: PrWatchInput,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let target = target_from_params(&params)?;
    let mut state = PrWatchState::new(target);
    state.origin_session_id = Some(ctx.session_id.clone());
    state.root_dir = Some(root.display().to_string());
    let path = state_path(store, &state.watch_id);
    apply_schedule_fields(&mut state, &params);
    let would_write = !params.dry_run.unwrap_or(false);
    if would_write {
        fs::create_dir_all(store)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "watch state already exists or cannot be created: {}",
                    path.display()
                )
            })?;
        file.write_all(&serde_json::to_vec_pretty(&state)?)?;
    }
    let scheduled = maybe_schedule_next(ctx, &mut state, &params)?;
    if would_write {
        register_webhook_index_entry(root, store, &state)?;
        write_state_atomic(&path, &state)?;
    }
    Ok(ToolOutput::new(format!(
        "PR watch initialized: {}\nPath: {}\nMode: local state initialized. Use poll_now for read-only gh collection{}{}",
        state.watch_id,
        path.display(),
        scheduled.as_deref().map(|s| format!("\nScheduled: {s}")).unwrap_or_default(),
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

fn authorize_watch(store: &Path, params: PrWatchInput, ctx: &ToolContext) -> Result<ToolOutput> {
    let mut state = load_state_for_params(store, &params)?;
    let scopes = parse_write_scopes(params.scopes.as_deref())?;
    if scopes.is_empty() {
        bail!("authorize requires at least one scope");
    }
    let reason = params
        .reason
        .clone()
        .filter(|value| !value.trim().is_empty())
        .context("authorize requires a non-empty reason")?;
    let would_write = !params.dry_run.unwrap_or(false);
    let _lock = if would_write {
        match acquire_watch_lock(store, &state.watch_id)? {
            Some(lock) => Some(lock),
            None => return Ok(watch_locked_output(store, &state, "authorize")),
        }
    } else {
        None
    };
    let now = Utc::now();
    let expires_in_minutes = params.expires_in_minutes.unwrap_or(120).clamp(1, 24 * 60);
    let grant_id = format!("grant-{}", now.timestamp_millis());
    let grant = AuthorizationGrant {
        grant_id: grant_id.clone(),
        granted_at: now.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        expires_at: (now + Duration::minutes(expires_in_minutes as i64))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
        granted_by_session_id: ctx.session_id.clone(),
        scopes,
        single_use: params.single_use.unwrap_or(false),
        reason: Some(reason),
    };
    state.authorization.active_grants.push(grant.clone());
    state.updated_at = Some(now_iso());
    state.push_event(WatchEvent {
        at: now_iso(),
        kind: "grant_created".to_string(),
        data: json!({
            "grant_id": grant.grant_id,
            "scopes": grant.scopes,
            "expires_at": grant.expires_at,
            "single_use": grant.single_use,
            "reason": grant.reason,
            "read_only_watch_invariant": true,
        }),
    });
    let path = state_path(store, &state.watch_id);
    if would_write {
        write_state_atomic(&path, &state)?;
    }
    Ok(ToolOutput::new(format!(
        "PR watch grant recorded: {}\nRepo: {}\nPR: #{}\nGrant: {}\nScopes: {}\nExpires: {}\nNote: pr_watch poll_now/monitor/scheduled follow-ups remain read-only; this grant is for a separate explicit remediation workflow.{}",
        state.watch_id,
        state.pr.repo,
        state.pr.number,
        grant_id,
        format_scopes(&grant.scopes),
        grant.expires_at,
        if would_write { "" } else { "\nDry run: no file written" }
    ))
    .with_title(format!("authorized {}", state.watch_id))
    .with_metadata(json!({"watch": state, "grant_id": grant_id, "written": would_write})))
}

fn revoke_watch_grant(
    store: &Path,
    params: PrWatchInput,
    _ctx: &ToolContext,
) -> Result<ToolOutput> {
    let mut state = load_state_for_params(store, &params)?;
    let grant_id = params.grant_id.clone();
    let scopes = parse_write_scopes(params.scopes.as_deref())?;
    if grant_id.is_none() && scopes.is_empty() {
        bail!("revoke requires grant_id or at least one scope");
    }
    let would_write = !params.dry_run.unwrap_or(false);
    let _lock = if would_write {
        match acquire_watch_lock(store, &state.watch_id)? {
            Some(lock) => Some(lock),
            None => return Ok(watch_locked_output(store, &state, "revoke")),
        }
    } else {
        None
    };
    let before = state.authorization.active_grants.len();
    state.authorization.active_grants.retain(|grant| {
        if let Some(id) = &grant_id {
            return &grant.grant_id != id;
        }
        grant.scopes.is_disjoint(&scopes)
    });
    let revoked = before.saturating_sub(state.authorization.active_grants.len());
    state.updated_at = Some(now_iso());
    state.push_event(WatchEvent {
        at: now_iso(),
        kind: "grant_revoked".to_string(),
        data: json!({"grant_id": grant_id, "scopes": scopes, "revoked_count": revoked}),
    });
    let path = state_path(store, &state.watch_id);
    if would_write {
        write_state_atomic(&path, &state)?;
    }
    Ok(ToolOutput::new(format!(
        "PR watch grant revoke recorded: {}\nRevoked grants: {}{}",
        state.watch_id,
        revoked,
        if would_write {
            ""
        } else {
            "\nDry run: no file written"
        }
    ))
    .with_title(format!("revoked {}", state.watch_id))
    .with_metadata(json!({"watch": state, "revoked_count": revoked, "written": would_write})))
}

fn active_grant_for_scope<'a>(
    state: &'a PrWatchState,
    scope: WriteScope,
    session_id: &str,
) -> Option<&'a AuthorizationGrant> {
    let now = now_iso();
    state
        .authorization
        .active_grants
        .iter()
        .find(|grant| grant.grants(scope, &now, session_id))
}

fn consume_single_use_grant(state: &mut PrWatchState, grant_id: &str) -> bool {
    let before = state.authorization.active_grants.len();
    state
        .authorization
        .active_grants
        .retain(|grant| !(grant.single_use && grant.grant_id == grant_id));
    before != state.authorization.active_grants.len()
}

fn has_non_empty_commit_or_reason(params: &PrWatchInput) -> bool {
    params
        .commit_sha
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        || params
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
}

fn has_explicit_no_code_reason(params: &PrWatchInput) -> bool {
    if !params.no_code_resolution {
        return false;
    }
    params
        .commit_sha
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
        && params
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
}

fn has_duplicate_thread_ids(thread_ids: &[String]) -> bool {
    let mut seen = HashSet::new();
    thread_ids.iter().any(|id| !seen.insert(id))
}

fn commit_sha_matches_current_head(params: &PrWatchInput, current_head: &str) -> bool {
    params
        .commit_sha
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|commit_sha| commit_sha == current_head)
}

fn skipped_resolution_attempt(
    thread_id: &str,
    expected_head: &str,
    params: &PrWatchInput,
) -> ThreadResolutionAttempt {
    ThreadResolutionAttempt {
        thread_id: thread_id.to_string(),
        attempted_at: now_iso(),
        status: ResolutionAttemptStatus::Skipped,
        head_sha: Some(expected_head.to_string()),
        commit_sha: params.commit_sha.clone(),
        validation: params.validation.clone(),
        reason: params.reason.clone().unwrap_or_else(|| {
            "resolved after validated fix for addressed review feedback".to_string()
        }),
        error: Some("skipped due to previous failure in batch".to_string()),
    }
}

fn review_thread_had_prior_resolution_attempt(state: &PrWatchState, thread_id: &str) -> bool {
    state
        .last_resolution_attempts
        .iter()
        .any(|attempt| attempt.thread_id == thread_id)
}

fn review_thread_had_successful_resolution_attempt(state: &PrWatchState, thread_id: &str) -> bool {
    state.last_resolution_attempts.iter().any(|attempt| {
        attempt.thread_id == thread_id
            && matches!(
                attempt.status,
                ResolutionAttemptStatus::Resolved | ResolutionAttemptStatus::AlreadyResolved
            )
    })
}

fn review_threads_fetch_succeeded_at(state: &PrWatchState, collected_at: &str) -> bool {
    state
        .last_successful_fetch
        .get("review_threads")
        .is_some_and(|value| value == collected_at)
}

fn review_thread_marker_fingerprint(state: &PrWatchState, thread_ids: &[String]) -> Result<String> {
    let mut canonical = Vec::new();
    for id in thread_ids {
        let marker = state
            .last_seen
            .review_threads
            .get(id)
            .with_context(|| format!("unknown review thread id for freshness check: {id}"))?;
        canonical.push(json!({
            "id": id,
            "updated_at": marker.updated_at,
            "resolved": marker.resolved,
            "outdated": marker.outdated,
            "body_hash": marker.body_hash,
            "url": marker.url,
        }));
    }
    serde_json::to_vec(&canonical)
        .map(|bytes| sha256_hex(&bytes))
        .context("failed to serialize review thread freshness markers")
}

fn ensure_resolve_freshness_matches(state: &PrWatchState, params: &PrWatchInput) -> Result<()> {
    let expected_cycle = params.expected_cycle_number.context(
        "resolve_addressed requires expected_cycle_number from the current handoff/status",
    )?;
    if expected_cycle != state.polling.cycle_number {
        bail!(
            "resolve_addressed expected_cycle_number is stale: expected {}, current {}",
            expected_cycle,
            state.polling.cycle_number
        );
    }

    let expected_fingerprint = params
        .expected_fingerprint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context(
            "resolve_addressed requires expected_fingerprint from the current handoff/status",
        )?;
    let current_actionable_fingerprint = actionable_fingerprint(&state.pending_actionable);
    if current_actionable_fingerprint.as_deref() == Some(expected_fingerprint) {
        return Ok(());
    }
    let current_thread_fingerprint = review_thread_marker_fingerprint(state, &params.thread_ids)?;
    if current_thread_fingerprint != expected_fingerprint {
        bail!(
            "resolve_addressed expected_fingerprint is stale: expected {}, current actionable {}, current thread {}",
            expected_fingerprint,
            current_actionable_fingerprint.as_deref().unwrap_or("none"),
            current_thread_fingerprint
        );
    }
    Ok(())
}

fn validation_status_is_passing(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "pass" | "passed" | "success" | "succeeded" | "successful" | "ok"
    )
}

fn all_validation_evidence_is_passing(validation: &[ValidationEvidence]) -> bool {
    validation
        .iter()
        .all(|evidence| validation_status_is_passing(&evidence.status))
}

fn ensure_post_resolution_poll_cleared(state: &PrWatchState) -> Result<()> {
    if state.resolution_requires_post_poll {
        bail!(
            "resolve_addressed requires a post-resolution poll before retrying; run pr_watch action=poll_now first"
        );
    }
    Ok(())
}

fn merge_resolution_attempts(
    previous: &[ThreadResolutionAttempt],
    current: Vec<ThreadResolutionAttempt>,
) -> Vec<ThreadResolutionAttempt> {
    let current_thread_ids: HashSet<&str> = current
        .iter()
        .map(|attempt| attempt.thread_id.as_str())
        .collect();
    let mut merged: Vec<ThreadResolutionAttempt> = previous
        .iter()
        .filter(|attempt| {
            matches!(
                attempt.status,
                ResolutionAttemptStatus::Resolved | ResolutionAttemptStatus::AlreadyResolved
            ) && !current_thread_ids.contains(attempt.thread_id.as_str())
        })
        .cloned()
        .collect();
    merged.extend(current);
    merged
}

fn requeue_failed_resolution_threads(state: &PrWatchState, pending: &mut Vec<ActionableItem>) {
    let mut pending_ids: HashSet<String> = pending.iter().map(|item| item.id.clone()).collect();
    for attempt in &state.last_resolution_attempts {
        if !matches!(attempt.status, ResolutionAttemptStatus::Failed) {
            continue;
        }
        let Some(marker) = state.last_seen.review_threads.get(&attempt.thread_id) else {
            continue;
        };
        if marker.resolved || marker.outdated || !pending_ids.insert(attempt.thread_id.clone()) {
            continue;
        }
        pending.push(ActionableItem {
            id: attempt.thread_id.clone(),
            surface: "review_threads".to_string(),
            summary: attempt.error.clone().unwrap_or_else(|| {
                "Review thread resolution failed; retry or inspect manually".to_string()
            }),
            url: marker.url.clone(),
            path: None,
            status: Some("resolution_failed".to_string()),
            reason: Some("failed_resolution_retry".to_string()),
        });
    }
}

async fn resolve_addressed(
    root: &Path,
    store: &Path,
    params: PrWatchInput,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let mut state = load_state_for_params(store, &params)?;
    if let Some(expected_root) = state.root_dir.as_deref() {
        let current_root = root.display().to_string();
        if expected_root != current_root {
            bail!(
                "Watch state root mismatch for {}. Expected root: {}. Current root: {}. Refusing mutating resolve_addressed.",
                state.watch_id,
                expected_root,
                current_root
            );
        }
    }
    if params.thread_ids.is_empty() {
        bail!("resolve_addressed requires at least one thread_id");
    }
    if has_duplicate_thread_ids(&params.thread_ids) {
        bail!("resolve_addressed rejects duplicate thread_ids");
    }
    if params.validation.is_empty() && !params.dry_run.unwrap_or(false) {
        bail!("resolve_addressed requires validation evidence");
    }
    if !params.validation.is_empty() && !all_validation_evidence_is_passing(&params.validation) {
        bail!("resolve_addressed requires all validation evidence statuses to be passing");
    }
    let would_write = !params.dry_run.unwrap_or(false);
    let _lock = if would_write {
        match acquire_watch_lock(store, &state.watch_id)? {
            Some(lock) => Some(lock),
            None => return Ok(watch_locked_output(store, &state, "resolve_addressed")),
        }
    } else {
        None
    };
    if would_write {
        state = load_state_for_params(store, &params)?;
        if let Some(expected_root) = state.root_dir.as_deref() {
            let current_root = root.display().to_string();
            if expected_root != current_root {
                bail!(
                    "Watch state root mismatch for {}. Expected root: {}. Current root: {}. Refusing mutating resolve_addressed.",
                    state.watch_id,
                    expected_root,
                    current_root
                );
            }
        }
        ensure_post_resolution_poll_cleared(&state)?;
        ensure_resolve_freshness_matches(&state, &params)?;
    }
    let expected_head = params
        .head_sha
        .as_deref()
        .context("resolve_addressed requires head_sha")?;
    let current_head = state
        .pr
        .head_sha
        .as_deref()
        .context("watch state has no current PR head_sha; poll first")?;
    if expected_head != current_head {
        bail!(
            "resolve_addressed head_sha is stale: expected {}, current {}",
            expected_head,
            current_head
        );
    }
    if !has_non_empty_commit_or_reason(&params) {
        bail!("resolve_addressed requires a non-empty commit_sha or a non-empty no-code reason");
    }
    if !has_explicit_no_code_reason(&params)
        && !commit_sha_matches_current_head(&params, current_head)
    {
        bail!(
            "resolve_addressed commit_sha must match the watched PR head_sha ({current_head}) unless this is an explicit no-code resolution"
        );
    }
    let grant = active_grant_for_scope(&state, WriteScope::ResolveThreads, &ctx.session_id)
        .cloned()
        .context("resolve_addressed requires an active resolve_threads grant for this session")?;

    let mut prevalidated = Vec::new();
    for id in &params.thread_ids {
        let Some(marker) = state.last_seen.review_threads.get(id) else {
            bail!("resolve_addressed unknown review thread id: {id}");
        };
        if marker.resolved {
            if review_thread_had_successful_resolution_attempt(&state, id) {
                continue;
            }
            bail!("resolve_addressed thread is already resolved in watch state: {id}");
        }
        if !state.pending_actionable.iter().any(|item| item.id == *id)
            && params.reason.as_deref().map(|reason| reason.contains(id)) != Some(true)
        {
            bail!(
                "resolve_addressed thread is not pending actionable and reason does not explicitly link it: {id}"
            );
        }
        prevalidated.push(id.clone());
    }

    let mut attempts = Vec::new();
    let mut failed = false;
    for id in prevalidated {
        if failed {
            attempts.push(skipped_resolution_attempt(&id, expected_head, &params));
            continue;
        }
        let attempted_at = now_iso();
        let outcome = if would_write {
            run_gh_resolve_review_thread(root, &id).await
        } else {
            Ok(ResolveReviewThreadOutcome::Resolved)
        };
        let (status, error) = match outcome {
            Ok(ResolveReviewThreadOutcome::Resolved) => (ResolutionAttemptStatus::Resolved, None),
            Ok(ResolveReviewThreadOutcome::AlreadyResolved) => {
                if review_thread_had_prior_resolution_attempt(&state, &id) {
                    (ResolutionAttemptStatus::AlreadyResolved, None)
                } else {
                    failed = true;
                    (
                        ResolutionAttemptStatus::Failed,
                        Some(
                            "GitHub reported thread already resolved while watch state still marks it unresolved; poll before retry"
                                .to_string(),
                        ),
                    )
                }
            }
            Ok(ResolveReviewThreadOutcome::NotResolved) => {
                failed = true;
                (
                    ResolutionAttemptStatus::Failed,
                    Some("GitHub response reported isResolved=false".to_string()),
                )
            }
            Ok(ResolveReviewThreadOutcome::MalformedResponse(message)) => {
                failed = true;
                (ResolutionAttemptStatus::Failed, Some(message))
            }
            Err(err) => {
                failed = true;
                (ResolutionAttemptStatus::Failed, Some(err.to_string()))
            }
        };
        attempts.push(ThreadResolutionAttempt {
            thread_id: id,
            attempted_at,
            status,
            head_sha: Some(expected_head.to_string()),
            commit_sha: params.commit_sha.clone(),
            validation: params.validation.clone(),
            reason: params.reason.clone().unwrap_or_else(|| {
                "resolved after validated fix for addressed review feedback".to_string()
            }),
            error,
        });
    }

    let resolved_count = attempts
        .iter()
        .filter(|attempt| {
            matches!(
                attempt.status,
                ResolutionAttemptStatus::Resolved | ResolutionAttemptStatus::AlreadyResolved
            )
        })
        .count();
    let remote_resolved_count = attempts
        .iter()
        .filter(|attempt| matches!(attempt.status, ResolutionAttemptStatus::Resolved))
        .count();
    let failed_count = attempts
        .iter()
        .filter(|attempt| matches!(attempt.status, ResolutionAttemptStatus::Failed))
        .count();
    state.last_resolution_attempts =
        merge_resolution_attempts(&state.last_resolution_attempts, attempts);
    state.last_resolution_error = (failed_count > 0)
        .then(|| "one or more thread resolutions failed; poll before retry".to_string());
    state.resolution_requires_post_poll = would_write && remote_resolved_count > 0;
    if grant.single_use && would_write && remote_resolved_count > 0 {
        let consumed = consume_single_use_grant(&mut state, &grant.grant_id);
        state.push_event(WatchEvent {
            at: now_iso(),
            kind: "grant_consumed".to_string(),
            data: json!({"grant_id": grant.grant_id, "consumed": consumed, "scope": "resolve_threads", "remote_resolved_count": remote_resolved_count}),
        });
    }
    state.updated_at = Some(now_iso());
    state.push_event(WatchEvent {
        at: now_iso(),
        kind: "resolve_addressed_completed".to_string(),
        data: json!({
            "resolved_count": resolved_count,
            "remote_resolved_count": remote_resolved_count,
            "failed_count": failed_count,
            "requires_post_poll": state.resolution_requires_post_poll,
        }),
    });
    if would_write {
        write_state_atomic(&state_path(store, &state.watch_id), &state)?;
    }
    let status = if failed_count > 0 {
        "partial_failure"
    } else {
        "resolved"
    };
    Ok(ToolOutput::new(format!(
        "PR watch resolve_addressed: {}\nRepo: {}\nPR: #{}\nStatus: {}\nResolved/already-resolved: {}\nFailed: {}\nPost-mutation poll required: {}{}",
        state.watch_id,
        state.pr.repo,
        state.pr.number,
        status,
        resolved_count,
        failed_count,
        state.resolution_requires_post_poll,
        if would_write { "" } else { "\nDry run: no GitHub mutation performed" }
    ))
    .with_title(format!("{} resolve_addressed {status}", state.watch_id))
    .with_metadata(json!({"watch": state, "written": would_write, "resolved_count": resolved_count, "failed_count": failed_count})))
}

fn reschedule_watch(
    store: &Path,
    mut params: PrWatchInput,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let mut state = load_state_for_params(store, &params)?;
    let would_write = !params.dry_run.unwrap_or(false);
    let _lock = if would_write {
        match acquire_watch_lock(store, &state.watch_id)? {
            Some(lock) => Some(lock),
            None => return Ok(watch_locked_output(store, &state, "reschedule")),
        }
    } else {
        None
    };
    apply_schedule_fields(&mut state, &params);
    state.updated_at = Some(now_iso());
    let canceled = if would_write {
        cancel_queued_watch_items(&state)?
    } else {
        0
    };
    state.push_event(WatchEvent {
        at: now_iso(),
        kind: "rescheduled".to_string(),
        data: json!({
            "canceled_existing_items": canceled,
            "lock_strategy": "watch lock file plus updated_at/cycle stale-write checks",
            "read_only_watch_invariant": true,
        }),
    });
    let path = state_path(store, &state.watch_id);
    params.schedule_next = true;
    let scheduled = maybe_schedule_next(ctx, &mut state, &params)?;
    if would_write {
        let root = state
            .root_dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        register_webhook_index_entry(&root, store, &state)?;
        write_state_atomic(&path, &state)?;
    }
    Ok(ToolOutput::new(format!(
        "PR watch rescheduled: {}\nCanceled queued items: {}\nScheduled: {}\nNote: scheduled monitor cycles remain read-only and must not push, comment, resolve threads, or merge.{}",
        state.watch_id,
        canceled,
        scheduled.unwrap_or_else(|| "not scheduled".to_string()),
        if would_write { "" } else { "\nDry run: no file written" }
    ))
    .with_title(format!("rescheduled {}", state.watch_id))
    .with_metadata(json!({"watch": state, "canceled": canceled, "written": would_write})))
}

fn apply_schedule_fields(state: &mut PrWatchState, params: &PrWatchInput) {
    if let Some(mode) = &params.event_mode {
        state.webhook.mode = mode.clone();
        state.webhook.enabled =
            matches!(mode, PrWatchEventMode::Webhook | PrWatchEventMode::Hybrid);
    }
    if let Some(seconds) = params.fallback_heartbeat_seconds {
        state.webhook.fallback_heartbeat_seconds = Some(seconds.max(300));
    } else if params.event_mode.is_some() {
        state.webhook.fallback_heartbeat_seconds = None;
    }
    if let Some(url) = params
        .webhook_url_hint
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        state.webhook.webhook_url_hint = Some(url.trim().to_string());
    }
    if let Some(seconds) = params.poll_interval_seconds {
        state.polling.poll_interval_seconds = seconds.max(60);
    }
    if let Some(required) = params.quiet_cycles_required {
        state.polling.required_quiet_cycles = required.max(1);
    }
    if params.schedule_next {
        if should_schedule_normal_monitor(state) {
            let wake_at =
                Utc::now() + Duration::seconds(state.polling.poll_interval_seconds as i64);
            state.polling.next_poll_at = Some(wake_at.format("%Y-%m-%dT%H:%M:%SZ").to_string());
        } else {
            state.polling.next_poll_at = None;
        }
    }
}

fn should_schedule_normal_monitor(state: &PrWatchState) -> bool {
    !matches!(state.webhook.mode, PrWatchEventMode::Webhook)
}

fn parse_write_scopes(values: Option<&[String]>) -> Result<BTreeSet<WriteScope>> {
    let mut scopes = BTreeSet::new();
    for value in values.unwrap_or(&[]) {
        let scope = match value.as_str() {
            "local_fix" => WriteScope::LocalFix,
            "commit" => WriteScope::Commit,
            "push" => WriteScope::Push,
            "comment" => WriteScope::Comment,
            "resolve_threads" => WriteScope::ResolveThreads,
            "merge" => bail!("merge is not an authorizable pr_watch scope"),
            other => bail!("unknown pr_watch authorization scope: {other}"),
        };
        scopes.insert(scope);
    }
    Ok(scopes)
}

fn format_scope(scope: WriteScope) -> &'static str {
    match scope {
        WriteScope::LocalFix => "local_fix",
        WriteScope::Commit => "commit",
        WriteScope::Push => "push",
        WriteScope::Comment => "comment",
        WriteScope::ResolveThreads => "resolve_threads",
    }
}

fn format_scopes(scopes: &BTreeSet<WriteScope>) -> String {
    if scopes.is_empty() {
        return "none".to_string();
    }
    scopes
        .iter()
        .copied()
        .map(format_scope)
        .collect::<Vec<_>>()
        .join(",")
}

fn schedule_overdue_by_seconds(state: &PrWatchState) -> Option<i64> {
    if state.terminal {
        return None;
    }
    let next = state.polling.next_poll_at.as_deref()?;
    let parsed = chrono::DateTime::parse_from_rfc3339(next).ok()?;
    let seconds = (Utc::now() - parsed.with_timezone(&Utc)).num_seconds();
    (seconds > 0).then_some(seconds)
}

fn schedule_status_line(state: &PrWatchState) -> String {
    match schedule_overdue_by_seconds(state) {
        Some(seconds) => format!(
            "Schedule: overdue by {}; recover with `pr_watch action=\"reschedule\" repo={} pr={} watch_id={} schedule_next=true`",
            human_duration(seconds),
            state.pr.repo,
            state.pr.number,
            state.watch_id
        ),
        None => format!(
            "Schedule: next poll {}",
            state
                .polling
                .next_poll_at
                .as_deref()
                .unwrap_or("not scheduled")
        ),
    }
}

fn schedule_queue_health_line(state: &PrWatchState) -> String {
    if state.terminal {
        return "Schedule health: terminal".to_string();
    }
    if !state.pending_actionable.is_empty() {
        return format!(
            "Schedule health: paused_action_required; run `pr_watch action=\"reschedule\" repo={} pr={} watch_id={} schedule_next=true` after remediation if continued monitoring is desired",
            state.pr.repo, state.pr.number, state.watch_id
        );
    }
    if state.last_cycle.failed_check_count > 0 {
        return format!(
            "Schedule health: paused_checks_failed; inspect checks, then run `pr_watch action=\"reschedule\" repo={} pr={} watch_id={} schedule_next=true` to continue monitoring",
            state.pr.repo, state.pr.number, state.watch_id
        );
    }

    let Ok(manager) = AmbientManager::new() else {
        return "Schedule health: unknown; failed to load ambient queue".to_string();
    };
    let matches = find_existing_scheduled_watch_items(manager.queue().items(), state);
    if matches.is_empty() {
        if state.polling.next_poll_at.is_some() || state.polling.last_schedule_due_at.is_some() {
            return format!(
                "Schedule health: missing_queue_item; recover with `pr_watch action=\"reschedule\" repo={} pr={} watch_id={} schedule_next=true`",
                state.pr.repo, state.pr.number, state.watch_id
            );
        }
        return "Schedule health: unscheduled".to_string();
    }
    let now = Utc::now();
    let future = matches
        .iter()
        .filter(|item| item.scheduled_for > now)
        .count();
    let overdue = matches.len().saturating_sub(future);
    if matches.len() > 1 {
        return format!(
            "Schedule health: duplicate_future_items count={} future={} overdue={}; recover with `pr_watch action=\"reschedule\" repo={} pr={} watch_id={} schedule_next=true`",
            matches.len(),
            future,
            overdue,
            state.pr.repo,
            state.pr.number,
            state.watch_id
        );
    }
    let item = matches[0];
    if item.scheduled_for <= now {
        return format!(
            "Schedule health: overdue_unclaimed due_at={}; recover with `pr_watch action=\"reschedule\" repo={} pr={} watch_id={} schedule_next=true`",
            item.scheduled_for.format("%Y-%m-%dT%H:%M:%SZ"),
            state.pr.repo,
            state.pr.number,
            state.watch_id
        );
    }
    format!(
        "Schedule health: healthy; canonical monitor {} due_at={}",
        item.id,
        item.scheduled_for.format("%Y-%m-%dT%H:%M:%SZ")
    )
}

fn human_duration(seconds: i64) -> String {
    if seconds >= 3600 {
        format!("{}h{}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m{}s", seconds / 60, seconds % 60)
    } else {
        format!("{}s", seconds)
    }
}

fn active_grant_lines(state: &PrWatchState) -> Vec<String> {
    let now = now_iso();
    state
        .authorization
        .active_grants
        .iter()
        .map(|grant| {
            let active = now <= grant.expires_at;
            format!(
                "{} scopes={} expires={} status={} reason={}",
                grant.grant_id,
                format_scopes(&grant.scopes),
                grant.expires_at,
                if active { "active" } else { "expired" },
                grant.reason.as_deref().unwrap_or("none")
            )
        })
        .collect()
}

fn recent_grant_event_lines(state: &PrWatchState) -> Vec<String> {
    state
        .events
        .iter()
        .rev()
        .filter(|event| matches!(event.kind.as_str(), "grant_created" | "grant_revoked"))
        .take(5)
        .map(|event| format!("{} {} {}", event.at, event.kind, event.data))
        .collect()
}

fn monitor_max_runtime_seconds(params: &PrWatchInput) -> u64 {
    params
        .max_runtime_seconds
        .unwrap_or(DEFAULT_MONITOR_MAX_RUNTIME_SECONDS)
        .clamp(1, MAX_MONITOR_MAX_RUNTIME_SECONDS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MonitorStatus {
    QuietSatisfied,
    PendingNextPoll,
    ActionRequired,
    ChecksPending,
    ChecksFailed,
    TransientFailure,
    AlreadyRunning,
    Stopped,
}

impl MonitorStatus {
    fn as_str(self) -> &'static str {
        match self {
            MonitorStatus::QuietSatisfied => "quiet_satisfied",
            MonitorStatus::PendingNextPoll => "pending_next_poll",
            MonitorStatus::ActionRequired => "action_required",
            MonitorStatus::ChecksPending => "checks_pending",
            MonitorStatus::ChecksFailed => "checks_failed",
            MonitorStatus::TransientFailure => "transient_failure",
            MonitorStatus::AlreadyRunning => "already_running",
            MonitorStatus::Stopped => "stopped",
        }
    }
}

fn monitor_status_for_state(state: &PrWatchState, partial_failure: bool) -> MonitorStatus {
    if state.terminal {
        return if state.stop_reason.as_deref() == Some("quiet_cycles_satisfied") {
            MonitorStatus::QuietSatisfied
        } else {
            MonitorStatus::Stopped
        };
    }
    if !state.pending_actionable.is_empty() {
        MonitorStatus::ActionRequired
    } else if state.last_cycle.failed_check_count > 0 {
        MonitorStatus::ChecksFailed
    } else if state.last_cycle.pending_check_count > 0 {
        MonitorStatus::ChecksPending
    } else if partial_failure || state.polling.consecutive_transient_failures > 0 {
        MonitorStatus::TransientFailure
    } else if state.polling.quiet_cycles >= state.polling.required_quiet_cycles {
        MonitorStatus::QuietSatisfied
    } else {
        MonitorStatus::PendingNextPoll
    }
}

fn monitor_should_schedule_followup(status: MonitorStatus) -> bool {
    matches!(
        status,
        MonitorStatus::PendingNextPoll
            | MonitorStatus::ChecksPending
            | MonitorStatus::TransientFailure
    )
}

fn watch_state_changed_since_load(
    current_state: &PrWatchState,
    loaded_existing_state: bool,
    loaded_updated_at: &Option<String>,
    loaded_cycle_number: u64,
) -> bool {
    !loaded_existing_state
        || current_state.updated_at != *loaded_updated_at
        || current_state.polling.cycle_number != loaded_cycle_number
}

fn timed_out_collection(max_runtime_seconds: u64) -> GhCollection {
    let message = format!("monitor collection exceeded max_runtime_seconds={max_runtime_seconds}");
    GhCollection {
        metadata: Err(SurfaceError::transient("metadata", message.clone())),
        checks: Err(SurfaceError::transient("checks", message.clone())),
        review_comments: Err(SurfaceError::transient("review_comments", message.clone())),
        issue_comments: Err(SurfaceError::transient("issue_comments", message.clone())),
        reviews: Err(SurfaceError::transient("reviews", message.clone())),
        review_threads: Err(SurfaceError::transient("review_threads", message)),
    }
}

fn maybe_schedule_next(
    ctx: &ToolContext,
    state: &mut PrWatchState,
    params: &PrWatchInput,
) -> Result<Option<String>> {
    if matches!(state.webhook.mode, PrWatchEventMode::Webhook) {
        maybe_schedule_webhook_heartbeat(ctx, state, params)
    } else {
        maybe_schedule_next_monitor(ctx, state, params)
    }
}

fn maybe_schedule_webhook_heartbeat(
    ctx: &ToolContext,
    state: &mut PrWatchState,
    params: &PrWatchInput,
) -> Result<Option<String>> {
    if params.dry_run.unwrap_or(false) {
        return Ok(None);
    }
    if state.terminal {
        let _ = cancel_queued_watch_items(state);
        state.polling.next_poll_at = None;
        state.polling.last_schedule_due_at = None;
        state.polling.last_schedule_id = None;
        state.polling.last_schedule_kind = None;
        state.polling.last_schedule_target = None;
        return Ok(None);
    }
    if !params.schedule_next {
        return Ok(None);
    }
    let Some(seconds) = state.webhook.fallback_heartbeat_seconds else {
        state.polling.next_poll_at = None;
        state.polling.last_schedule_due_at = None;
        state.polling.last_schedule_id = None;
        state.polling.last_schedule_kind = None;
        state.polling.last_schedule_target = None;
        return Ok(Some(
            "webhook mode: normal monitor polling disabled; heartbeat disabled".to_string(),
        ));
    };
    let wake_at = Utc::now() + Duration::seconds(seconds as i64);
    let task = scheduled_webhook_heartbeat_prompt(state, monitor_max_runtime_seconds(params));
    let mut manager = AmbientManager::new()?;
    let key = webhook_heartbeat_schedule_key_for_watch(&state.watch_id);
    let existing_ids: Vec<String> = manager
        .queue()
        .items()
        .iter()
        .filter(|item| item.schedule_key.as_deref() == Some(&key))
        .map(|item| item.id.clone())
        .collect();
    for stale_id in existing_ids {
        manager.cancel_schedule(&stale_id)?;
    }
    let daemon_originated = ctx.session_id == "pr-watch-webhook-daemon";
    let target = match params.target.as_deref() {
        Some("spawn") => ScheduleTarget::Spawn {
            parent_session_id: ctx.session_id.clone(),
        },
        None if daemon_originated => ScheduleTarget::Spawn {
            parent_session_id: ctx.session_id.clone(),
        },
        Some("resume") | None => ScheduleTarget::Session {
            session_id: ctx.session_id.clone(),
        },
        Some(other) => bail!("invalid schedule target {other}; expected resume or spawn"),
    };
    let target_summary = format_schedule_target_for_state(&target);
    let payload = PrWatchWebhookHeartbeatPayload::new(state, seconds);
    payload.validate_against_state(state)?;
    let id = manager.schedule(ScheduleRequest {
        wake_in_minutes: None,
        wake_at: Some(wake_at),
        context: task.clone(),
        priority: Priority::Normal,
        target,
        created_by_session: ctx.session_id.clone(),
        working_dir: ctx
            .working_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        task_description: Some(task),
        relevant_files: vec![state_file_for_watch(&state.watch_id)],
        git_branch: None,
        additional_context: Some(
            "Scheduled by pr_watch webhook heartbeat; invoke webhook_heartbeat only. Read-only refresh only.".to_string(),
        ),
        schedule_key: Some(key),
        schedule_kind: Some("pr_watch.webhook_heartbeat".to_string()),
        schedule_payload: Some(serde_json::to_value(payload)?),
    })?;
    let due_at = wake_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    state.polling.next_poll_at = Some(due_at.clone());
    state.polling.last_scheduled_at = Some(now_iso());
    state.polling.last_schedule_id = Some(id.clone());
    state.polling.last_schedule_kind = Some("webhook_heartbeat".to_string());
    state.polling.last_schedule_target = Some(target_summary);
    state.polling.last_schedule_due_at = Some(due_at);
    state.polling.last_schedule_error = None;
    super::ambient::nudge_schedule_runner();
    Ok(Some(format!(
        "webhook heartbeat {id} at {}",
        wake_at.format("%Y-%m-%dT%H:%M:%SZ")
    )))
}

fn normalize_handoff_summary(summary: &str) -> String {
    summary
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

fn sha256_hex(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    format!("{:x}", hasher.finalize())
}

const WEBHOOK_MAX_BODY_BYTES: usize = 1024 * 1024;
const WEBHOOK_DEBOUNCE_SECONDS: i64 = 10;
const WEBHOOK_CONNECTION_TIMEOUT_SECONDS: u64 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedGithubDelivery {
    delivery_id: String,
    event: String,
    action: Option<String>,
    repo: Option<String>,
    pr: Option<u64>,
    payload: Value,
}

fn content_type_is_accepted(value: &str) -> bool {
    let mut parts = value.split(';');
    let Some(media_type) = parts.next().map(str::trim) else {
        return false;
    };
    if !media_type.eq_ignore_ascii_case("application/json") {
        return false;
    }
    parts.all(|part| {
        let trimmed = part.trim();
        trimmed.is_empty() || trimmed.to_ascii_lowercase().starts_with("charset=")
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn hmac_sha256(secret: &[u8], body: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut key = [0u8; BLOCK_SIZE];
    if secret.len() > BLOCK_SIZE {
        let mut hasher = Sha256::new();
        hasher.update(secret);
        let digest = hasher.finalize();
        key[..32].copy_from_slice(&digest);
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }
    let mut o_key_pad = [0x5cu8; BLOCK_SIZE];
    let mut i_key_pad = [0x36u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        o_key_pad[i] ^= key[i];
        i_key_pad[i] ^= key[i];
    }
    let mut inner = Sha256::new();
    inner.update(i_key_pad);
    inner.update(body);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(o_key_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn verify_github_signature(secret: &str, body: &[u8], signature: &str) -> Result<()> {
    let secret = secret.as_bytes();
    if secret.is_empty() {
        bail!("GITHUB_WEBHOOK_SECRET must be non-empty");
    }
    let hex_signature = signature
        .strip_prefix("sha256=")
        .context("missing sha256= signature prefix")?;
    let expected = hmac_sha256(secret, body);
    let actual = hex::decode(hex_signature).context("invalid webhook signature hex")?;
    if !constant_time_eq(&expected, &actual) {
        bail!("webhook signature mismatch");
    }
    Ok(())
}

fn verified_github_delivery_from_parts(
    secret: &str,
    content_type: &str,
    event: &str,
    delivery_id: &str,
    signature: &str,
    body: &[u8],
) -> Result<VerifiedGithubDelivery> {
    if body.len() > WEBHOOK_MAX_BODY_BYTES {
        bail!("webhook body exceeds {} bytes", WEBHOOK_MAX_BODY_BYTES);
    }
    if !content_type_is_accepted(content_type) {
        bail!("unsupported webhook content type: {content_type}");
    }
    if event.trim().is_empty() || delivery_id.trim().is_empty() {
        bail!("webhook event and delivery id are required");
    }
    verify_github_signature(secret, body, signature)?;
    let payload: Value = serde_json::from_slice(body).context("invalid webhook json body")?;
    Ok(VerifiedGithubDelivery {
        delivery_id: delivery_id.to_string(),
        event: event.to_string(),
        action: payload
            .get("action")
            .and_then(Value::as_str)
            .map(str::to_string),
        repo: payload
            .pointer("/repository/full_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        pr: payload
            .pointer("/pull_request/number")
            .or_else(|| payload.pointer("/issue/number"))
            .or_else(|| payload.pointer("/check_run/pull_requests/0/number"))
            .and_then(Value::as_u64),
        payload,
    })
}

pub fn run_webhook_status_command(json_output: bool) -> Result<()> {
    let index = load_webhook_index()?;
    let health = read_webhook_health()?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "index_path": webhook_index_path()?,
                "health_path": webhook_health_path()?,
                "health": health,
                "entries": index.entries,
            }))?
        );
        return Ok(());
    }
    println!("PR watch webhook status");
    println!("Index: {}", webhook_index_path()?.display());
    println!("Health: {}", webhook_health_path()?.display());
    match health {
        Some(health) => println!(
            "Daemon: {} pid={} alive={} bind={}:{} updated={} last_delivery={} last_result={}",
            health.status,
            health.pid,
            process_is_alive(health.pid),
            health.bind,
            health.port,
            health.updated_at,
            health.last_delivery_id.as_deref().unwrap_or("none"),
            health.last_result.as_deref().unwrap_or("none")
        ),
        None => println!("Daemon: down (no health file)"),
    }
    println!("Indexed watches: {}", index.entries.len());
    for entry in index.entries {
        println!(
            "- {} {}#{} mode={:?} active={} root={} state={}",
            entry.watch_id,
            entry.repo,
            entry.pr,
            entry.event_mode,
            entry.active,
            entry.root_dir,
            entry.state_path
        );
    }
    Ok(())
}

pub async fn run_webhook_doctor_command(repo: Option<&str>, json_output: bool) -> Result<()> {
    let index = load_webhook_index()?;
    let health = read_webhook_health()?;
    let secret_present = std::env::var("GITHUB_WEBHOOK_SECRET")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let mut findings = Vec::new();
    findings.push(json!({
        "check": "daemon_health",
        "status": if health.as_ref().map(|h| h.status.as_str()) == Some("running") { "ok" } else { "warn" },
        "message": health.as_ref().map(|h| h.message.clone().unwrap_or_else(|| h.status.clone())).unwrap_or_else(|| "daemon_down: no health file".to_string()),
    }));
    findings.push(json!({
        "check": "secret",
        "status": if secret_present { "ok" } else { "error" },
        "message": if secret_present { "GITHUB_WEBHOOK_SECRET is configured" } else { "GITHUB_WEBHOOK_SECRET is missing or empty" },
    }));
    for entry in index
        .entries
        .iter()
        .filter(|entry| repo.is_none_or(|repo| repo == entry.repo))
    {
        let root_ok = Path::new(&entry.root_dir).is_dir();
        let state_ok = Path::new(&entry.state_path).is_file();
        let state_matches = if state_ok {
            fs::read_to_string(&entry.state_path)
                .ok()
                .and_then(|text| normalize_watch_state_json(&text).ok())
                .map(|state| {
                    state.root_dir.as_deref() == Some(entry.root_dir.as_str())
                        && state.pr.repo == entry.repo
                        && state.pr.number == entry.pr
                        && state.watch_id == entry.watch_id
                })
                .unwrap_or(false)
        } else {
            false
        };
        findings.push(json!({
            "check": "watch_index_entry",
            "watch_id": entry.watch_id,
            "repo": entry.repo,
            "pr": entry.pr,
            "status": if root_ok && state_ok && state_matches && entry.active { "ok" } else { "error" },
            "root_ok": root_ok,
            "state_ok": state_ok,
            "state_matches_index": state_matches,
            "active": entry.active,
            "message": if root_ok && state_ok && state_matches && entry.active { "indexed watch is routable" } else { "indexed watch is not routable; daemon would quarantine matching deliveries" },
        }));
    }

    let daemon_alive = health
        .as_ref()
        .map(|h| process_is_alive(h.pid))
        .unwrap_or(false);
    findings.push(json!({
        "check": "daemon_pid_alive",
        "status": if daemon_alive { "ok" } else { "warn" },
        "message": if daemon_alive { "daemon PID is alive" } else { "daemon_down: no live PID from health file" },
    }));

    let gh_status = Command::new("gh").arg("auth").arg("status").output().await;
    findings.push(json!({
        "check": "gh_auth_status",
        "status": if gh_status.as_ref().map(|out| out.status.success()).unwrap_or(false) { "ok" } else { "warn" },
        "message": if gh_status.as_ref().map(|out| out.status.success()).unwrap_or(false) { "gh auth status succeeded" } else { "gh auth status failed; GitHub hook inspection may be unavailable" },
    }));

    let repos_to_check: BTreeSet<String> = if let Some(repo) = repo {
        [repo.to_string()].into_iter().collect()
    } else {
        index
            .entries
            .iter()
            .map(|entry| entry.repo.clone())
            .collect()
    };
    for repo in repos_to_check {
        let hook_status = Command::new("gh")
            .args(["api", &format!("repos/{repo}/hooks")])
            .output()
            .await;
        let ok = hook_status
            .as_ref()
            .map(|out| out.status.success())
            .unwrap_or(false);
        let hook_message = if ok {
            let hooks: Value = serde_json::from_slice(&hook_status.as_ref().unwrap().stdout)
                .unwrap_or_else(|_| json!([]));
            let failing = hooks.as_array().into_iter().flatten().find_map(|hook| {
                let code = hook
                    .pointer("/last_response/code")
                    .and_then(Value::as_i64)?;
                (!(200..300).contains(&code)).then_some(code)
            });
            let required_events = [
                "pull_request",
                "pull_request_review",
                "pull_request_review_comment",
                "issue_comment",
                "check_run",
                "check_suite",
                "status",
            ];
            let missing_events = hooks.as_array().into_iter().flatten().all(|hook| {
                let Some(events) = hook.get("events").and_then(Value::as_array) else {
                    return true;
                };
                !required_events
                    .iter()
                    .all(|required| events.iter().any(|event| event.as_str() == Some(*required)))
            });
            match failing {
                Some(code) => format!("github_hook_failing: hook last_response.code={code}"),
                None if missing_events => {
                    "github_hook_failing: no hook advertises all required PR watch events"
                        .to_string()
                }
                None => "GitHub hooks API reachable; no non-2xx last_response found".to_string(),
            }
        } else {
            "github_hook_failing_or_auth_failing: could not read hooks with gh api".to_string()
        };
        findings.push(json!({
            "check": "github_hooks",
            "repo": repo,
            "status": if hook_message.contains("github_hook_failing") || !ok { "warn" } else { "ok" },
            "message": hook_message,
        }));
    }

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "findings": findings }))?
        );
    } else {
        println!("PR watch webhook doctor");
        for finding in findings {
            println!(
                "- [{}] {}: {}",
                finding
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                finding
                    .get("check")
                    .and_then(Value::as_str)
                    .unwrap_or("check"),
                finding.get("message").and_then(Value::as_str).unwrap_or("")
            );
        }
    }
    Ok(())
}

pub async fn run_webhook_serve_command(
    bind: String,
    port: u16,
    secret_env: String,
    allow_non_local: bool,
) -> Result<()> {
    let local_bind = bind == "127.0.0.1" || bind == "localhost" || bind == "::1";
    if !local_bind && !allow_non_local {
        bail!("refusing non-local webhook bind {bind}; pass --allow-non-local explicitly");
    }
    let secret = std::env::var(&secret_env)
        .with_context(|| format!("{secret_env} must be set for webhook signature verification"))?;
    if secret.trim().is_empty() {
        bail!("{secret_env} must be non-empty");
    }
    fs::create_dir_all(webhook_runtime_dir()?)?;
    if let Some(pid) = read_webhook_pid()? {
        if process_is_alive(pid) {
            bail!("webhook daemon already appears alive with pid {pid}");
        }
        let _ = fs::remove_file(webhook_pid_path()?);
        let _ = fs::remove_file(webhook_lock_path()?);
    }
    let lock_path = webhook_lock_path()?;
    let _daemon_lock = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(mut file) => {
            writeln!(file, "pid={} at={}", std::process::id(), now_iso())?;
            Some(WatchLock { path: lock_path })
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "webhook daemon lock already exists: {}",
                lock_path.display()
            )
        }
        Err(err) => return Err(err).context("failed to create webhook daemon lock"),
    };
    let listener = match TcpListener::bind((bind.as_str(), port)).await {
        Ok(listener) => listener,
        Err(err) => {
            let _ = write_webhook_health(&WebhookDaemonHealth {
                status: "port_collision".to_string(),
                pid: std::process::id(),
                bind: bind.clone(),
                port,
                updated_at: now_iso(),
                last_delivery_id: None,
                last_event: None,
                last_result: None,
                message: Some(format!(
                    "failed to bind webhook daemon on {bind}:{port}: {err}"
                )),
            });
            return Err(err)
                .with_context(|| format!("failed to bind webhook daemon on {bind}:{port}"));
        }
    };
    fs::write(webhook_pid_path()?, std::process::id().to_string())?;
    let mut health = WebhookDaemonHealth {
        status: "running".to_string(),
        pid: std::process::id(),
        bind: bind.clone(),
        port,
        updated_at: now_iso(),
        last_delivery_id: None,
        last_event: None,
        last_result: None,
        message: Some("daemon running".to_string()),
    };
    write_webhook_health(&health)?;
    println!("PR watch webhook daemon listening on http://{bind}:{port}/github");
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                health.status = "stopped".to_string();
                health.updated_at = now_iso();
                health.message = Some("stopped by signal".to_string());
                let _ = write_webhook_health(&health);
                break;
            }
            _ = async { #[cfg(unix)] { terminate.recv().await } #[cfg(not(unix))] { std::future::pending::<Option<()>>().await } } => {
                health.status = "stopped".to_string();
                health.updated_at = now_iso();
                health.message = Some("stopped by SIGTERM".to_string());
                let _ = write_webhook_health(&health);
                break;
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                match tokio::time::timeout(
                    StdDuration::from_secs(WEBHOOK_CONNECTION_TIMEOUT_SECONDS),
                    handle_webhook_connection(stream, &secret),
                ).await {
                    Err(_) => {
                        health.last_result = Some(format!(
                            "rejected: webhook connection timed out after {WEBHOOK_CONNECTION_TIMEOUT_SECONDS}s"
                        ));
                    }
                    Ok(result) => match result {
                    Ok((delivery, result)) => {
                        health.last_delivery_id = Some(delivery.delivery_id);
                        health.last_event = Some(delivery.event);
                        health.last_result = Some(result);
                    }
                    Err(err) => {
                        health.last_result = Some(format!("rejected: {}", err));
                    }
                    },
                }
                health.updated_at = now_iso();
                let _ = write_webhook_health(&health);
            }
        }
    }
    Ok(())
}

async fn handle_webhook_connection(
    mut stream: TcpStream,
    secret: &str,
) -> Result<(VerifiedGithubDelivery, String)> {
    let request = read_webhook_http_request(&mut stream).await?;
    let (status, body, delivery_result) = match process_webhook_http_request(&request, secret).await
    {
        Ok((delivery, result)) => ("200 OK", format!("{result}\n"), Ok((delivery, result))),
        Err(err) => ("400 Bad Request", format!("{}\n", err), Err(err)),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    delivery_result
}

async fn read_webhook_http_request(stream: &mut TcpStream) -> Result<Vec<u8>> {
    const WEBHOOK_MAX_HEADER_BYTES: usize = 8192;
    let mut request = Vec::with_capacity(WEBHOOK_MAX_HEADER_BYTES);
    let mut header_end = None;

    while header_end.is_none() {
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        header_end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4);
        if header_end.is_none() && request.len() > WEBHOOK_MAX_HEADER_BYTES {
            bail!("webhook headers exceed {WEBHOOK_MAX_HEADER_BYTES} bytes");
        }
    }

    let header_end = header_end.context("malformed HTTP request")?;
    if header_end > WEBHOOK_MAX_HEADER_BYTES {
        bail!("webhook headers exceed {WEBHOOK_MAX_HEADER_BYTES} bytes");
    }
    let headers_raw =
        std::str::from_utf8(&request[..header_end - 4]).context("headers are not utf-8")?;
    let content_length = headers_raw
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(name, value)| {
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .context("missing content-length")?;
    if content_length > WEBHOOK_MAX_BODY_BYTES {
        bail!("webhook body exceeds {} bytes", WEBHOOK_MAX_BODY_BYTES);
    }

    let expected_len = header_end
        .checked_add(content_length)
        .context("webhook request length overflow")?;
    while request.len() < expected_len {
        let remaining = expected_len - request.len();
        let mut chunk = vec![0u8; remaining.min(8192)];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            bail!("incomplete webhook body read");
        }
        request.extend_from_slice(&chunk[..read]);
    }
    request.truncate(expected_len);
    Ok(request)
}

async fn process_webhook_http_request(
    request: &[u8],
    secret: &str,
) -> Result<(VerifiedGithubDelivery, String)> {
    let split = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("malformed HTTP request")?;
    let headers_raw = std::str::from_utf8(&request[..split]).context("headers are not utf-8")?;
    let body = &request[split + 4..];
    let mut lines = headers_raw.lines();
    let request_line = lines.next().context("missing request line")?;
    if !request_line.starts_with("POST ") {
        bail!("only POST is supported");
    }
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .context("missing content-length")?;
    if content_length > WEBHOOK_MAX_BODY_BYTES || content_length > body.len() {
        bail!("invalid webhook body length");
    }
    let body = &body[..content_length];
    let delivery = match verified_github_delivery_from_parts(
        secret,
        headers
            .get("content-type")
            .map(String::as_str)
            .unwrap_or(""),
        headers
            .get("x-github-event")
            .map(String::as_str)
            .unwrap_or(""),
        headers
            .get("x-github-delivery")
            .map(String::as_str)
            .unwrap_or(""),
        headers
            .get("x-hub-signature-256")
            .map(String::as_str)
            .unwrap_or(""),
        body,
    ) {
        Ok(delivery) => delivery,
        Err(err) => {
            let _ = append_rejected_webhook_delivery_log(
                headers.get("x-github-delivery").map(String::as_str),
                headers.get("x-github-event").map(String::as_str),
                "rejected",
                &err.to_string(),
            );
            return Err(err);
        }
    };
    if delivery_already_seen(&delivery)? {
        return Ok((delivery, "duplicate_ignored".to_string()));
    }
    let result = route_verified_webhook_delivery(&delivery).await?;
    remember_webhook_delivery(&delivery)?;
    Ok((delivery, result))
}

async fn route_verified_webhook_delivery(delivery: &VerifiedGithubDelivery) -> Result<String> {
    if delivery.event == "ping" {
        let _ = append_webhook_delivery_log(delivery, "accepted", "ping");
        return Ok("ping_accepted".to_string());
    }
    let index = load_webhook_index()?;
    let targets = webhook_delivery_targets(delivery, &index).await?;
    if targets.is_empty() {
        let reason = ignored_webhook_reason(delivery);
        let _ = append_webhook_delivery_log(delivery, "ignored", &reason);
        return Ok(reason);
    }
    let mut results = Vec::new();
    for entry in targets {
        let coalesced = schedule_webhook_followup_refresh(&entry, delivery)?;
        record_webhook_delivery_on_state(&entry, delivery, coalesced)?;
        let result = if coalesced { "coalesced" } else { "queued" };
        let _ = append_webhook_delivery_log(delivery, result, "debounced_followup_refresh");
        results.push(result);
    }
    Ok(results.join("; "))
}

fn ignored_webhook_reason(delivery: &VerifiedGithubDelivery) -> String {
    match delivery.event.as_str() {
        "check_suite"
            if delivery
                .payload
                .pointer("/check_suite/pull_requests")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty) =>
        {
            "check_suite_without_pr".to_string()
        }
        "pull_request"
        | "pull_request_review"
        | "pull_request_review_comment"
        | "issue_comment"
        | "check_run"
        | "check_suite"
        | "status" => "no_indexed_watch_target".to_string(),
        _ => "unknown_event".to_string(),
    }
}

async fn webhook_delivery_targets(
    delivery: &VerifiedGithubDelivery,
    index: &WebhookWatchIndex,
) -> Result<Vec<WebhookWatchIndexEntry>> {
    let repo = delivery
        .repo
        .as_deref()
        .context("delivery has no repository")?;
    let active_repo_entries = active_webhook_entries_for_repo(index, repo);
    if active_repo_entries.is_empty() {
        return Ok(Vec::new());
    }
    let mut prs = BTreeSet::new();
    match delivery.event.as_str() {
        "pull_request" | "pull_request_review" | "pull_request_review_comment" => {
            if let Some(pr) = delivery
                .payload
                .pointer("/pull_request/number")
                .and_then(Value::as_u64)
            {
                prs.insert(pr);
            }
        }
        "issue_comment" => {
            if delivery.payload.pointer("/issue/pull_request").is_some() {
                if let Some(pr) = delivery
                    .payload
                    .pointer("/issue/number")
                    .and_then(Value::as_u64)
                {
                    prs.insert(pr);
                }
            }
        }
        "check_run" => {
            if let Some(values) = delivery
                .payload
                .pointer("/check_run/pull_requests")
                .and_then(Value::as_array)
            {
                for value in values {
                    if let Some(pr) = value.get("number").and_then(Value::as_u64) {
                        prs.insert(pr);
                    }
                }
            }
        }
        "check_suite" => {
            if let Some(values) = delivery
                .payload
                .pointer("/check_suite/pull_requests")
                .and_then(Value::as_array)
            {
                for value in values {
                    if let Some(pr) = value.get("number").and_then(Value::as_u64) {
                        prs.insert(pr);
                    }
                }
            }
        }
        "status" => {
            if let Some(sha) = delivery.payload.get("sha").and_then(Value::as_str) {
                for pr in resolve_status_sha_prs(repo, sha).await? {
                    prs.insert(pr);
                }
            }
        }
        _ => return Ok(Vec::new()),
    }
    Ok(active_repo_entries
        .into_iter()
        .filter(|entry| prs.contains(&entry.pr))
        .cloned()
        .collect())
}

async fn resolve_status_sha_prs(repo: &str, sha: &str) -> Result<Vec<u64>> {
    let output = Command::new("gh")
        .args([
            "api",
            &format!("repos/{repo}/commits/{sha}/pulls"),
            "-H",
            "Accept: application/vnd.github+json",
        ])
        .output()
        .await
        .context("failed to run gh api for status sha PR lookup")?;
    if !output.status.success() {
        bail!("gh api status sha PR lookup failed");
    }
    let value: Value =
        serde_json::from_slice(&output.stdout).context("invalid gh status PR lookup json")?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|pr| pr.get("number").and_then(Value::as_u64))
        .collect())
}

#[allow(dead_code)]
async fn webhook_refresh_watch(
    entry: &WebhookWatchIndexEntry,
    delivery: &VerifiedGithubDelivery,
) -> Result<String> {
    let root = PathBuf::from(&entry.root_dir);
    let store = watch_dir(&root);
    let _lock = match acquire_watch_lock(&store, &entry.watch_id)? {
        Some(lock) => lock,
        None => {
            let _ = schedule_webhook_followup_refresh(entry, delivery)?;
            return Ok(format!("locked_followup_requested {}", entry.watch_id));
        }
    };
    let mut state = load_state_for_params(&store, &webhook_refresh_params(entry, true))?;
    if state.root_dir.as_deref() != Some(entry.root_dir.as_str())
        || state.pr.repo != entry.repo
        || state.pr.number != entry.pr
        || state.watch_id != entry.watch_id
        || state.terminal
    {
        bail!(
            "indexed watch {} failed webhook refresh validation; refusing write",
            entry.watch_id
        );
    }
    let ctx = ToolContext {
        session_id: "pr-watch-webhook-daemon".to_string(),
        message_id: delivery.delivery_id.clone(),
        tool_call_id: format!("webhook-{}", delivery.delivery_id),
        working_dir: Some(root.clone()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    };
    let collected_at = now_iso();
    let result = collect_with_gh(&root, &state.pr.repo, state.pr.number).await;
    let outcome = update_state_from_collection(&mut state, result, &collected_at);
    if review_threads_fetch_succeeded_at(&state, &collected_at) {
        state.resolution_requires_post_poll = false;
    }
    apply_schedule_fields(
        &mut state,
        &webhook_refresh_params(entry, !matches!(entry.event_mode, PrWatchEventMode::Hybrid)),
    );
    let handoff = if !state.pending_actionable.is_empty() {
        maybe_schedule_action_required_handoff(&store, &mut state, &ctx)?
    } else {
        clear_action_required_handoff(&store, &mut state)?
    };
    if state.polling.quiet_cycles >= state.polling.required_quiet_cycles
        && state.pending_actionable.is_empty()
        && state.last_cycle.pending_check_count == 0
        && state.last_cycle.failed_check_count == 0
        && !outcome.partial_failure
    {
        state.terminal = true;
        state.stop_reason = Some("quiet_cycles_satisfied".to_string());
        state.polling.next_poll_at = None;
    }
    let scheduled = maybe_schedule_next(
        &ctx,
        &mut state,
        &webhook_refresh_params(entry, !matches!(entry.event_mode, PrWatchEventMode::Hybrid)),
    )?;
    state.webhook.last_delivery_id = Some(delivery.delivery_id.clone());
    state.webhook.last_delivery_at = Some(now_iso());
    state.webhook.last_event_type = Some(delivery.event.clone());
    state.webhook.last_event_action = delivery.action.clone();
    state.webhook.last_delivery_status = Some(if outcome.partial_failure {
        "refresh_partial_failure".to_string()
    } else {
        "routed".to_string()
    });
    if state.terminal {
        remove_webhook_index_entry(&state.watch_id)?;
    }
    write_state_atomic(&state_path(&store, &entry.watch_id), &state)?;
    Ok(format!(
        "routed {} partial_failure={} scheduled={} handoff={}",
        entry.watch_id,
        outcome.partial_failure,
        scheduled.unwrap_or_else(|| "none".to_string()),
        handoff.unwrap_or_else(|| "none".to_string())
    ))
}

fn schedule_webhook_followup_refresh(
    entry: &WebhookWatchIndexEntry,
    delivery: &VerifiedGithubDelivery,
) -> Result<bool> {
    let mut manager = AmbientManager::new()?;
    let key = webhook_followup_schedule_key_for_watch(&entry.watch_id);
    let existing = manager
        .queue()
        .items()
        .iter()
        .any(|item| item.schedule_key.as_deref() == Some(&key));
    if existing {
        return Ok(true);
    }
    let state_file = state_file_for_watch(&entry.watch_id);
    let payload = json!({
        "tool": "pr_watch",
        "watch_id": entry.watch_id,
        "repo": entry.repo,
        "pr": entry.pr,
        "action": "webhook_heartbeat",
        "state_file": state_file,
        "heartbeat_seconds": 300,
        "readonly": true,
    });
    let followup_command = format!(
        "pr_watch action=webhook_heartbeat repo={} pr={} watch_id={}",
        entry.repo, entry.pr, entry.watch_id
    );
    let followup_instructions = format!(
        "Run `{followup_command}` only. Read-only refresh only. Delivery {} event {} triggered this follow-up. Never push, comment, resolve threads, or merge.",
        delivery.delivery_id, delivery.event
    );
    manager.schedule(ScheduleRequest {
        wake_in_minutes: None,
        wake_at: Some(Utc::now() + Duration::seconds(WEBHOOK_DEBOUNCE_SECONDS)),
        context: format!(
            "Webhook follow-up refresh for PR watch {} after lock contention. {}",
            entry.watch_id, followup_instructions
        ),
        priority: Priority::Normal,
        target: ScheduleTarget::Spawn {
            parent_session_id: "pr-watch-webhook-daemon".to_string(),
        },
        created_by_session: "pr-watch-webhook-daemon".to_string(),
        working_dir: Some(entry.root_dir.clone()),
        task_description: Some(format!(
            "PR watch webhook follow-up refresh: {followup_command}"
        )),
        relevant_files: vec![state_file],
        git_branch: None,
        additional_context: Some(format!(
            "Scheduled by pr_watch webhook lock contention. {followup_instructions}"
        )),
        schedule_key: Some(key),
        schedule_kind: Some("pr_watch.webhook_followup".to_string()),
        schedule_payload: Some(payload),
    })?;
    super::ambient::nudge_schedule_runner();
    Ok(false)
}

fn record_webhook_delivery_on_state(
    entry: &WebhookWatchIndexEntry,
    delivery: &VerifiedGithubDelivery,
    coalesced: bool,
) -> Result<()> {
    let root = PathBuf::from(&entry.root_dir);
    let store = watch_dir(&root);
    let Some(_lock) = acquire_watch_lock(&store, &entry.watch_id)? else {
        return Ok(());
    };
    let mut state = load_state_for_params(&store, &webhook_refresh_params(entry, false))?;
    if state.root_dir.as_deref() != Some(entry.root_dir.as_str())
        || state.pr.repo != entry.repo
        || state.pr.number != entry.pr
        || state.watch_id != entry.watch_id
    {
        bail!(
            "indexed watch {} failed webhook delivery metadata validation",
            entry.watch_id
        );
    }
    state.webhook.last_delivery_id = Some(delivery.delivery_id.clone());
    state.webhook.last_delivery_at = Some(now_iso());
    state.webhook.last_event_type = Some(delivery.event.clone());
    state.webhook.last_event_action = delivery.action.clone();
    state.webhook.last_delivery_status =
        Some(if coalesced { "coalesced" } else { "queued" }.to_string());
    if coalesced {
        state.webhook.collapsed_event_count = state.webhook.collapsed_event_count.saturating_add(1);
    }
    write_state_atomic(&state_path(&store, &entry.watch_id), &state)
}

fn webhook_refresh_params(entry: &WebhookWatchIndexEntry, schedule_next: bool) -> PrWatchInput {
    PrWatchInput {
        action: PrWatchAction::PollNow,
        repo: Some(entry.repo.clone()),
        pr: Some(entry.pr),
        watch_id: Some(entry.watch_id.clone()),
        dry_run: Some(false),
        schedule_next,
        poll_interval_seconds: None,
        quiet_cycles_required: None,
        max_runtime_seconds: None,
        target: None,
        scopes: None,
        reason: None,
        expires_in_minutes: None,
        single_use: None,
        grant_id: None,
        thread_ids: Vec::new(),
        head_sha: None,
        commit_sha: None,
        validation: Vec::new(),
        expected_fingerprint: None,
        expected_cycle_number: None,
        no_code_resolution: false,
        event_mode: None,
        fallback_heartbeat_seconds: None,
        webhook_url_hint: None,
    }
}

fn actionable_fingerprint(items: &[ActionableItem]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut canonical: Vec<Value> = items
        .iter()
        .map(|item| {
            json!({
                "surface": item.surface,
                "id": item.id,
                "reason": item.reason,
                "status": item.status,
                "url": item.url,
                "path": item.path,
                "summary_hash": sha256_hex(normalize_handoff_summary(&item.summary).as_bytes()),
            })
        })
        .collect();
    canonical.sort_by_key(|value| {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            value.get("surface").and_then(Value::as_str).unwrap_or(""),
            value.get("id").and_then(Value::as_str).unwrap_or(""),
            value.get("url").and_then(Value::as_str).unwrap_or(""),
            value.get("path").and_then(Value::as_str).unwrap_or(""),
            value.get("reason").and_then(Value::as_str).unwrap_or(""),
            value.get("status").and_then(Value::as_str).unwrap_or(""),
        )
    });
    serde_json::to_vec(&canonical)
        .ok()
        .map(|bytes| sha256_hex(&bytes))
}

fn handoff_prompt(state: &PrWatchState, fingerprint: &str) -> String {
    let state_file = state_file_for_watch(&state.watch_id);
    format!(
        "Action required for PR watch {}. State file: {}. First run/read `pr_watch action=handoff repo={} pr={} watch_id={}` or inspect the state, and verify current actionable fingerprint `{}` still matches before remediation. Current cycle number is `{}`. If stale or no actionable items remain, report no-op. Do not call `pr_watch monitor` from this handoff. If current, inspect pending_actionable and remediate only if the current user workflow or active grants authorize local remediation. No push without an active push grant. No comment without an active comment grant. No review-thread resolution without an active resolve_threads grant. If a review thread is addressed and resolve_threads is granted, completion requires calling `pr_watch action=resolve_addressed` with the addressed thread IDs, current head_sha, expected_fingerprint `{}`, expected_cycle_number `{}`, validation evidence, and commit_sha matching the watched head or explicit no-code reason; otherwise record the blocked reason. Poll after any successful resolution. Never merge.",
        state.watch_id,
        state_file,
        state.pr.repo,
        state.pr.number,
        state.watch_id,
        fingerprint,
        state.polling.cycle_number,
        fingerprint,
        state.polling.cycle_number,
    )
}

#[derive(Debug, Clone)]
struct QueuedHandoffItem {
    id: String,
    created_at: chrono::DateTime<Utc>,
    payload: Option<PrWatchHandoffPayload>,
}

fn handoff_items_for_key(items: &[ScheduledItem], key: &str) -> Vec<QueuedHandoffItem> {
    items
        .iter()
        .filter(|item| item.schedule_key.as_deref() == Some(key))
        .map(|item| {
            let payload = PrWatchHandoffPayload::from_scheduled_item(item)
                .ok()
                .flatten();
            QueuedHandoffItem {
                id: item.id.clone(),
                created_at: item.created_at,
                payload,
            }
        })
        .collect()
}

fn cancel_queued_handoff_ids(manager: &mut AmbientManager, ids: &HashSet<String>) -> usize {
    manager.remove_items_by_id(ids)
}

fn set_handoff_state(
    state: &mut PrWatchState,
    status: ActionRequiredHandoffStatus,
    schedule_id: Option<String>,
    target: Option<String>,
    fingerprint: Option<String>,
    error: Option<String>,
) {
    state.action_required_handoff.status = status;
    state.action_required_handoff.schedule_id = schedule_id;
    state.action_required_handoff.target = target;
    state.action_required_handoff.fingerprint = fingerprint;
    state.action_required_handoff.error = error;
    state.action_required_handoff.updated_at = Some(now_iso());
}

fn infer_origin_session_id(state: &PrWatchState) -> Option<String> {
    state.origin_session_id.clone().or_else(|| {
        state
            .polling
            .last_schedule_target
            .as_deref()
            .and_then(|target| {
                target
                    .strip_prefix("spawn:")
                    .or_else(|| target.strip_prefix("resume:"))
                    .map(ToString::to_string)
            })
    })
}

fn clear_action_required_handoff(store: &Path, state: &mut PrWatchState) -> Result<Option<String>> {
    let key = handoff_schedule_key_for_watch(&state.watch_id);
    let Some(_lock) = acquire_handoff_lock(store, &state.watch_id)? else {
        set_handoff_state(
            state,
            ActionRequiredHandoffStatus::Error,
            state.action_required_handoff.schedule_id.clone(),
            state.action_required_handoff.target.clone(),
            state.action_required_handoff.fingerprint.clone(),
            Some("handoff queue lock busy during cleanup".to_string()),
        );
        return Ok(Some("handoff cleanup lock busy".to_string()));
    };
    let mut manager = AmbientManager::new()?;
    let ids: HashSet<String> = handoff_items_for_key(manager.queue().items(), &key)
        .into_iter()
        .map(|item| item.id)
        .collect();
    let removed = cancel_queued_handoff_ids(&mut manager, &ids);
    set_handoff_state(
        state,
        if removed > 0 {
            ActionRequiredHandoffStatus::Superseded
        } else {
            ActionRequiredHandoffStatus::Missing
        },
        None,
        None,
        None,
        None,
    );
    Ok((removed > 0).then(|| format!("cleared {removed} queued handoff(s)")))
}

fn maybe_schedule_action_required_handoff(
    store: &Path,
    state: &mut PrWatchState,
    ctx: &ToolContext,
) -> Result<Option<String>> {
    let Some(fingerprint) = actionable_fingerprint(&state.pending_actionable) else {
        return clear_action_required_handoff(store, state);
    };
    if state.origin_session_id.is_none() {
        state.origin_session_id = infer_origin_session_id(state);
    }
    let Some(origin_session_id) = state.origin_session_id.clone() else {
        set_handoff_state(
            state,
            ActionRequiredHandoffStatus::MissingOrigin,
            None,
            None,
            Some(fingerprint),
            Some(
                "watch has no origin_session_id; restart/rebind watch from a live session"
                    .to_string(),
            ),
        );
        return Ok(Some("action handoff missing origin_session_id".to_string()));
    };
    if origin_session_id != ctx.session_id && Session::load(&origin_session_id).is_err() {
        set_handoff_state(
            state,
            ActionRequiredHandoffStatus::OriginUnavailable,
            None,
            Some(format!("resume:{origin_session_id}")),
            Some(fingerprint),
            Some(
                "origin session is unavailable; rebind/restart watch from a live session"
                    .to_string(),
            ),
        );
        return Ok(Some("action handoff origin unavailable".to_string()));
    }
    if origin_session_id == ctx.session_id
        && state
            .polling
            .last_schedule_target
            .as_deref()
            .is_some_and(|target| target.starts_with("spawn:"))
    {
        set_handoff_state(
            state,
            ActionRequiredHandoffStatus::SelfTargetGuard,
            None,
            Some(format!("resume:{origin_session_id}")),
            Some(fingerprint),
            Some("scheduled monitor child would target itself; refusing handoff".to_string()),
        );
        return Ok(Some("action handoff self-target guard".to_string()));
    }

    let Some(_handoff_lock) = acquire_handoff_lock(store, &state.watch_id)? else {
        set_handoff_state(
            state,
            ActionRequiredHandoffStatus::Error,
            state.action_required_handoff.schedule_id.clone(),
            Some(format!("resume:{origin_session_id}")),
            Some(fingerprint),
            Some("handoff queue lock busy".to_string()),
        );
        return Ok(Some("action handoff queue lock busy".to_string()));
    };

    let key = handoff_schedule_key_for_watch(&state.watch_id);
    let mut manager = AmbientManager::new()?;
    let existing = handoff_items_for_key(manager.queue().items(), &key);
    let mut same: Vec<QueuedHandoffItem> = existing
        .iter()
        .filter_map(|item| {
            item.payload
                .as_ref()
                .filter(|payload| payload.fingerprint == fingerprint)
                .map(|_| item.clone())
        })
        .collect();
    same.sort_by_key(|item| item.created_at);
    if let Some(keep) = same.first() {
        let duplicate_ids: HashSet<String> =
            same.iter().skip(1).map(|item| item.id.clone()).collect();
        let removed = cancel_queued_handoff_ids(&mut manager, &duplicate_ids);
        set_handoff_state(
            state,
            ActionRequiredHandoffStatus::Queued,
            Some(keep.id.clone()),
            Some(format!("resume:{origin_session_id}")),
            Some(fingerprint),
            None,
        );
        return Ok(Some(if removed > 0 {
            format!(
                "action handoff reused {} and removed {removed} duplicate(s)",
                keep.id
            )
        } else {
            format!("action handoff reused {}", keep.id)
        }));
    }

    let payload = PrWatchHandoffPayload::new(state, fingerprint.clone());
    let id = manager.schedule(ScheduleRequest {
        wake_in_minutes: None,
        wake_at: Some(Utc::now()),
        context: handoff_prompt(state, &fingerprint),
        priority: Priority::High,
        target: ScheduleTarget::Session {
            session_id: origin_session_id.clone(),
        },
        created_by_session: ctx.session_id.clone(),
        working_dir: ctx
            .working_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        task_description: Some(handoff_prompt(state, &fingerprint)),
        relevant_files: vec![state_file_for_watch(&state.watch_id)],
        git_branch: None,
        additional_context: Some("Scheduled by pr_watch ActionRequired handoff; re-check state and grants before remediation.".to_string()),
        schedule_key: Some(key.clone()),
        schedule_kind: Some("pr_watch.action_required_handoff".to_string()),
        schedule_payload: Some(serde_json::to_value(payload)?),
    })?;
    let superseded: HashSet<String> = existing
        .iter()
        .filter_map(|item| {
            item.payload
                .as_ref()
                .filter(|payload| payload.fingerprint != fingerprint)
                .map(|_| item.id.clone())
        })
        .collect();
    let removed = cancel_queued_handoff_ids(&mut manager, &superseded);
    set_handoff_state(
        state,
        ActionRequiredHandoffStatus::Queued,
        Some(id.clone()),
        Some(format!("resume:{origin_session_id}")),
        Some(fingerprint),
        None,
    );
    super::ambient::nudge_schedule_runner();
    Ok(Some(if removed > 0 {
        format!("action handoff scheduled {id}; superseded {removed} older handoff(s)")
    } else {
        format!("action handoff scheduled {id}")
    }))
}

fn maybe_schedule_next_monitor(
    ctx: &ToolContext,
    state: &mut PrWatchState,
    params: &PrWatchInput,
) -> Result<Option<String>> {
    if params.dry_run.unwrap_or(false) || state.terminal {
        if state.terminal {
            let _ = cancel_queued_watch_items(state);
        }
        return Ok(None);
    }
    if !params.schedule_next {
        return Ok(None);
    }
    let wake_at = Utc::now() + Duration::seconds(state.polling.poll_interval_seconds as i64);
    let task = scheduled_monitor_prompt(state, monitor_max_runtime_seconds(params));
    let mut manager = AmbientManager::new()?;
    let existing_ids: Vec<String> =
        find_existing_scheduled_watch_items(manager.queue().items(), state)
            .into_iter()
            .map(|item| item.id.clone())
            .collect();
    let mut future_existing = None;
    for id in &existing_ids {
        if let Some(item) = manager.queue().items().iter().find(|item| item.id == *id)
            && item.scheduled_for > Utc::now()
            && item.schedule_key.as_deref() == Some(&schedule_key_for_watch(&state.watch_id))
        {
            future_existing = Some((item.id.clone(), item.scheduled_for));
            break;
        }
    }
    if let Some((id, scheduled_for)) = future_existing {
        state.polling.duplicate_count = existing_ids.len().saturating_sub(1) as u64;
        state.polling.last_schedule_id = Some(id.clone());
        state.polling.last_schedule_kind = Some("monitor".to_string());
        state.polling.last_schedule_due_at =
            Some(scheduled_for.format("%Y-%m-%dT%H:%M:%SZ").to_string());
        state.polling.next_poll_at = state.polling.last_schedule_due_at.clone();
        return Ok(Some(format!(
            "{} at {} (already scheduled)",
            id,
            scheduled_for.format("%Y-%m-%dT%H:%M:%SZ")
        )));
    }
    for stale_id in existing_ids {
        manager.cancel_schedule(&stale_id)?;
    }
    let target = match params.target.as_deref() {
        Some("spawn") => ScheduleTarget::Spawn {
            parent_session_id: ctx.session_id.clone(),
        },
        Some("resume") | None => ScheduleTarget::Session {
            session_id: ctx.session_id.clone(),
        },
        Some(other) => bail!("invalid schedule target {other}; expected resume or spawn"),
    };
    let target_summary = format_schedule_target_for_state(&target);
    let payload =
        PrWatchSchedulePayload::for_action(state, "monitor", monitor_max_runtime_seconds(params));
    payload.validate_against_state(state)?;
    let id = manager.schedule(ScheduleRequest {
        wake_in_minutes: None,
        wake_at: Some(wake_at),
        context: task.clone(),
        priority: Priority::Normal,
        target,
        created_by_session: ctx.session_id.clone(),
        working_dir: ctx
            .working_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        task_description: Some(task),
        relevant_files: vec![state_file_for_watch(&state.watch_id)],
        git_branch: None,
        additional_context: Some(
            "Scheduled by pr_watch monitor; invoke structured monitor action only. Read-only poll only.".to_string(),
        ),
        schedule_key: Some(schedule_key_for_watch(&state.watch_id)),
        schedule_kind: Some("pr_watch.monitor".to_string()),
        schedule_payload: Some(serde_json::to_value(payload)?),
    })?;
    let scheduled_at = now_iso();
    let due_at = wake_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    state.polling.next_poll_at = Some(due_at.clone());
    state.polling.last_scheduled_at = Some(scheduled_at);
    state.polling.last_schedule_id = Some(id.clone());
    state.polling.last_schedule_kind = Some("monitor".to_string());
    state.polling.last_schedule_target = Some(target_summary);
    state.polling.last_schedule_due_at = Some(due_at);
    state.polling.duplicate_count = 0;
    state.polling.last_schedule_error = None;
    super::ambient::nudge_schedule_runner();
    Ok(Some(format!(
        "{} at {}",
        id,
        wake_at.format("%Y-%m-%dT%H:%M:%SZ")
    )))
}

fn cancel_queued_watch_items(state: &PrWatchState) -> Result<usize> {
    let mut manager = AmbientManager::new()?;
    let ids: HashSet<String> = manager
        .queue()
        .items()
        .iter()
        .filter(|item| scheduled_item_matches_watch(item, state))
        .map(|item| item.id.clone())
        .collect();
    Ok(manager.remove_items_by_id(&ids))
}

fn scheduled_item_matches_watch(item: &ScheduledItem, state: &PrWatchState) -> bool {
    if item.schedule_key.as_deref() == Some(&schedule_key_for_watch(&state.watch_id)) {
        return true;
    }
    if let Ok(Some(payload)) = PrWatchSchedulePayload::from_scheduled_item(item)
        && payload.watch_id == state.watch_id
        && payload.repo == state.pr.repo
        && payload.pr == state.pr.number
    {
        return true;
    }
    let state_file = state_file_for_watch(&state.watch_id);
    let description = item.task_description.as_deref().unwrap_or(&item.context);
    description.contains(&state.watch_id)
        && (item.relevant_files.iter().any(|file| file == &state_file)
            || description.contains(&format!("watch_id={}", state.watch_id)))
}

fn find_existing_scheduled_watch_items<'a>(
    items: &'a [ScheduledItem],
    state: &PrWatchState,
) -> Vec<&'a ScheduledItem> {
    items
        .iter()
        .filter(|item| scheduled_item_matches_watch(item, state))
        .collect()
}

#[cfg(test)]
fn find_existing_scheduled_watch_item<'a>(
    items: &'a [ScheduledItem],
    state: &PrWatchState,
    action: &str,
) -> Option<&'a ScheduledItem> {
    let state_file = state_file_for_watch(&state.watch_id);
    let action_marker = format!("action={action}");
    items.iter().find(|item| {
        if let Ok(Some(payload)) = PrWatchSchedulePayload::from_scheduled_item(item) {
            return payload.watch_id == state.watch_id && payload.action == action;
        }
        let description = item.task_description.as_deref().unwrap_or(&item.context);
        description.contains(&state.watch_id)
            && description.contains(&action_marker)
            && (item.relevant_files.iter().any(|file| file == &state_file)
                || description.contains(&format!("watch_id={}", state.watch_id)))
    })
}

fn format_schedule_target_for_state(target: &ScheduleTarget) -> String {
    match target {
        ScheduleTarget::Ambient => "ambient".to_string(),
        ScheduleTarget::Session { session_id } => format!("resume:{session_id}"),
        ScheduleTarget::Spawn { parent_session_id } => format!("spawn:{parent_session_id}"),
    }
}

#[cfg(test)]
fn scheduled_poll_prompt(state: &PrWatchState) -> String {
    let state_file = format!(".jcode/pr-feedback-watch/{}-state.json", state.watch_id);
    if state.last_successful_fetch.is_empty() {
        return format!(
            "Run the first read-only PR watch baseline acknowledgement for {}. State file: {}. Use pr_watch with action=ack_baseline, repo={}, pr={}, watch_id={}, schedule_next=true. Do not push, comment, resolve threads, or merge.",
            state.watch_id, state_file, state.pr.repo, state.pr.number, state.watch_id
        );
    }
    format!(
        "Run the next read-only PR watch poll for {}. State file: {}. Use pr_watch with action=poll_now, repo={}, pr={}, watch_id={}, schedule_next=true. Do not push, comment, resolve threads, or merge.",
        state.watch_id, state_file, state.pr.repo, state.pr.number, state.watch_id
    )
}

fn scheduled_monitor_prompt(state: &PrWatchState, max_runtime_seconds: u64) -> String {
    let state_file = format!(".jcode/pr-feedback-watch/{}-state.json", state.watch_id);
    let grant_note = if state.authorization.active_grants.is_empty() {
        "No active remediation grant is recorded."
    } else {
        "A remediation grant may be recorded, but this scheduled monitor must remain read-only; use a separate explicit remediation workflow to consume grants."
    };
    format!(
        "Run the next structured PR watch monitor cycle for {}. State file: {}. Use pr_watch with action=monitor, repo={}, pr={}, watch_id={}, schedule_next=true, poll_interval_seconds={}, quiet_cycles_required={}, max_runtime_seconds={}. {} The monitor is read-only: do not push, comment, resolve threads, or merge.",
        state.watch_id,
        state_file,
        state.pr.repo,
        state.pr.number,
        state.watch_id,
        state.polling.poll_interval_seconds,
        state.polling.required_quiet_cycles,
        max_runtime_seconds,
        grant_note,
    )
}

fn scheduled_webhook_heartbeat_prompt(state: &PrWatchState, max_runtime_seconds: u64) -> String {
    let state_file = format!(".jcode/pr-feedback-watch/{}-state.json", state.watch_id);
    format!(
        "Run the low-frequency read-only webhook heartbeat for {}. State file: {}. Use pr_watch with action=webhook_heartbeat, repo={}, pr={}, watch_id={}, schedule_next=true, max_runtime_seconds={}. This heartbeat is a safety net for webhook-mode watches only: do not push, comment, resolve threads, or merge.",
        state.watch_id,
        state_file,
        state.pr.repo,
        state.pr.number,
        state.watch_id,
        max_runtime_seconds,
    )
}

async fn webhook_heartbeat(
    root: &Path,
    store: &Path,
    mut params: PrWatchInput,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    params.event_mode.get_or_insert(PrWatchEventMode::Webhook);
    let state = load_state_for_params(store, &params)?;
    let entry = WebhookWatchIndexEntry {
        watch_id: state.watch_id.clone(),
        repo: state.pr.repo.clone(),
        pr: state.pr.number,
        root_dir: state
            .root_dir
            .clone()
            .unwrap_or_else(|| root.display().to_string()),
        state_path: state_path(store, &state.watch_id).display().to_string(),
        event_mode: state.webhook.mode.clone(),
        active: !state.terminal,
        updated_at: now_iso(),
    };
    let delivery = VerifiedGithubDelivery {
        delivery_id: format!("heartbeat-{}", now_iso()),
        event: "webhook_heartbeat".to_string(),
        action: Some("heartbeat".to_string()),
        repo: Some(entry.repo.clone()),
        pr: Some(entry.pr),
        payload: json!({"source":"webhook_heartbeat"}),
    };
    let result = webhook_refresh_watch(&entry, &delivery).await?;
    let refreshed = load_state_for_params(store, &params)?;
    Ok(ToolOutput::new(format!(
        "PR watch webhook heartbeat: {}\nRepo: {}\nPR: #{}\nResult: {}",
        refreshed.watch_id, refreshed.pr.repo, refreshed.pr.number, result
    ))
    .with_title("webhook heartbeat".to_string())
    .with_metadata(json!({"watch": refreshed, "result": result, "ctx_session": ctx.session_id})))
}

async fn ack_baseline(
    root: &Path,
    store: &Path,
    params: PrWatchInput,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let mut state = load_state_for_params(store, &params)?;
    let path = state_path(store, &state.watch_id);
    let loaded_updated_at = state.updated_at.clone();
    let loaded_cycle_number = state.polling.cycle_number;
    let would_write = !params.dry_run.unwrap_or(false);
    if state.terminal {
        let readiness = state.readiness();
        return Ok(ToolOutput::new(format!(
            "PR watch is stopped: {}\nRepo: {}\nPR: #{}\nStop reason: {}\nNo poll was run and no state was changed.",
            state.watch_id,
            state.pr.repo,
            state.pr.number,
            state.stop_reason.as_deref().unwrap_or("terminal")
        ))
        .with_title(format!("{} stopped", state.watch_id))
        .with_metadata(json!({"watch": state, "readiness": readiness, "written": false})));
    }
    let _lock = if would_write {
        match acquire_watch_lock(store, &state.watch_id)? {
            Some(lock) => Some(lock),
            None => return Ok(watch_locked_output(store, &state, "ack_baseline")),
        }
    } else {
        None
    };
    let collected_at = now_iso();
    let collection = collect_with_gh(root, &state.pr.repo, state.pr.number).await;
    let partial_failure = apply_baseline_from_collection(&mut state, collection, &collected_at);
    apply_schedule_fields(&mut state, &params);
    if would_write {
        let current_text = fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to re-read {} before writing poll result",
                path.display()
            )
        })?;
        let current_state = normalize_watch_state_json(&current_text).with_context(|| {
            format!(
                "failed to parse {} before writing poll result",
                path.display()
            )
        })?;
        if current_state.updated_at != loaded_updated_at
            || current_state.polling.cycle_number != loaded_cycle_number
        {
            let readiness = current_state.readiness();
            return Ok(ToolOutput::new(format!(
                "PR watch poll result is stale: {}\nRepo: {}\nPR: #{}\nNo state was changed because another poll updated the watch first.",
                current_state.watch_id, current_state.pr.repo, current_state.pr.number
            ))
            .with_title(format!("{} stale poll", current_state.watch_id))
            .with_metadata(json!({"watch": current_state, "readiness": readiness, "written": false, "stale_poll": true})));
        }
    }
    let scheduled = maybe_schedule_next(ctx, &mut state, &params)?;
    if would_write {
        write_state_atomic(&path, &state)?;
    }
    let text = format!(
        "PR watch baseline acknowledged: {}\nRepo: {}\nPR: #{}\nUnresolved threads: {}\nReview comments seen: {}\nIssue comments seen: {}\nReviews seen: {}\nPartial failure: {}{}{}",
        state.watch_id,
        state.pr.repo,
        state.pr.number,
        state.baseline.unresolved_thread_ids.len(),
        state.last_seen.review_comments.len(),
        state.last_seen.issue_comments.len(),
        state.last_seen.reviews.len(),
        partial_failure,
        scheduled
            .as_deref()
            .map(|s| format!("\nScheduled: {s}"))
            .unwrap_or_default(),
        if would_write {
            ""
        } else {
            "\nDry run: no file written"
        }
    );
    Ok(ToolOutput::new(text)
        .with_title(format!("baseline {}", state.watch_id))
        .with_metadata(
            json!({"watch": state, "partial_failure": partial_failure, "written": would_write}),
        ))
}

fn apply_baseline_from_collection(
    state: &mut PrWatchState,
    collection: GhCollection,
    collected_at: &str,
) -> bool {
    state.updated_at = Some(collected_at.to_string());
    state.baseline.established_at = Some(collected_at.to_string());
    state.polling.quiet_cycles = 0;
    state.pending_actionable.clear();
    state.last_cycle.completed_at = Some(collected_at.to_string());
    state.last_cycle.status = jcode_pr_watch_core::CycleStatus::BaselineEstablished;
    state.last_cycle.actionable_count = 0;
    state.last_cycle.pending_check_count = 0;
    state.last_cycle.failed_check_count = 0;
    state.last_cycle.surfaces_checked = vec![
        "metadata".to_string(),
        "checks".to_string(),
        "review_comments".to_string(),
        "issue_comments".to_string(),
        "reviews".to_string(),
        "review_threads".to_string(),
    ];
    state.last_cycle.surface_counts = BTreeMap::new();
    let mut partial_failure = false;

    match collection.metadata {
        Ok(metadata) => {
            state.pr = metadata.identity;
            state.baseline.head_sha = state.pr.head_sha.clone();
            state
                .last_successful_fetch
                .insert("metadata".to_string(), collected_at.to_string());
            state
                .last_cycle
                .surface_counts
                .insert("metadata".to_string(), 1);
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
    }

    match collection.checks {
        Ok(checks) => {
            state.last_checks_for_sha.head_sha = state.pr.head_sha.clone();
            state.last_checks_for_sha.runs = checks;
            state
                .last_successful_fetch
                .insert("checks".to_string(), collected_at.to_string());
            state
                .last_cycle
                .surface_counts
                .insert("checks".to_string(), state.last_checks_for_sha.runs.len());
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
    }

    match collection.review_comments {
        Ok(comments) => {
            state.baseline.review_comment_count = comments.len();
            state
                .last_cycle
                .surface_counts
                .insert("review_comments".to_string(), comments.len());
            state
                .last_successful_fetch
                .insert("review_comments".to_string(), collected_at.to_string());
            for comment in comments {
                state.last_seen.review_comments.insert(
                    comment.id.clone(),
                    Marker {
                        id: comment.id,
                        updated_at: comment.updated_at,
                        author: comment.author,
                        body_hash: comment.body.as_ref().map(|body| stable_body_hash(body)),
                        url: comment.url,
                    },
                );
            }
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
    }

    match collection.issue_comments {
        Ok(comments) => {
            state.baseline.issue_comment_count = comments.len();
            state
                .last_cycle
                .surface_counts
                .insert("issue_comments".to_string(), comments.len());
            state
                .last_successful_fetch
                .insert("issue_comments".to_string(), collected_at.to_string());
            for comment in comments {
                state.last_seen.issue_comments.insert(
                    comment.id.clone(),
                    Marker {
                        id: comment.id,
                        updated_at: comment.updated_at,
                        author: comment.author,
                        body_hash: comment.body.as_ref().map(|body| stable_body_hash(body)),
                        url: comment.url,
                    },
                );
            }
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
    }

    match collection.reviews {
        Ok(reviews) => {
            state.baseline.review_count = reviews.len();
            state
                .last_cycle
                .surface_counts
                .insert("reviews".to_string(), reviews.len());
            state
                .last_successful_fetch
                .insert("reviews".to_string(), collected_at.to_string());
            for review in reviews {
                state.last_seen.reviews.insert(
                    review.id.clone(),
                    Marker {
                        id: review.id,
                        updated_at: review.submitted_at,
                        author: review.author,
                        body_hash: review.body.as_ref().map(|body| stable_body_hash(body)),
                        url: None,
                    },
                );
            }
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
    }

    match collection.review_threads {
        Ok(threads) => {
            state.baseline.unresolved_thread_ids = threads
                .iter()
                .filter(|thread| !thread.is_resolved && !thread.is_outdated)
                .map(|thread| thread.id.clone())
                .collect();
            state
                .last_cycle
                .surface_counts
                .insert("review_threads".to_string(), threads.len());
            state
                .last_successful_fetch
                .insert("review_threads".to_string(), collected_at.to_string());
            for thread in threads {
                state.last_seen.review_threads.insert(
                    thread.id.clone(),
                    jcode_pr_watch_core::ReviewThreadMarker {
                        id: thread.id,
                        updated_at: thread.updated_at,
                        resolved: thread.is_resolved,
                        outdated: thread.is_outdated,
                        body_hash: thread.body.as_ref().map(|body| stable_body_hash(body)),
                        url: thread.url,
                    },
                );
            }
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
    }

    state.push_event(WatchEvent {
        at: collected_at.to_string(),
        kind: "baseline_acknowledged".to_string(),
        data: json!({
            "partial_failure": partial_failure,
            "review_comment_count": state.baseline.review_comment_count,
            "issue_comment_count": state.baseline.issue_comment_count,
            "review_count": state.baseline.review_count,
            "unresolved_thread_count": state.baseline.unresolved_thread_ids.len(),
        }),
    });
    partial_failure
}

async fn poll_now(
    root: &Path,
    store: &Path,
    params: PrWatchInput,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let mut state = load_state_for_params(store, &params)?;
    let path = state_path(store, &state.watch_id);
    let loaded_updated_at = state.updated_at.clone();
    let loaded_cycle_number = state.polling.cycle_number;
    let would_write = !params.dry_run.unwrap_or(false);
    if state.terminal {
        let readiness = state.readiness();
        return Ok(ToolOutput::new(format!(
            "PR watch is stopped: {}\nRepo: {}\nPR: #{}\nStop reason: {}\nNo poll was run and no state was changed.",
            state.watch_id,
            state.pr.repo,
            state.pr.number,
            state.stop_reason.as_deref().unwrap_or("terminal")
        ))
        .with_title(format!("{} stopped", state.watch_id))
        .with_metadata(json!({"watch": state, "readiness": readiness, "written": false})));
    }
    let _lock = if would_write {
        match acquire_watch_lock(store, &state.watch_id)? {
            Some(lock) => Some(lock),
            None => return Ok(watch_locked_output(store, &state, "poll_now")),
        }
    } else {
        None
    };
    let collected_at = now_iso();
    let result = collect_with_gh(root, &state.pr.repo, state.pr.number).await;
    let outcome = update_state_from_collection(&mut state, result, &collected_at);
    if review_threads_fetch_succeeded_at(&state, &collected_at) {
        state.resolution_requires_post_poll = false;
    }
    apply_schedule_fields(&mut state, &params);
    let handoff = if !state.pending_actionable.is_empty() && would_write {
        maybe_schedule_action_required_handoff(store, &mut state, ctx)?
    } else if would_write {
        clear_action_required_handoff(store, &mut state)?
    } else {
        None
    };
    if state.polling.quiet_cycles >= state.polling.required_quiet_cycles
        && state.pending_actionable.is_empty()
        && state.last_cycle.pending_check_count == 0
        && state.last_cycle.failed_check_count == 0
        && !outcome.partial_failure
    {
        state.terminal = true;
        state.stop_reason = Some("quiet_cycles_satisfied".to_string());
        state.polling.next_poll_at = None;
    }
    if would_write {
        let current_text = fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to re-read {} before writing poll result",
                path.display()
            )
        })?;
        let current_state = normalize_watch_state_json(&current_text).with_context(|| {
            format!(
                "failed to parse {} before writing poll result",
                path.display()
            )
        })?;
        if current_state.updated_at != loaded_updated_at
            || current_state.polling.cycle_number != loaded_cycle_number
        {
            let readiness = current_state.readiness();
            return Ok(ToolOutput::new(format!(
                "PR watch poll result is stale: {}\nRepo: {}\nPR: #{}\nNo state was changed because another poll updated the watch first.",
                current_state.watch_id, current_state.pr.repo, current_state.pr.number
            ))
            .with_title(format!("{} stale poll", current_state.watch_id))
            .with_metadata(json!({"watch": current_state, "readiness": readiness, "written": false, "stale_poll": true})));
        }
    }
    let scheduled = maybe_schedule_next(ctx, &mut state, &params)?;
    if would_write {
        if state.terminal {
            remove_webhook_index_entry(&state.watch_id)?;
        }
        write_state_atomic(&path, &state)?;
    }
    let readiness = state.readiness();
    let text = format!(
        "PR watch polled: {}\nRepo: {}\nPR: #{}\nState: {:?}\nReadiness: {:?}\nQuiet cycles: {}/{}\nActionable: {}\nPending checks: {}\nFailed checks: {}\nPartial failure: {}\nFailed surfaces: {}{}{}{}",
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
        outcome.partial_failure,
        failed_surface_names(&state).join(", "),
        scheduled
            .as_deref()
            .map(|s| format!("\nScheduled: {s}"))
            .unwrap_or_default(),
        handoff
            .as_deref()
            .map(|s| format!("\nAction handoff: {s}"))
            .unwrap_or_default(),
        if would_write {
            ""
        } else {
            "\nDry run: no file written"
        }
    );
    Ok(ToolOutput::new(text)
        .with_title(format!("{} {:?}", state.watch_id, state.last_cycle.status))
        .with_metadata(json!({"watch": state, "readiness": readiness, "written": would_write})))
}

async fn monitor_once(
    root: &Path,
    store: &Path,
    params: PrWatchInput,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let watch_id = match &params.watch_id {
        Some(id) => id.clone(),
        None => target_from_params(&params)?.watch_id(),
    };
    let path = state_path(store, &watch_id);
    let Some(lock) = acquire_watch_lock(store, &watch_id)? else {
        return Ok(ToolOutput::new(format!(
            "PR watch monitor already running: {watch_id}\nLock: {}\nNo state was changed.",
            lock_path(store, &watch_id).display()
        ))
        .with_title(format!("{} monitor locked", watch_id))
        .with_metadata(json!({"watch_id": watch_id, "monitor_status": MonitorStatus::AlreadyRunning.as_str(), "written": false})));
    };

    let loaded_existing_state = path.exists();
    let mut state = if loaded_existing_state {
        load_state_for_params(store, &params)?
    } else {
        let target = target_from_params(&params)?;
        let mut state = PrWatchState::new(target);
        if let Some(id) = &params.watch_id {
            state.watch_id = id.clone();
        }
        state.created_at = Some(now_iso());
        state
    };
    let loaded_updated_at = state.updated_at.clone();
    let loaded_cycle_number = state.polling.cycle_number;

    let max_runtime_seconds = monitor_max_runtime_seconds(&params);
    let would_write = !params.dry_run.unwrap_or(false);
    let mut partial_failure = false;
    let mode;

    if state.terminal {
        mode = "terminal";
    } else {
        let collected_at = now_iso();
        let collection = match tokio::time::timeout(
            StdDuration::from_secs(max_runtime_seconds),
            collect_with_gh(root, &state.pr.repo, state.pr.number),
        )
        .await
        {
            Ok(collection) => collection,
            Err(_) => timed_out_collection(max_runtime_seconds),
        };
        if state.last_successful_fetch.is_empty() {
            partial_failure = apply_baseline_from_collection(&mut state, collection, &collected_at);
            mode = "baseline";
        } else {
            let outcome = update_state_from_collection(&mut state, collection, &collected_at);
            if review_threads_fetch_succeeded_at(&state, &collected_at) {
                state.resolution_requires_post_poll = false;
            }
            partial_failure = outcome.partial_failure;
            mode = "poll";
        }
        apply_schedule_fields(&mut state, &params);
        if state.polling.quiet_cycles >= state.polling.required_quiet_cycles
            && state.pending_actionable.is_empty()
            && state.last_cycle.pending_check_count == 0
            && state.last_cycle.failed_check_count == 0
            && !partial_failure
        {
            state.terminal = true;
            state.stop_reason = Some("quiet_cycles_satisfied".to_string());
            state.polling.next_poll_at = None;
        }
        state.push_event(WatchEvent {
            at: collected_at,
            kind: "monitor_cycle_completed".to_string(),
            data: json!({
                "mode": mode,
                "max_runtime_seconds": max_runtime_seconds,
                "partial_failure": partial_failure,
            }),
        });
    }

    if would_write {
        fs::create_dir_all(store)?;
        if path.exists() {
            let current_text = fs::read_to_string(&path).with_context(|| {
                format!(
                    "failed to re-read {} before writing monitor result",
                    path.display()
                )
            })?;
            let current_state = normalize_watch_state_json(&current_text).with_context(|| {
                format!(
                    "failed to parse {} before writing monitor result",
                    path.display()
                )
            })?;
            if watch_state_changed_since_load(
                &current_state,
                loaded_existing_state,
                &loaded_updated_at,
                loaded_cycle_number,
            ) {
                let readiness = current_state.readiness();
                return Ok(ToolOutput::new(format!(
                    "PR watch monitor result is stale: {}\nRepo: {}\nPR: #{}\nNo state was changed because another watch action updated the state first.",
                    current_state.watch_id, current_state.pr.repo, current_state.pr.number
                ))
                .with_title(format!("{} stale monitor", current_state.watch_id))
                .with_metadata(json!({"watch": current_state, "readiness": readiness, "monitor_status": "stale", "written": false, "stale_monitor": true})));
            }
        } else if loaded_existing_state {
            return Ok(ToolOutput::new(format!(
                "PR watch monitor result is stale: {watch_id}\nState path disappeared before write: {}\nNo state was changed.",
                path.display()
            ))
            .with_title(format!("{} stale monitor", watch_id))
            .with_metadata(json!({"watch_id": watch_id, "monitor_status": "stale", "written": false, "stale_monitor": true})));
        }
    }
    let status = monitor_status_for_state(&state, partial_failure);
    let scheduled = if monitor_should_schedule_followup(status) {
        maybe_schedule_next(ctx, &mut state, &params)?
    } else {
        None
    };
    let handoff = if status == MonitorStatus::ActionRequired && would_write {
        maybe_schedule_action_required_handoff(store, &mut state, ctx)?
    } else if !state.pending_actionable.is_empty() || !would_write {
        None
    } else {
        clear_action_required_handoff(store, &mut state)?
    };
    if would_write {
        if state.terminal {
            remove_webhook_index_entry(&state.watch_id)?;
        }
        write_state_atomic(&path, &state)?;
    }
    drop(lock);
    let readiness = state.readiness();
    let text = format!(
        "PR watch monitor cycle: {}\nRepo: {}\nPR: #{}\nMode: {}\nMonitor status: {}\nState: {:?}\nReadiness: {}\nQuiet cycles: {}/{}\nActionable: {}\nPending checks: {}\nFailed checks: {}\nPartial failure: {}\nMax runtime seconds: {}\nState path: {}{}{}{}",
        state.watch_id,
        state.pr.repo,
        state.pr.number,
        mode,
        status.as_str(),
        state.last_cycle.status,
        readiness_label(&readiness),
        state.polling.quiet_cycles,
        state.polling.required_quiet_cycles,
        state.pending_actionable.len(),
        state.last_cycle.pending_check_count,
        state.last_cycle.failed_check_count,
        partial_failure,
        max_runtime_seconds,
        path.display(),
        scheduled
            .as_deref()
            .map(|s| format!("\nScheduled: {s}"))
            .unwrap_or_default(),
        handoff
            .as_deref()
            .map(|s| format!("\nAction handoff: {s}"))
            .unwrap_or_default(),
        if would_write {
            ""
        } else {
            "\nDry run: no file written"
        }
    );
    Ok(ToolOutput::new(text)
        .with_title(format!("{} monitor {}", state.watch_id, status.as_str()))
        .with_metadata(json!({
            "watch": state,
            "readiness": readiness,
            "monitor_status": status.as_str(),
            "monitor_mode": mode,
            "max_runtime_seconds": max_runtime_seconds,
            "scheduled": scheduled,
            "written": would_write,
        })))
}

async fn collect_with_gh(root: &Path, repo: &str, pr: u64) -> GhCollection {
    GhCollection {
        metadata: run_gh(root, &["pr", "view", &pr.to_string(), "--repo", repo, "--json", "url,state,baseRefName,headRefName,headRefOid,mergeStateStatus,reviewDecision,isDraft"]).await
            .and_then(|stdout| parse_gh_pr_view(repo, pr, &stdout).map_err(|err| SurfaceError::transient("metadata", err.to_string()))),
        checks: run_gh_pr_checks(root, repo, pr).await.and_then(|stdout| {
            parse_gh_checks(&stdout).map_err(|err| SurfaceError::transient("checks", err.to_string()))
        }),
        review_comments: run_gh(root, &["api", &format!("repos/{repo}/pulls/{pr}/comments"), "--paginate"]).await
            .and_then(|stdout| parse_gh_review_comments(&stdout).map_err(|err| SurfaceError::transient("review_comments", err.to_string()))),
        issue_comments: run_gh(root, &["api", &format!("repos/{repo}/issues/{pr}/comments"), "--paginate"]).await
            .and_then(|stdout| parse_gh_issue_comments(&stdout).map_err(|err| SurfaceError::transient("issue_comments", err.to_string()))),
        reviews: run_gh(root, &["api", &format!("repos/{repo}/pulls/{pr}/reviews"), "--paginate"]).await
            .and_then(|stdout| parse_gh_reviews(&stdout).map_err(|err| SurfaceError::transient("reviews", err.to_string()))),
        review_threads: run_gh_graphql_review_threads(root, repo, pr).await
            .and_then(|stdout| parse_gh_review_threads(&stdout).map_err(|err| SurfaceError::transient("review_threads", err.to_string()))),
    }
}

#[derive(Debug)]
struct GhCollection {
    metadata: Result<jcode_pr_watch_core::PrMetadata, SurfaceError>,
    checks: Result<Vec<CheckRunState>, SurfaceError>,
    review_comments: Result<Vec<jcode_pr_watch_core::ReviewComment>, SurfaceError>,
    issue_comments: Result<Vec<jcode_pr_watch_core::IssueComment>, SurfaceError>,
    reviews: Result<Vec<jcode_pr_watch_core::Review>, SurfaceError>,
    review_threads: Result<Vec<jcode_pr_watch_core::ReviewThread>, SurfaceError>,
}

async fn run_gh_graphql_review_threads(
    root: &Path,
    repo: &str,
    pr: u64,
) -> Result<String, SurfaceError> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| SurfaceError::permanent("review_threads", "repo must be owner/name"))?;
    let pr_s = pr.to_string();
    let mut after: Option<String> = None;
    let mut all_nodes: Vec<Value> = Vec::new();

    loop {
        let after_clause = after
            .as_ref()
            .map(|cursor| format!(", after:\"{}\"", cursor.replace('"', "\\\"")))
            .unwrap_or_default();
        let query = format!(
            r#"
query($owner:String!, $name:String!, $number:Int!) {{
  repository(owner:$owner, name:$name) {{
    pullRequest(number:$number) {{
      reviewThreads(first:100{after_clause}) {{
        pageInfo {{ hasNextPage endCursor }}
        nodes {{
          id
          isResolved
          isOutdated
          comments(first:100) {{
            pageInfo {{ hasNextPage endCursor }}
            nodes {{
              path
              line
              url
              body
              createdAt
              updatedAt
              author {{ login }}
            }}
          }}
        }}
      }}
    }}
  }}
}}
"#
        );
        let stdout = run_gh(
            root,
            &[
                "api",
                "graphql",
                "-f",
                &format!("owner={owner}"),
                "-f",
                &format!("name={name}"),
                "-F",
                &format!("number={pr_s}"),
                "-f",
                &format!("query={query}"),
            ],
        )
        .await?;
        let page: Value = serde_json::from_str(&stdout).map_err(|err| {
            SurfaceError::transient("review_threads", format!("invalid GraphQL JSON: {err}"))
        })?;
        let connection = page
            .pointer("/data/repository/pullRequest/reviewThreads")
            .ok_or_else(|| {
                SurfaceError::transient("review_threads", "missing reviewThreads connection")
            })?;
        if let Some(nodes) = connection.get("nodes").and_then(Value::as_array) {
            for node in nodes {
                let enriched = enrich_review_thread_comments(root, node.clone()).await?;
                all_nodes.push(enriched);
            }
        }
        let page_info = connection.get("pageInfo").unwrap_or(&Value::Null);
        if !page_info
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            break;
        }
        after = page_info
            .get("endCursor")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if after.is_none() {
            return Err(SurfaceError::transient(
                "review_threads",
                "reviewThreads pageInfo indicated more pages without an endCursor",
            ));
        }
    }

    Ok(
        json!({"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes": all_nodes}}}}})
            .to_string(),
    )
}

async fn enrich_review_thread_comments(
    root: &Path,
    mut thread: Value,
) -> Result<Value, SurfaceError> {
    let thread_id = thread
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| SurfaceError::transient("review_threads", "thread missing id"))?
        .to_string();
    let mut after = thread
        .pointer("/comments/pageInfo/endCursor")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let mut has_next = thread
        .pointer("/comments/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    while has_next {
        let after_clause = after
            .as_ref()
            .map(|cursor| format!(", after:\"{}\"", cursor.replace('"', "\\\"")))
            .unwrap_or_default();
        let query = format!(
            r#"
query($threadId:ID!) {{
  node(id:$threadId) {{
    ... on PullRequestReviewThread {{
      comments(first:100{after_clause}) {{
        pageInfo {{ hasNextPage endCursor }}
        nodes {{
          path
          line
          url
          body
          createdAt
          updatedAt
          author {{ login }}
        }}
      }}
    }}
  }}
}}
"#
        );
        let stdout = run_gh(
            root,
            &[
                "api",
                "graphql",
                "-f",
                &format!("threadId={thread_id}"),
                "-f",
                &format!("query={query}"),
            ],
        )
        .await?;
        let page: Value = serde_json::from_str(&stdout).map_err(|err| {
            SurfaceError::transient(
                "review_threads",
                format!("invalid thread comments GraphQL JSON: {err}"),
            )
        })?;
        let comments = page.pointer("/data/node/comments").ok_or_else(|| {
            SurfaceError::transient("review_threads", "missing thread comments connection")
        })?;
        let extra_nodes = comments
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(existing) = thread
            .pointer_mut("/comments/nodes")
            .and_then(Value::as_array_mut)
        {
            existing.extend(extra_nodes);
        }
        let page_info = comments.get("pageInfo").unwrap_or(&Value::Null);
        has_next = page_info
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        after = page_info
            .get("endCursor")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if has_next && after.is_none() {
            return Err(SurfaceError::transient(
                "review_threads",
                "thread comments pageInfo indicated more pages without an endCursor",
            ));
        }
    }
    Ok(thread)
}

async fn run_gh(root: &Path, args: &[&str]) -> Result<String, SurfaceError> {
    run_gh_allow_exit(root, args, &[]).await
}

async fn run_gh_resolve_review_thread(
    root: &Path,
    thread_id: &str,
) -> Result<ResolveReviewThreadOutcome> {
    let mutation = "mutation($threadId: ID!) { resolveReviewThread(input: {threadId: $threadId}) { thread { id isResolved } } }";
    let variable = format!("threadId={thread_id}");
    let output = Command::new("gh")
        .args([
            "api",
            "graphql",
            "-f",
            &format!("query={mutation}"),
            "-f",
            &variable,
        ])
        .current_dir(root)
        .output()
        .await
        .map_err(|err| anyhow::anyhow!("failed to run gh: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.to_ascii_lowercase().contains("already resolved") {
            return Ok(ResolveReviewThreadOutcome::AlreadyResolved);
        }
        bail!("gh resolveReviewThread failed: {stderr}");
    }
    parse_resolve_review_thread_output(&String::from_utf8_lossy(&output.stdout))
}

fn parse_resolve_review_thread_output(output: &str) -> Result<ResolveReviewThreadOutcome> {
    let value: Value = serde_json::from_str(output)
        .map_err(|err| anyhow::anyhow!("malformed resolveReviewThread JSON: {err}"))?;
    if let Some(errors) = value.get("errors").and_then(Value::as_array)
        && !errors.is_empty()
    {
        let text = errors
            .iter()
            .filter_map(|error| error.get("message").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("; ");
        if text.to_ascii_lowercase().contains("already resolved") {
            return Ok(ResolveReviewThreadOutcome::AlreadyResolved);
        }
        return Ok(ResolveReviewThreadOutcome::MalformedResponse(format!(
            "GitHub GraphQL errors: {text}"
        )));
    }
    let Some(is_resolved) = value
        .pointer("/data/resolveReviewThread/thread/isResolved")
        .and_then(Value::as_bool)
    else {
        return Ok(ResolveReviewThreadOutcome::MalformedResponse(
            "missing data.resolveReviewThread.thread.isResolved".to_string(),
        ));
    };
    if is_resolved {
        Ok(ResolveReviewThreadOutcome::Resolved)
    } else {
        Ok(ResolveReviewThreadOutcome::NotResolved)
    }
}

async fn run_gh_pr_checks(root: &Path, repo: &str, pr: u64) -> Result<String, SurfaceError> {
    let pr_s = pr.to_string();
    let args = [
        "pr",
        "checks",
        &pr_s,
        "--repo",
        repo,
        "--json",
        "name,state,event,link,bucket,workflow,description,startedAt,completedAt",
    ];
    let output = Command::new("gh")
        .args(args)
        .current_dir(root)
        .output()
        .await
        .map_err(|err| SurfaceError::transient("gh", format!("failed to run gh: {err}")))?;
    let code = output.status.code().unwrap_or(-1);
    if output.status.success() || code == 8 {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    if gh_pr_checks_reported_no_checks(code, &output.stdout, &output.stderr) {
        return Ok("[]".to_string());
    }
    Err(SurfaceError::transient(
        "gh",
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    ))
}

fn gh_pr_checks_reported_no_checks(code: i32, stdout: &[u8], stderr: &[u8]) -> bool {
    if code != 1 {
        return false;
    }
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    )
    .to_ascii_lowercase();
    combined.contains("no checks reported")
}

async fn run_gh_allow_exit(
    root: &Path,
    args: &[&str],
    allowed_nonzero_exit_codes: &[i32],
) -> Result<String, SurfaceError> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(root)
        .output()
        .await
        .map_err(|err| SurfaceError::transient("gh", format!("failed to run gh: {err}")))?;
    let code = output.status.code().unwrap_or(-1);
    if !output.status.success() && !allowed_nonzero_exit_codes.contains(&code) {
        return Err(SurfaceError::transient(
            "gh",
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn update_state_from_collection(
    state: &mut PrWatchState,
    collection: GhCollection,
    collected_at: &str,
) -> CycleOutcome {
    state.updated_at = Some(collected_at.to_string());
    state.last_cycle.completed_at = Some(collected_at.to_string());
    state.last_cycle.surfaces_checked = vec![
        "metadata".to_string(),
        "checks".to_string(),
        "review_comments".to_string(),
        "issue_comments".to_string(),
        "reviews".to_string(),
        "review_threads".to_string(),
    ];
    state.last_cycle.surface_counts = BTreeMap::new();

    let mut partial_failure = false;
    let mut pending_actionable = Vec::new();
    let mut pending_check_count = 0;
    let mut failed_check_count = 0;
    let mut any_surface_success = false;

    match collection.metadata {
        Ok(metadata) => {
            any_surface_success = true;
            let previous_head_sha = state.pr.head_sha.clone();
            let next_head_sha = metadata.identity.head_sha.clone();
            if previous_head_sha.is_some()
                && next_head_sha.is_some()
                && previous_head_sha != next_head_sha
            {
                state.polling.quiet_cycles = 0;
                state.baseline.head_sha = next_head_sha.clone();
                state.push_event(WatchEvent {
                    at: collected_at.to_string(),
                    kind: "head_changed".to_string(),
                    data: json!({
                        "previous_head_sha": previous_head_sha,
                        "new_head_sha": next_head_sha,
                    }),
                });
            }
            state.pr = metadata.identity;
            state
                .last_successful_fetch
                .insert("metadata".to_string(), collected_at.to_string());
            state
                .last_cycle
                .surface_counts
                .insert("metadata".to_string(), 1);
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
    }

    match collection.checks {
        Ok(checks) => {
            any_surface_success = true;
            pending_check_count = checks
                .iter()
                .filter(|check| is_pending_check(check))
                .count();
            failed_check_count = checks.iter().filter(|check| is_failed_check(check)).count();
            state.last_checks_for_sha.head_sha = state.pr.head_sha.clone();
            state.last_checks_for_sha.runs = checks;
            state
                .last_successful_fetch
                .insert("checks".to_string(), collected_at.to_string());
            state
                .last_cycle
                .surface_counts
                .insert("checks".to_string(), state.last_checks_for_sha.runs.len());
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
    }

    match collection.review_comments {
        Ok(comments) => {
            any_surface_success = true;
            state
                .last_cycle
                .surface_counts
                .insert("review_comments".to_string(), comments.len());
            state
                .last_successful_fetch
                .insert("review_comments".to_string(), collected_at.to_string());
            for comment in comments {
                let previous = state.last_seen.review_comments.get(&comment.id);
                let body_hash = comment.body.as_ref().map(|body| stable_body_hash(body));
                let is_new = previous.is_none();
                let is_edited = previous
                    .map(|marker| {
                        marker.updated_at != comment.updated_at || marker.body_hash != body_hash
                    })
                    .unwrap_or(false);
                state.last_seen.review_comments.insert(
                    comment.id.clone(),
                    Marker {
                        id: comment.id.clone(),
                        updated_at: comment.updated_at.clone(),
                        author: comment.author.clone(),
                        body_hash,
                        url: comment.url.clone(),
                    },
                );
                if (is_new || is_edited)
                    && !is_automation_chatter(comment.author.as_deref(), comment.body.as_deref())
                {
                    pending_actionable.push(ActionableItem {
                        id: comment.id,
                        surface: "review_comments".to_string(),
                        summary: comment
                            .body
                            .unwrap_or_else(|| "New review comment".to_string()),
                        url: comment.url,
                        path: comment.path,
                        status: Some(if is_edited { "edited" } else { "new" }.to_string()),
                        reason: Some(
                            if is_edited {
                                "edited_review_comment"
                            } else {
                                "new_review_comment"
                            }
                            .to_string(),
                        ),
                    });
                }
            }
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
    }

    match collection.issue_comments {
        Ok(comments) => {
            any_surface_success = true;
            state
                .last_cycle
                .surface_counts
                .insert("issue_comments".to_string(), comments.len());
            state
                .last_successful_fetch
                .insert("issue_comments".to_string(), collected_at.to_string());
            for comment in comments {
                let previous = state.last_seen.issue_comments.get(&comment.id);
                let body_hash = comment.body.as_ref().map(|body| stable_body_hash(body));
                let is_new = previous.is_none();
                let is_edited = previous
                    .map(|marker| {
                        marker.updated_at != comment.updated_at || marker.body_hash != body_hash
                    })
                    .unwrap_or(false);
                state.last_seen.issue_comments.insert(
                    comment.id.clone(),
                    Marker {
                        id: comment.id.clone(),
                        updated_at: comment.updated_at.clone(),
                        author: comment.author.clone(),
                        body_hash,
                        url: comment.url.clone(),
                    },
                );
                if (is_new || is_edited)
                    && !is_automation_chatter(comment.author.as_deref(), comment.body.as_deref())
                {
                    pending_actionable.push(ActionableItem {
                        id: comment.id,
                        surface: "issue_comments".to_string(),
                        summary: comment
                            .body
                            .unwrap_or_else(|| "New issue comment".to_string()),
                        url: comment.url,
                        path: None,
                        status: Some(if is_edited { "edited" } else { "new" }.to_string()),
                        reason: Some(
                            if is_edited {
                                "edited_issue_comment"
                            } else {
                                "new_issue_comment"
                            }
                            .to_string(),
                        ),
                    });
                }
            }
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
    }

    match collection.review_threads {
        Ok(threads) => {
            any_surface_success = true;
            state
                .last_cycle
                .surface_counts
                .insert("review_threads".to_string(), threads.len());
            state
                .last_successful_fetch
                .insert("review_threads".to_string(), collected_at.to_string());
            state.baseline.unresolved_thread_ids = threads
                .iter()
                .filter(|thread| !thread.is_resolved && !thread.is_outdated)
                .map(|thread| thread.id.clone())
                .collect();
            for thread in threads {
                let previous = state.last_seen.review_threads.get(&thread.id);
                let body_hash = thread.body.as_ref().map(|body| stable_body_hash(body));
                let has_new_reply = previous
                    .and_then(|marker| marker.body_hash.as_ref())
                    .zip(body_hash.as_ref())
                    .map(|(old, new)| old != new)
                    .unwrap_or(false);
                let is_known_unchanged = previous
                    .map(|marker| {
                        marker.updated_at == thread.updated_at
                            && marker.resolved == thread.is_resolved
                            && marker.outdated == thread.is_outdated
                            && marker.body_hash == body_hash
                    })
                    .unwrap_or(false);
                state.last_seen.review_threads.insert(
                    thread.id.clone(),
                    jcode_pr_watch_core::ReviewThreadMarker {
                        id: thread.id.clone(),
                        updated_at: thread.updated_at.clone(),
                        resolved: thread.is_resolved,
                        outdated: thread.is_outdated,
                        body_hash: body_hash.clone(),
                        url: thread.url.clone(),
                    },
                );
                if !thread.is_resolved && !thread.is_outdated && !is_known_unchanged {
                    pending_actionable.push(ActionableItem {
                        id: thread.id,
                        surface: "review_threads".to_string(),
                        summary: thread
                            .body
                            .unwrap_or_else(|| "Unresolved review thread".to_string()),
                        url: thread.url,
                        path: thread.path,
                        status: Some(
                            if has_new_reply {
                                "new_reply"
                            } else {
                                "unresolved"
                            }
                            .to_string(),
                        ),
                        reason: Some(
                            if has_new_reply {
                                "changed_unresolved_thread"
                            } else {
                                "new_unresolved_thread"
                            }
                            .to_string(),
                        ),
                    });
                }
            }
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
    }

    match collection.reviews {
        Ok(reviews) => {
            any_surface_success = true;
            state
                .last_cycle
                .surface_counts
                .insert("reviews".to_string(), reviews.len());
            state
                .last_successful_fetch
                .insert("reviews".to_string(), collected_at.to_string());
            for review in reviews {
                let is_new = !state.last_seen.reviews.contains_key(&review.id);
                state.last_seen.reviews.insert(
                    review.id.clone(),
                    Marker {
                        id: review.id.clone(),
                        updated_at: review.submitted_at.clone(),
                        author: review.author.clone(),
                        body_hash: review.body.as_ref().map(|body| stable_body_hash(body)),
                        url: None,
                    },
                );
                if is_new && review.state.as_deref() == Some("CHANGES_REQUESTED") {
                    pending_actionable.push(ActionableItem {
                        id: review.id,
                        surface: "reviews".to_string(),
                        summary: review
                            .body
                            .unwrap_or_else(|| "Review requested changes".to_string()),
                        url: None,
                        path: None,
                        status: review.state,
                        reason: Some("review_changes_requested".to_string()),
                    });
                }
            }
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
    }

    if review_threads_fetch_succeeded_at(state, collected_at) {
        requeue_failed_resolution_threads(state, &mut pending_actionable);
    }

    if partial_failure && !any_surface_success {
        pending_actionable = state.pending_actionable.clone();
        pending_check_count = state.last_cycle.pending_check_count;
        failed_check_count = state.last_cycle.failed_check_count;
    }

    let outcome = CycleOutcome {
        pending_actionable,
        pending_check_count,
        failed_check_count,
        partial_failure,
    };
    state.apply_cycle_outcome(outcome.clone());
    state.push_event(WatchEvent {
        at: collected_at.to_string(),
        kind: "poll_completed".to_string(),
        data: json!({
            "status": format!("{:?}", state.last_cycle.status),
            "actionable_count": state.pending_actionable.len(),
            "pending_check_count": state.last_cycle.pending_check_count,
            "failed_check_count": state.last_cycle.failed_check_count,
            "partial_failure": partial_failure,
        }),
    });
    outcome
}

fn surface_error_event(at: &str, err: SurfaceError) -> WatchEvent {
    WatchEvent {
        at: at.to_string(),
        kind: "surface_error".to_string(),
        data: json!({"surface": err.surface, "message": err.message, "transient": err.transient}),
    }
}

fn failed_surface_names(state: &PrWatchState) -> Vec<String> {
    state
        .events
        .iter()
        .rev()
        .take(10)
        .filter_map(|event| {
            if event.kind == "surface_error" {
                event
                    .data
                    .get("surface")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            } else {
                None
            }
        })
        .collect()
}

fn is_pending_check(check: &CheckRunState) -> bool {
    let status = check
        .status
        .as_deref()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let conclusion = check
        .conclusion
        .as_deref()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(
        status.as_str(),
        "IN_PROGRESS" | "QUEUED" | "PENDING" | "WAITING" | "REQUESTED"
    ) || (conclusion.is_empty()
        && !matches!(
            status.as_str(),
            "COMPLETED" | "SUCCESS" | "FAILURE" | "ERROR" | "CANCELLED" | "SKIPPED"
        ))
}

fn is_failed_check(check: &CheckRunState) -> bool {
    let conclusion = check
        .conclusion
        .as_deref()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let status = check
        .status
        .as_deref()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(
        conclusion.as_str(),
        "FAILURE" | "FAIL" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED" | "CANCEL"
    ) || matches!(
        status.as_str(),
        "FAILURE" | "FAIL" | "ERROR" | "FAILED" | "CANCELLED" | "CANCEL"
    )
}

fn is_automation_chatter(author: Option<&str>, body: Option<&str>) -> bool {
    let author = author.unwrap_or_default().to_ascii_lowercase();
    let body = body.unwrap_or_default().to_ascii_lowercase();
    if body_contains_review_signal(&body) {
        return false;
    }
    body.starts_with("fix-summary:")
        || body.contains("triggered the review bot")
        || body.contains("automation progress")
        || body.contains("<!-- jcode-pr-watch-ignore -->")
        || (author == "shopify"
            && body.contains("oxygen deployed a preview")
            && body.contains("deployment details"))
        || ((author.contains("vercel") || body.contains("vercel"))
            && (body.contains("deployment") || body.contains("preview"))
            && (body.contains("ready")
                || body.contains("building")
                || body.contains("completed")
                || body.contains("deployed")
                || body.contains("visit preview")
                || body.contains("queued")))
        || ((author.contains("netlify") || body.contains("netlify"))
            && (body.contains("deploy preview") || body.contains("deployment"))
            && (body.contains("ready")
                || body.contains("building")
                || body.contains("published")
                || body.contains("deploying")))
        || ((author.contains("github-actions") || author.ends_with("[bot]"))
            && (body.contains("workflow run")
                || body.contains("check run")
                || body.contains("deployment preview"))
            && (body.contains("started")
                || body.contains("queued")
                || body.contains("in progress")
                || body.contains("completed successfully")))
        || (body.contains("jules, reporting for duty")
            || body.contains("reporting for duty! i'm here to lend a hand"))
}

fn body_contains_review_signal(body: &str) -> bool {
    body.contains("[high]")
        || body.contains("[medium]")
        || body.contains("[low]")
        || body.contains("requested changes")
        || body.contains("please fix")
        || body.contains("please ")
        || body.contains("must fix")
        || body.contains("needs changes")
        || body.contains("action required")
        || body.contains("review required")
        || body.contains("security")
        || body.contains("failing")
        || body.contains("failed")
        || body.contains("failure")
        || body.contains("error:")
        || body.contains(" error")
}

fn stable_body_hash(body: &str) -> String {
    let mut hasher = 0xcbf29ce484222325u64;
    for byte in body.as_bytes() {
        hasher ^= u64::from(*byte);
        hasher = hasher.wrapping_mul(0x100000001b3);
    }
    format!("hash:{:016x}", hasher)
}

fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn status_like(store: &Path, params: PrWatchInput) -> Result<ToolOutput> {
    let state = load_state_for_params(store, &params)?;
    let readiness = state.readiness();
    let mut text = format_status_report(&state, readiness_label(&readiness));
    text.push('\n');
    text.push_str(&schedule_queue_health_line(&state));
    Ok(ToolOutput::new(text)
        .with_title(format!(
            "{} {}",
            state.watch_id,
            readiness_label(&readiness)
        ))
        .with_metadata(json!({"watch": state, "readiness": readiness})))
}

fn webhook_status(store: &Path, params: PrWatchInput) -> Result<ToolOutput> {
    let state = load_state_for_params(store, &params)?;
    let mut text = format!(
        "PR watch webhook status: {}\nRepo: {}\nPR: #{}\nEvent source: {:?}\nWebhook enabled: {}\nLast delivery: {}\nLast delivery result: {}\nLast event: {} {}\nCollapsed events: {}\nDropped events: {}\nFallback heartbeat: {}\nWebhook URL hint: {}",
        state.watch_id,
        state.pr.repo,
        state.pr.number,
        state.webhook.mode,
        state.webhook.enabled,
        state.webhook.last_delivery_at.as_deref().unwrap_or("none"),
        state
            .webhook
            .last_delivery_status
            .as_deref()
            .unwrap_or("none"),
        state.webhook.last_event_type.as_deref().unwrap_or("none"),
        state.webhook.last_event_action.as_deref().unwrap_or(""),
        state.webhook.collapsed_event_count,
        state.webhook.dropped_event_count,
        state
            .webhook
            .fallback_heartbeat_seconds
            .map(|seconds| format!("{}s", seconds))
            .unwrap_or_else(|| "disabled".to_string()),
        state.webhook.webhook_url_hint.as_deref().unwrap_or("none"),
    );
    text.push_str("\nMutation policy: webhook deliveries are read-only wake signals and do not grant push/comment/resolve permissions.");
    match read_webhook_health()? {
        Some(health) => text.push_str(&format!(
            "\nWebhook daemon: {} pid={} alive={} bind={}:{} last_result={}",
            health.status,
            health.pid,
            process_is_alive(health.pid),
            health.bind,
            health.port,
            health.last_result.as_deref().unwrap_or("none")
        )),
        None => text.push_str("\nWebhook daemon: daemon_down (no health file)"),
    }
    Ok(ToolOutput::new(text)
        .with_title(format!("webhook status {}", state.watch_id))
        .with_metadata(json!({"watch": state, "webhook_status": "reported"})))
}

async fn webhook_doctor(root: &Path, store: &Path, params: PrWatchInput) -> Result<ToolOutput> {
    let state = load_state_for_params(store, &params)?;
    let path = state_path(store, &state.watch_id);
    let mut checks = Vec::new();
    checks.push(("state_path_readable", path.is_file()));
    checks.push((
        "root_dir_matches",
        state
            .root_dir
            .as_deref()
            .map(|recorded| recorded == root.display().to_string())
            .unwrap_or(false),
    ));
    checks.push(("webhook_enabled", state.webhook.enabled));
    checks.push((
        "secret_env_present",
        std::env::var("GITHUB_WEBHOOK_SECRET")
            .map(|value| !value.is_empty())
            .unwrap_or(false),
    ));
    checks.push((
        "normal_monitor_suppressed",
        !matches!(state.webhook.mode, PrWatchEventMode::Webhook)
            || state.polling.last_schedule_kind.as_deref() != Some("monitor"),
    ));
    let health = read_webhook_health()?;
    let daemon_alive = health
        .as_ref()
        .map(|h| process_is_alive(h.pid))
        .unwrap_or(false);
    checks.push(("daemon_pid_alive", daemon_alive));
    let hook_output = Command::new("gh")
        .args(["api", &format!("repos/{}/hooks", state.pr.repo)])
        .output()
        .await;
    let mut hook_signal = "github_hook_unknown".to_string();
    let mut hook_ok = false;
    if let Ok(output) = hook_output {
        if output.status.success() {
            let hooks: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| json!([]));
            let failing = hooks.as_array().into_iter().flatten().find_map(|hook| {
                let code = hook
                    .pointer("/last_response/code")
                    .and_then(Value::as_i64)?;
                (!(200..300).contains(&code)).then_some(code)
            });
            let required_events = [
                "pull_request",
                "pull_request_review",
                "pull_request_review_comment",
                "issue_comment",
                "check_run",
                "check_suite",
                "status",
            ];
            let missing_events = hooks.as_array().into_iter().flatten().all(|hook| {
                let Some(events) = hook.get("events").and_then(Value::as_array) else {
                    return true;
                };
                !required_events
                    .iter()
                    .all(|required| events.iter().any(|event| event.as_str() == Some(*required)))
            });
            if let Some(code) = failing {
                hook_signal = format!("github_hook_failing last_response.code={code}");
            } else if missing_events {
                hook_signal = "github_hook_failing missing_required_events".to_string();
            } else {
                hook_ok = true;
                hook_signal = "github_hooks_reachable".to_string();
            }
        } else {
            hook_signal = "github_hook_failing_or_auth_failing".to_string();
        }
    }
    checks.push(("github_hook_last_response_ok", hook_ok));
    let tunnel_signal = match state.webhook.webhook_url_hint.as_deref() {
        Some(url) if daemon_alive && hook_signal.contains("github_hook_failing") => {
            format!("tunnel_down_or_hook_misrouted public_url={url}")
        }
        Some(url) => format!("public_url_configured {url}"),
        None => "tunnel_unknown no webhook_url_hint configured".to_string(),
    };
    let mut text = format!(
        "PR watch webhook doctor: {}\nRepo: {}\nPR: #{}\nState path: {}\nCurrent root: {}\n",
        state.watch_id,
        state.pr.repo,
        state.pr.number,
        path.display(),
        root.display(),
    );
    for (name, ok) in &checks {
        text.push_str(&format!(
            "- {}: {}\n",
            name,
            if *ok { "ok" } else { "problem" }
        ));
    }
    text.push_str(&format!(
        "- daemon_signal: {}\n- hook_signal: {}\n- tunnel_signal: {}\nSignals: daemon_down means missing/stale health or dead pid; tunnel_down is suspected when daemon is alive but GitHub hook last_response is non-2xx/404; github_hook_failing reports hook API failures, missing required events, or non-2xx last responses.\n",
        if daemon_alive { "daemon_alive" } else { "daemon_down" },
        hook_signal,
        tunnel_signal
    ));
    let ok = checks.iter().all(|(_, ok)| *ok);
    Ok(ToolOutput::new(text)
        .with_title(format!(
            "webhook doctor {}",
            if ok { "ok" } else { "problem" }
        ))
        .with_metadata(json!({"watch": state, "doctor_ok": ok})))
}

fn readiness_report(store: &Path, params: PrWatchInput) -> Result<ToolOutput> {
    let state = load_state_for_params(store, &params)?;
    let readiness = state.readiness();
    let mut text = format_status_report(&state, readiness_label(&readiness));
    text.push('\n');
    text.push_str(&schedule_queue_health_line(&state));
    text.push_str("\n\nReadiness decision:\n");
    text.push_str(&format!("- {}\n", readiness_label(&readiness)));
    for reason in readiness_reasons(&state) {
        text.push_str(&format!("- {}\n", reason));
    }
    Ok(ToolOutput::new(text)
        .with_title(format!("readiness {}", readiness_label(&readiness)))
        .with_metadata(json!({"watch": state, "readiness": readiness})))
}

fn handoff_report(store: &Path, params: PrWatchInput) -> Result<ToolOutput> {
    let state = load_state_for_params(store, &params)?;
    let readiness = state.readiness();
    let mut text = String::new();
    text.push_str(&format!("# PR watch handoff: {}\n\n", state.watch_id));
    text.push_str(&format!("- PR: {}/#{}\n", state.pr.repo, state.pr.number));
    if let Some(url) = &state.pr.url {
        text.push_str(&format!("- URL: {}\n", url));
    }
    text.push_str(&format!("- Readiness: {}\n", readiness_label(&readiness)));
    text.push_str(&format!(
        "- Current status: {:?}\n",
        state.last_cycle.status
    ));
    text.push_str(&format!(
        "- Quiet cycles: {}/{}\n",
        state.polling.quiet_cycles, state.polling.required_quiet_cycles
    ));
    text.push_str(&format!("- {}\n", schedule_status_line(&state)));
    let grants = active_grant_lines(&state);
    if !grants.is_empty() {
        text.push_str("\n## Authorization grants\n");
        for grant in grants {
            text.push_str(&format!("- {grant}\n"));
        }
        text.push_str("- Watch invariant: pr_watch poll/status/monitor/scheduled follow-ups remain read-only even with active grants.\n");
    }
    let grant_events = recent_grant_event_lines(&state);
    if !grant_events.is_empty() {
        text.push_str("\n## Recent grant lifecycle events\n");
        for event in grant_events {
            text.push_str(&format!("- {event}\n"));
        }
    }
    text.push_str("\n## Evidence\n");
    for line in evidence_lines(&state) {
        text.push_str(&format!("- {}\n", line));
    }
    text.push_str("\n## Pending actionable items\n");
    if state.pending_actionable.is_empty() {
        text.push_str("- None recorded.\n");
    } else {
        for item in &state.pending_actionable {
            text.push_str(&format!(
                "- [{}] {}{}\n",
                item.surface,
                item.summary,
                item.url
                    .as_ref()
                    .map(|u| format!(" ({u})"))
                    .unwrap_or_default()
            ));
        }
    }
    text.push_str("\n## Human next step\n");
    text.push_str(&human_next_step(&state));
    text.push('\n');
    text.push_str("\nNo mutation was performed by this report. Do not merge unless repository policy and a human maintainer approve it.\n");
    Ok(ToolOutput::new(text)
        .with_title(format!("handoff {}", readiness_label(&readiness)))
        .with_metadata(json!({"watch": state, "readiness": readiness})))
}

fn format_status_report(state: &PrWatchState, readiness: &str) -> String {
    let mut text = format!(
        "PR watch: {}\nRepo: {}\nPR: #{}\nState: {:?}\nReadiness: {}\nQuiet cycles: {}/{}\nActionable: {}\nPending checks: {}\nFailed checks: {}\nUnresolved threads: {}\nNext poll: {}\n{}\nHandoff: status={:?} schedule_id={} target={} fingerprint={} error={}\nPolicy: local_fix={}, commit={}, push={}, comment={}, resolve_threads={} (legacy display only; remote mutation requires a separate grant-consuming remediation workflow)",
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
        state.baseline.unresolved_thread_ids.len(),
        state
            .polling
            .next_poll_at
            .as_deref()
            .unwrap_or("not scheduled"),
        schedule_status_line(state),
        state.action_required_handoff.status,
        state
            .action_required_handoff
            .schedule_id
            .as_deref()
            .unwrap_or("none"),
        state
            .action_required_handoff
            .target
            .as_deref()
            .unwrap_or("none"),
        state
            .action_required_handoff
            .fingerprint
            .as_deref()
            .unwrap_or("none"),
        state
            .action_required_handoff
            .error
            .as_deref()
            .unwrap_or("none"),
        state.policy.local_fix,
        state.policy.commit,
        state.policy.push,
        state.policy.comment,
        state.policy.resolve_threads,
    );
    if !state.last_successful_fetch.is_empty() {
        text.push_str("\nLast successful fetch:");
        for (surface, at) in &state.last_successful_fetch {
            text.push_str(&format!("\n- {}: {}", surface, at));
        }
    }
    let grants = active_grant_lines(state);
    if !grants.is_empty() {
        text.push_str("\nAuthorization grants:");
        for grant in grants {
            text.push_str(&format!("\n- {grant}"));
        }
        text.push_str(
            "\nGrant note: pr_watch watch actions remain read-only even with active grants.",
        );
    }
    if state.resolution_requires_post_poll || !state.last_resolution_attempts.is_empty() {
        text.push_str(&format!(
            "\nResolution: attempts={} post_poll_required={} error={}",
            state.last_resolution_attempts.len(),
            state.resolution_requires_post_poll,
            state.last_resolution_error.as_deref().unwrap_or("none")
        ));
    }
    text.push_str(&format!(
        "\nWebhook: mode={:?} enabled={} last_delivery={} status={} heartbeat={} collapsed={} dropped={}",
        state.webhook.mode,
        state.webhook.enabled,
        state.webhook.last_delivery_at.as_deref().unwrap_or("none"),
        state.webhook.last_delivery_status.as_deref().unwrap_or("none"),
        state
            .webhook
            .fallback_heartbeat_seconds
            .map(|seconds| format!("{}s", seconds))
            .unwrap_or_else(|| "disabled".to_string()),
        state.webhook.collapsed_event_count,
        state.webhook.dropped_event_count,
    ));
    text
}

fn readiness_label(readiness: &jcode_pr_watch_core::Readiness) -> &'static str {
    match readiness {
        jcode_pr_watch_core::Readiness::NotReadyActionRequired => "not_ready_action_required",
        jcode_pr_watch_core::Readiness::NotReadyChecksPending => "not_ready_checks_pending",
        jcode_pr_watch_core::Readiness::NotReadyChecksFailed => "not_ready_checks_failed",
        jcode_pr_watch_core::Readiness::NotReadyValidationStale => "not_ready_validation_stale",
        jcode_pr_watch_core::Readiness::ReadyForHumanReview => "ready_for_human_review",
        jcode_pr_watch_core::Readiness::ReadyForHumanPush => "ready_for_human_push",
        jcode_pr_watch_core::Readiness::ReadyForHumanMerge => "ready_for_human_merge",
        jcode_pr_watch_core::Readiness::BlockedByPolicy => "blocked_by_policy",
        jcode_pr_watch_core::Readiness::BlockedByClosedPr => "blocked_by_closed_pr",
    }
}

fn readiness_reasons(state: &PrWatchState) -> Vec<String> {
    let mut reasons = Vec::new();
    if state.pr.state.as_deref() != Some("OPEN") && state.pr.state.is_some() {
        reasons.push("PR is not open.".to_string());
    }
    if !state.pending_actionable.is_empty() {
        reasons.push(format!(
            "{} actionable item(s) need attention.",
            state.pending_actionable.len()
        ));
    }
    if state.last_cycle.failed_check_count > 0 {
        reasons.push(format!(
            "{} check(s) failed.",
            state.last_cycle.failed_check_count
        ));
    }
    if state.last_cycle.pending_check_count > 0 {
        reasons.push(format!(
            "{} check(s) are pending.",
            state.last_cycle.pending_check_count
        ));
    }
    if state.polling.quiet_cycles < state.polling.required_quiet_cycles {
        reasons.push(format!(
            "Quiet cycle requirement not yet met: {}/{}.",
            state.polling.quiet_cycles, state.polling.required_quiet_cycles
        ));
    }
    if state.last_successful_fetch.is_empty() {
        reasons.push("No successful fetch evidence recorded yet.".to_string());
    }
    if reasons.is_empty() {
        reasons.push("Required quiet cycles are satisfied and no actionable items or blocking checks are recorded.".to_string());
    }
    reasons
}

fn evidence_lines(state: &PrWatchState) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!(
        "Last cycle completed: {}",
        state
            .last_cycle
            .completed_at
            .as_deref()
            .unwrap_or("unknown")
    ));
    lines.push(format!(
        "Surfaces checked: {}",
        if state.last_cycle.surfaces_checked.is_empty() {
            "none".to_string()
        } else {
            state.last_cycle.surfaces_checked.join(", ")
        }
    ));
    lines.push(format!(
        "Checks: {} pending, {} failed",
        state.last_cycle.pending_check_count, state.last_cycle.failed_check_count
    ));
    lines.push(format!(
        "Actionable items: {}",
        state.pending_actionable.len()
    ));
    lines.push(format!(
        "Unresolved review threads at baseline/latest poll: {}",
        state.baseline.unresolved_thread_ids.len()
    ));
    if let Some(head) = &state.pr.head_sha {
        lines.push(format!("Head SHA: {}", head));
    }
    if let Some(next) = &state.polling.next_poll_at {
        lines.push(format!("Next scheduled poll: {}", next));
    }
    if let Some(seconds) = schedule_overdue_by_seconds(state) {
        lines.push(format!(
            "Scheduled poll overdue by {}",
            human_duration(seconds)
        ));
    }
    lines
}

fn human_next_step(state: &PrWatchState) -> String {
    let readiness = state.readiness();
    match readiness {
        jcode_pr_watch_core::Readiness::ReadyForHumanMerge => format!(
            "- Human maintainer may review repository policy and choose an approved merge strategy, for example: `gh pr merge {} --repo {} [--squash|--merge|--rebase]`",
            state.pr.number, state.pr.repo
        ),
        jcode_pr_watch_core::Readiness::NotReadyActionRequired => "- Address actionable review feedback, validate locally, then run `pr_watch poll_now` again.".to_string(),
        jcode_pr_watch_core::Readiness::NotReadyChecksPending => "- Wait for pending checks, then run `pr_watch poll_now` again.".to_string(),
        jcode_pr_watch_core::Readiness::NotReadyChecksFailed => "- Investigate failing checks, fix locally if appropriate, then run `pr_watch poll_now` again.".to_string(),
        jcode_pr_watch_core::Readiness::BlockedByClosedPr => "- PR is closed; stop the watcher or reopen the PR before continuing.".to_string(),
        _ => "- Continue monitoring until quiet-cycle and validation requirements are satisfied.".to_string(),
    }
}

fn stop_watch(store: &Path, params: PrWatchInput) -> Result<ToolOutput> {
    let mut state = load_state_for_params(store, &params)?;
    let would_write = !params.dry_run.unwrap_or(false);
    let _lock = if would_write {
        match acquire_watch_lock(store, &state.watch_id)? {
            Some(lock) => Some(lock),
            None => return Ok(watch_locked_output(store, &state, "stop")),
        }
    } else {
        None
    };
    state.terminal = true;
    state.stop_reason = Some("stopped_by_pr_watch_tool".to_string());
    state.polling.next_poll_at = None;
    state.last_cycle.status = jcode_pr_watch_core::CycleStatus::Stopped;
    let path = state_path(store, &state.watch_id);
    if would_write {
        write_state_atomic(&path, &state)?;
        remove_webhook_index_entry(&state.watch_id)?;
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
    use jcode_pr_watch_core::ReviewThreadMarker;

    struct EnvVarGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &Path) -> Self {
            let prev = std::env::var_os(key);
            crate::env::set_var(key, value);
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(prev) = self.prev.take() {
                crate::env::set_var(self.key, prev);
            } else {
                crate::env::remove_var(self.key);
            }
        }
    }

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
        assert!(actions.iter().any(|value| value == "monitor"));
        assert!(actions.iter().any(|value| value == "ack_baseline"));
        assert!(
            schema
                .pointer("/properties/quiet_cycles_required")
                .is_some()
        );
        assert!(schema.pointer("/properties/max_runtime_seconds").is_some());
        assert!(actions.iter().any(|value| value == "authorize"));
        assert!(actions.iter().any(|value| value == "revoke"));
        assert!(actions.iter().any(|value| value == "reschedule"));
        assert!(actions.iter().any(|value| value == "resolve_addressed"));
        assert!(actions.iter().any(|value| value == "webhook_status"));
        assert!(actions.iter().any(|value| value == "webhook_doctor"));
        assert!(actions.iter().any(|value| value == "webhook_heartbeat"));
        assert!(!actions.iter().any(|value| value == "merge"));
        assert!(schema.pointer("/properties/scopes").is_some());
        assert!(schema.pointer("/properties/thread_ids").is_some());
        assert!(schema.pointer("/properties/event_mode").is_some());
        assert!(
            schema
                .pointer("/properties/fallback_heartbeat_seconds")
                .is_some()
        );
        let validation = schema
            .pointer("/properties/validation")
            .expect("validation schema should be advertised");
        assert_eq!(validation["type"], json!("array"));
        assert_eq!(validation["items"]["type"], json!("object"));
        assert_eq!(
            validation["items"]["properties"]["command"]["type"],
            json!("string")
        );
    }

    #[test]
    fn webhook_content_type_accepts_json_charset_only() {
        assert!(content_type_is_accepted("application/json"));
        assert!(content_type_is_accepted("application/json; charset=utf-8"));
        assert!(!content_type_is_accepted("text/plain"));
        assert!(!content_type_is_accepted("application/json; boundary=x"));
    }

    #[test]
    fn webhook_signature_verification_requires_valid_hmac() {
        let body = br#"{"action":"opened","repository":{"full_name":"owner/repo"},"pull_request":{"number":7}}"#;
        let digest = hmac_sha256(b"secret", body);
        let signature = format!("sha256={}", hex::encode(digest));
        let delivery = verified_github_delivery_from_parts(
            "secret",
            "application/json",
            "pull_request",
            "delivery-1",
            &signature,
            body,
        )
        .expect("valid delivery");
        assert_eq!(delivery.repo.as_deref(), Some("owner/repo"));
        assert_eq!(delivery.pr, Some(7));
        assert_eq!(delivery.action.as_deref(), Some("opened"));
        assert!(
            verified_github_delivery_from_parts(
                "wrong",
                "application/json",
                "pull_request",
                "delivery-1",
                &signature,
                body,
            )
            .is_err()
        );
    }

    #[test]
    fn parse_resolve_review_thread_output_classifies_outcomes() {
        let resolved =
            r#"{"data":{"resolveReviewThread":{"thread":{"id":"T","isResolved":true}}}}"#;
        assert_eq!(
            parse_resolve_review_thread_output(resolved).expect("resolved json"),
            ResolveReviewThreadOutcome::Resolved
        );

        let not_resolved =
            r#"{"data":{"resolveReviewThread":{"thread":{"id":"T","isResolved":false}}}}"#;
        assert_eq!(
            parse_resolve_review_thread_output(not_resolved).expect("not resolved json"),
            ResolveReviewThreadOutcome::NotResolved
        );

        let already = r#"{"errors":[{"message":"Review thread already resolved"}]}"#;
        assert_eq!(
            parse_resolve_review_thread_output(already).expect("already resolved json"),
            ResolveReviewThreadOutcome::AlreadyResolved
        );

        let malformed = r#"{"data":{"resolveReviewThread":{"thread":{"id":"T"}}}}"#;
        assert!(matches!(
            parse_resolve_review_thread_output(malformed).expect("malformed classified"),
            ResolveReviewThreadOutcome::MalformedResponse(_)
        ));
    }

    fn monitor_params(max_runtime_seconds: Option<u64>) -> PrWatchInput {
        PrWatchInput {
            action: PrWatchAction::Monitor,
            repo: Some("owner/repo".to_string()),
            pr: Some(7),
            watch_id: None,
            dry_run: None,
            schedule_next: false,
            poll_interval_seconds: None,
            quiet_cycles_required: None,
            max_runtime_seconds,
            target: None,
            scopes: None,
            reason: None,
            expires_in_minutes: None,
            single_use: None,
            grant_id: None,
            thread_ids: Vec::new(),
            head_sha: None,
            commit_sha: None,
            validation: Vec::new(),
            expected_fingerprint: None,
            expected_cycle_number: None,
            no_code_resolution: false,
            event_mode: None,
            fallback_heartbeat_seconds: None,
            webhook_url_hint: None,
        }
    }

    #[test]
    fn parse_write_scopes_rejects_merge_scope() {
        let scopes = vec!["local_fix".to_string(), "push".to_string()];
        let parsed = parse_write_scopes(Some(&scopes)).expect("valid scopes");
        assert!(parsed.contains(&WriteScope::LocalFix));
        assert!(parsed.contains(&WriteScope::Push));

        let invalid = vec!["merge".to_string()];
        assert!(parse_write_scopes(Some(&invalid)).is_err());
    }

    #[test]
    fn schedule_overdue_detects_nonterminal_due_poll() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 7,
        });
        state.polling.next_poll_at = Some(
            (Utc::now() - Duration::minutes(7))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string(),
        );
        assert!(schedule_overdue_by_seconds(&state).unwrap() >= 60);
        assert!(schedule_status_line(&state).contains("overdue"));
        state.terminal = true;
        assert!(schedule_overdue_by_seconds(&state).is_none());
    }

    #[test]
    fn monitor_defaults_are_bounded() {
        assert_eq!(
            monitor_max_runtime_seconds(&monitor_params(None)),
            DEFAULT_MONITOR_MAX_RUNTIME_SECONDS
        );
        assert!(DEFAULT_MONITOR_MAX_RUNTIME_SECONDS <= 540);
        assert_eq!(
            monitor_max_runtime_seconds(&monitor_params(Some(5_000))),
            MAX_MONITOR_MAX_RUNTIME_SECONDS
        );
        assert_eq!(monitor_max_runtime_seconds(&monitor_params(Some(0))), 1);
    }

    fn scheduled_item(id: &str, description: &str, relevant_files: Vec<String>) -> ScheduledItem {
        ScheduledItem {
            id: id.to_string(),
            scheduled_for: Utc::now() + Duration::minutes(5),
            context: description.to_string(),
            priority: Priority::Normal,
            target: ScheduleTarget::Spawn {
                parent_session_id: "parent".to_string(),
            },
            created_by_session: "parent".to_string(),
            created_at: Utc::now(),
            working_dir: None,
            task_description: Some(description.to_string()),
            relevant_files,
            git_branch: None,
            additional_context: Some("Scheduled by pr_watch schedule_next".to_string()),
            schedule_key: None,
            schedule_kind: None,
            schedule_payload: None,
        }
    }

    #[test]
    fn scheduled_watch_dedupe_finds_existing_poll_for_same_watch() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".to_string(),
            number: 7,
        });
        state
            .last_successful_fetch
            .insert("comments".to_string(), "2026-05-19T00:00:00Z".to_string());
        let description = scheduled_poll_prompt(&state);
        let item = scheduled_item(
            "sched_existing",
            &description,
            vec![format!(
                ".jcode/pr-feedback-watch/{}-state.json",
                state.watch_id
            )],
        );

        let items = vec![item];
        let found = find_existing_scheduled_watch_item(&items, &state, "poll_now")
            .expect("existing poll schedule should be found");
        assert_eq!(found.id, "sched_existing");
    }

    #[test]
    fn scheduled_watch_dedupe_separates_actions_and_watch_ids() {
        let state = PrWatchState::new(PrTarget {
            repo: "owner/repo".to_string(),
            number: 7,
        });
        let other = PrWatchState::new(PrTarget {
            repo: "owner/repo".to_string(),
            number: 8,
        });
        let monitor_item = scheduled_item(
            "sched_monitor",
            &scheduled_monitor_prompt(&state, 60),
            vec![],
        );
        let other_poll_item = scheduled_item("sched_other", &scheduled_poll_prompt(&other), vec![]);
        let items = vec![monitor_item, other_poll_item];

        assert!(find_existing_scheduled_watch_item(&items, &state, "poll_now").is_none());
        assert_eq!(
            find_existing_scheduled_watch_item(&items, &state, "monitor")
                .map(|item| item.id.as_str()),
            Some("sched_monitor")
        );
    }

    #[test]
    fn scheduled_watch_dedupe_uses_structured_payload() {
        let state = PrWatchState::new(PrTarget {
            repo: "owner/repo".to_string(),
            number: 7,
        });
        let payload = PrWatchSchedulePayload::for_action(&state, "monitor", 540);
        payload
            .validate_against_state(&state)
            .expect("valid payload");
        let mut item = scheduled_item("sched_structured", "legacy text without markers", vec![]);
        item.schedule_key = Some(schedule_key_for_watch(&state.watch_id));
        item.schedule_kind = Some("pr_watch.monitor".to_string());
        item.schedule_payload = Some(serde_json::to_value(payload).expect("payload json"));

        let items = vec![item];
        let found = find_existing_scheduled_watch_item(&items, &state, "monitor")
            .expect("structured monitor schedule should be found");
        assert_eq!(found.id, "sched_structured");
    }

    #[test]
    fn scheduled_payload_rejects_write_like_actions() {
        let state = PrWatchState::new(PrTarget {
            repo: "owner/repo".to_string(),
            number: 7,
        });
        let mut payload = PrWatchSchedulePayload::for_action(&state, "monitor", 540);
        payload.action = "push".to_string();
        assert!(payload.validate_against_state(&state).is_err());

        let mut item = scheduled_item("sched_bad", "bad payload", vec![]);
        item.schedule_payload = Some(serde_json::to_value(payload).expect("payload json"));
        assert!(PrWatchSchedulePayload::from_scheduled_item(&item).is_err());
    }

    #[test]
    fn monitor_lock_prevents_concurrent_runs() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let first = acquire_watch_lock(temp.path(), "owner-repo-pr-7")
            .expect("first lock")
            .expect("lock acquired");
        let second = acquire_watch_lock(temp.path(), "owner-repo-pr-7").expect("second lock");
        assert!(second.is_none());
        drop(first);
        let third = acquire_watch_lock(temp.path(), "owner-repo-pr-7")
            .expect("third lock")
            .expect("lock reacquired");
        drop(third);
    }

    #[test]
    fn handoff_lock_prevents_concurrent_queue_phase() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let first = acquire_handoff_lock(temp.path(), "owner-repo-pr-7")
            .expect("first lock")
            .expect("handoff lock acquired");
        let second = acquire_handoff_lock(temp.path(), "owner-repo-pr-7").expect("second lock");
        assert!(second.is_none());
        drop(first);
        let third = acquire_handoff_lock(temp.path(), "owner-repo-pr-7")
            .expect("third lock")
            .expect("handoff lock reacquired");
        drop(third);
    }

    fn actionable(id: &str, summary: &str) -> ActionableItem {
        ActionableItem {
            id: id.to_string(),
            surface: "review_comments".to_string(),
            summary: summary.to_string(),
            url: Some(format!("https://example.test/{id}")),
            path: Some("src/lib.rs".to_string()),
            status: Some("new".to_string()),
            reason: Some("new_review_comment".to_string()),
        }
    }

    #[test]
    fn actionable_fingerprint_is_deterministic_and_content_sensitive() {
        let a = actionable("a", "Fix this\r\nplease  ");
        let b = actionable("b", "Also fix this");
        let first = actionable_fingerprint(&[a.clone(), b.clone()]).expect("fingerprint");
        let reordered = actionable_fingerprint(&[b, a.clone()]).expect("fingerprint");
        assert_eq!(first, reordered);

        let changed = actionable_fingerprint(&[actionable("a", "Fix something else")])
            .expect("changed fingerprint");
        assert_ne!(first, changed);
    }

    #[test]
    fn handoff_payload_round_trips_and_uses_distinct_kind() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 7,
        });
        state.pending_actionable.push(actionable("a", "Fix this"));
        let fingerprint = actionable_fingerprint(&state.pending_actionable).expect("fingerprint");
        let payload = PrWatchHandoffPayload::new(&state, fingerprint.clone());
        let mut item = scheduled_item("sched_handoff", "handoff", vec![]);
        item.schedule_key = Some(handoff_schedule_key_for_watch(&state.watch_id));
        item.schedule_kind = Some("pr_watch.action_required_handoff".to_string());
        item.schedule_payload = Some(serde_json::to_value(&payload).expect("payload json"));

        let parsed = PrWatchHandoffPayload::from_scheduled_item(&item)
            .expect("parse payload")
            .expect("payload present");
        assert_eq!(parsed.fingerprint, fingerprint);
        assert_eq!(
            item.schedule_kind.as_deref(),
            Some("pr_watch.action_required_handoff")
        );
        assert!(PrWatchSchedulePayload::from_scheduled_item(&item).is_err());
    }

    #[test]
    fn handoff_prompt_contains_grant_gated_safety_language() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 7,
        });
        state.pending_actionable.push(actionable("a", "Fix this"));
        let fingerprint = actionable_fingerprint(&state.pending_actionable).expect("fingerprint");
        let prompt = handoff_prompt(&state, &fingerprint);
        assert!(prompt.contains("Do not call `pr_watch monitor`"));
        assert!(prompt.contains("No push without an active push grant"));
        assert!(prompt.contains("No comment without an active comment grant"));
        assert!(
            prompt.contains("No review-thread resolution without an active resolve_threads grant")
        );
        assert!(prompt.contains("Never merge"));
    }

    #[test]
    fn status_report_includes_handoff_health_fields() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 7,
        });
        state.action_required_handoff.status = ActionRequiredHandoffStatus::Queued;
        state.action_required_handoff.schedule_id = Some("sched_123".to_string());
        state.action_required_handoff.target = Some("resume:session_origin".to_string());
        state.action_required_handoff.fingerprint = Some("abc".to_string());
        let report = format_status_report(&state, "not_ready_action_required");
        assert!(report.contains("Handoff: status=Queued"));
        assert!(report.contains("schedule_id=sched_123"));
        assert!(report.contains("target=resume:session_origin"));
        assert!(report.contains("fingerprint=abc"));
    }

    #[test]
    fn action_required_handoff_schedules_once_for_direct_origin() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
        let work = temp.path().join("repo");
        fs::create_dir_all(&work).expect("work dir");
        let store = watch_dir(&work);
        let ctx = ToolContext {
            session_id: "session_origin".to_string(),
            message_id: "message".to_string(),
            tool_call_id: "tool".to_string(),
            working_dir: Some(work.clone()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: super::super::ToolExecutionMode::Direct,
        };
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 7,
        });
        state.origin_session_id = Some(ctx.session_id.clone());
        state.pending_actionable.push(actionable("a", "Fix this"));

        let first = maybe_schedule_action_required_handoff(&store, &mut state, &ctx)
            .expect("schedule handoff")
            .expect("handoff message");
        assert!(first.contains("action handoff scheduled"));
        assert_eq!(
            state.action_required_handoff.status,
            ActionRequiredHandoffStatus::Queued
        );
        let first_id = state
            .action_required_handoff
            .schedule_id
            .clone()
            .expect("schedule id");

        let second = maybe_schedule_action_required_handoff(&store, &mut state, &ctx)
            .expect("reuse handoff")
            .expect("reuse message");
        assert!(second.contains("action handoff reused"));
        assert_eq!(
            state.action_required_handoff.schedule_id.as_deref(),
            Some(first_id.as_str())
        );

        let manager = AmbientManager::new().expect("ambient manager");
        let key = handoff_schedule_key_for_watch(&state.watch_id);
        let queued = handoff_items_for_key(manager.queue().items(), &key);
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id, first_id);
    }

    #[test]
    fn watch_locked_output_uses_shared_lock_metadata() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 7,
        });

        let output = watch_locked_output(temp.path(), &state, "poll_now");
        assert!(output.output.contains("already running or locked"));
        assert!(output.output.contains("owner/repo"));
        let metadata = output.metadata.expect("metadata");
        assert_eq!(metadata.pointer("/watch_locked"), Some(&Value::Bool(true)));
        assert_eq!(
            metadata.pointer("/action"),
            Some(&Value::String("poll_now".to_string()))
        );
    }

    #[test]
    fn monitor_status_maps_actionable_and_checks() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 7,
        });
        state.pending_actionable.push(ActionableItem {
            id: "thread-1".into(),
            surface: "review_threads".into(),
            summary: "fix it".into(),
            url: None,
            path: None,
            status: Some("unresolved".into()),
            reason: Some("test".into()),
        });
        assert_eq!(
            monitor_status_for_state(&state, false),
            MonitorStatus::ActionRequired
        );
        state.pending_actionable.clear();
        state.last_cycle.failed_check_count = 1;
        assert_eq!(
            monitor_status_for_state(&state, false),
            MonitorStatus::ChecksFailed
        );
        state.last_cycle.failed_check_count = 0;
        state.last_cycle.pending_check_count = 1;
        assert_eq!(
            monitor_status_for_state(&state, false),
            MonitorStatus::ChecksPending
        );
        state.last_cycle.pending_check_count = 0;
        assert_eq!(
            monitor_status_for_state(&state, true),
            MonitorStatus::TransientFailure
        );
        state.polling.quiet_cycles = state.polling.required_quiet_cycles;
        assert_eq!(
            monitor_status_for_state(&state, false),
            MonitorStatus::QuietSatisfied
        );
    }

    #[test]
    fn monitor_should_schedule_recoverable_statuses() {
        assert!(monitor_should_schedule_followup(
            MonitorStatus::PendingNextPoll
        ));
        assert!(monitor_should_schedule_followup(
            MonitorStatus::ChecksPending
        ));
        assert!(monitor_should_schedule_followup(
            MonitorStatus::TransientFailure
        ));
        assert!(!monitor_should_schedule_followup(
            MonitorStatus::ActionRequired
        ));
        assert!(!monitor_should_schedule_followup(
            MonitorStatus::ChecksFailed
        ));
        assert!(!monitor_should_schedule_followup(
            MonitorStatus::QuietSatisfied
        ));
        assert!(!monitor_should_schedule_followup(MonitorStatus::Stopped));
    }

    #[test]
    fn monitor_stale_guard_detects_concurrent_state_changes() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 7,
        });
        state.updated_at = Some("2026-05-14T11:00:00Z".into());
        state.polling.cycle_number = 2;

        assert!(!watch_state_changed_since_load(
            &state,
            true,
            &state.updated_at,
            state.polling.cycle_number,
        ));

        let mut updated_at_changed = state.clone();
        updated_at_changed.updated_at = Some("2026-05-14T11:01:00Z".into());
        assert!(watch_state_changed_since_load(
            &updated_at_changed,
            true,
            &state.updated_at,
            state.polling.cycle_number,
        ));

        let mut cycle_changed = state.clone();
        cycle_changed.polling.cycle_number += 1;
        assert!(watch_state_changed_since_load(
            &cycle_changed,
            true,
            &state.updated_at,
            state.polling.cycle_number,
        ));

        assert!(watch_state_changed_since_load(
            &state,
            false,
            &state.updated_at,
            state.polling.cycle_number,
        ));
    }

    #[test]
    fn timed_out_collection_marks_all_surfaces_transient() {
        let collection = timed_out_collection(12);
        for (surface, result) in [
            ("metadata", collection.metadata.map(|_| ())),
            ("checks", collection.checks.map(|_| ())),
            ("review_comments", collection.review_comments.map(|_| ())),
            ("issue_comments", collection.issue_comments.map(|_| ())),
            ("reviews", collection.reviews.map(|_| ())),
            ("review_threads", collection.review_threads.map(|_| ())),
        ] {
            let err = result.expect_err("surface should time out");
            assert_eq!(err.surface, surface);
            assert!(err.transient);
            assert!(err.message.contains("max_runtime_seconds=12"));
        }
    }

    #[test]
    fn total_transient_failure_preserves_existing_blockers() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 7,
        });
        state.pending_actionable.push(ActionableItem {
            id: "THREAD_BLOCKER".into(),
            surface: "review_threads".into(),
            summary: "Existing unresolved thread".into(),
            url: Some("https://thread".into()),
            path: Some("src/lib.rs".into()),
            status: Some("unresolved".into()),
            reason: Some("test".into()),
        });
        state.last_cycle.pending_check_count = 1;
        state.last_cycle.failed_check_count = 1;

        let outcome = update_state_from_collection(
            &mut state,
            timed_out_collection(12),
            "2026-05-14T13:00:00Z",
        );

        assert!(outcome.partial_failure);
        assert_eq!(outcome.pending_actionable.len(), 1);
        assert_eq!(outcome.pending_actionable[0].id, "THREAD_BLOCKER");
        assert_eq!(outcome.pending_check_count, 1);
        assert_eq!(outcome.failed_check_count, 1);
        assert_eq!(state.pending_actionable.len(), 1);
        assert_eq!(state.last_cycle.pending_check_count, 1);
        assert_eq!(state.last_cycle.failed_check_count, 1);
    }

    #[test]
    fn scheduled_monitor_prompt_is_structured() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 7,
        });
        state.polling.poll_interval_seconds = 120;
        state.polling.required_quiet_cycles = 2;
        let prompt = scheduled_monitor_prompt(&state, 540);
        assert!(prompt.contains("action=monitor"));
        assert!(prompt.contains("repo=owner/repo"));
        assert!(prompt.contains("pr=7"));
        assert!(prompt.contains("poll_interval_seconds=120"));
        assert!(prompt.contains("quiet_cycles_required=2"));
        assert!(prompt.contains("max_runtime_seconds=540"));
        assert!(prompt.contains("read-only"));
    }

    #[test]
    fn check_classification_detects_pending_and_failed() {
        let pending = CheckRunState {
            id: None,
            name: "ci".into(),
            status: Some("IN_PROGRESS".into()),
            conclusion: None,
            url: None,
        };
        let failed = CheckRunState {
            id: None,
            name: "lint".into(),
            status: Some("COMPLETED".into()),
            conclusion: Some("FAILURE".into()),
            url: None,
        };
        let passed = CheckRunState {
            id: None,
            name: "test".into(),
            status: Some("COMPLETED".into()),
            conclusion: Some("SUCCESS".into()),
            url: None,
        };
        assert!(is_pending_check(&pending));
        assert!(!is_failed_check(&pending));
        assert!(is_failed_check(&failed));
        assert!(!is_pending_check(&passed));
        assert!(!is_failed_check(&passed));
    }

    #[test]
    fn gh_pr_checks_no_checks_exit_one_is_not_transient_failure() {
        assert!(gh_pr_checks_reported_no_checks(
            1,
            b"",
            b"no checks reported on the 'feature' branch"
        ));
        assert!(gh_pr_checks_reported_no_checks(
            1,
            b"No checks reported on the 'feature' branch",
            b""
        ));
    }

    #[test]
    fn gh_pr_checks_classifier_keeps_pending_and_real_failures_distinct() {
        assert!(!gh_pr_checks_reported_no_checks(
            8,
            br#"[{"name":"ci","state":"IN_PROGRESS"}]"#,
            b""
        ));
        assert!(!gh_pr_checks_reported_no_checks(
            1,
            b"",
            b"HTTP 404: Not Found"
        ));
        assert!(!gh_pr_checks_reported_no_checks(
            2,
            b"",
            b"no checks reported"
        ));
    }

    #[test]
    fn update_state_from_collection_preserves_partial_failure_and_actionable() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 9,
        });
        let collection = GhCollection {
            metadata: Ok(jcode_pr_watch_core::PrMetadata {
                identity: jcode_pr_watch_core::PrIdentity {
                    repo: "owner/repo".into(),
                    number: 9,
                    url: Some("https://github.com/owner/repo/pull/9".into()),
                    state: Some("OPEN".into()),
                    base_ref: Some("main".into()),
                    head_ref: Some("feature".into()),
                    head_sha: Some("abc".into()),
                    merge_state: Some("CLEAN".into()),
                    review_decision: None,
                },
                is_draft: Some(false),
            }),
            checks: Ok(vec![CheckRunState {
                id: Some("1".into()),
                name: "ci".into(),
                status: Some("COMPLETED".into()),
                conclusion: Some("SUCCESS".into()),
                url: None,
            }]),
            review_comments: Ok(vec![jcode_pr_watch_core::ReviewComment {
                id: "RC_1".into(),
                path: Some("src/lib.rs".into()),
                line: Some(7),
                url: Some("https://comment".into()),
                updated_at: Some("2026-05-13T17:00:00Z".into()),
                author: Some("reviewer".into()),
                body: Some("Please fix".into()),
            }]),
            issue_comments: Err(SurfaceError::transient("issue_comments", "timeout")),
            reviews: Ok(Vec::new()),
            review_threads: Ok(Vec::new()),
        };
        let outcome = update_state_from_collection(&mut state, collection, "2026-05-13T17:00:00Z");
        assert!(outcome.partial_failure);
        assert_eq!(state.pending_actionable.len(), 1);
        assert_eq!(
            state.last_cycle.status,
            jcode_pr_watch_core::CycleStatus::ActionRequired
        );
        assert!(state.last_seen.review_comments.contains_key("RC_1"));
        assert!(
            state
                .events
                .iter()
                .any(|event| event.kind == "surface_error")
        );
    }

    #[test]
    fn readiness_reasons_explain_actionable_items() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 14,
        });
        state.pending_actionable.push(ActionableItem {
            id: "a1".into(),
            surface: "review_threads".into(),
            summary: "Fix thread".into(),
            url: Some("https://thread".into()),
            path: Some("src/lib.rs".into()),
            status: Some("unresolved".into()),
            reason: Some("test".into()),
        });
        let reasons = readiness_reasons(&state);
        assert!(reasons.iter().any(|reason| reason.contains("actionable")));
        assert!(human_next_step(&state).contains("Address actionable"));
    }

    #[test]
    fn handoff_helpers_include_merge_template_only_when_ready() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 15,
        });
        state.pr.state = Some("OPEN".into());
        state.polling.quiet_cycles = 3;
        state.polling.required_quiet_cycles = 3;
        state.last_cycle.completed_at = Some("2026-05-13T19:00:00Z".into());
        let status = format_status_report(&state, "ready_for_human_merge");
        assert!(status.contains("Next poll: not scheduled"));
        let next = human_next_step(&state);
        assert!(next.contains("gh pr merge 15 --repo owner/repo"));
        assert!(next.contains("[--squash|--merge|--rebase]"));
        let evidence = evidence_lines(&state);
        assert!(
            evidence
                .iter()
                .any(|line| line.contains("Last cycle completed"))
        );
    }
    #[test]
    fn schedule_fields_set_interval_and_next_poll() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 12,
        });
        let mut params = PrWatchInput {
            action: PrWatchAction::PollNow,
            repo: None,
            pr: None,
            watch_id: Some(state.watch_id.clone()),
            dry_run: Some(true),
            schedule_next: true,
            poll_interval_seconds: Some(1),
            quiet_cycles_required: None,
            max_runtime_seconds: None,
            target: None,
            scopes: None,
            reason: None,
            expires_in_minutes: None,
            single_use: None,
            grant_id: None,
            thread_ids: Vec::new(),
            head_sha: None,
            commit_sha: None,
            validation: Vec::new(),
            expected_fingerprint: None,
            expected_cycle_number: None,
            no_code_resolution: false,
            event_mode: None,
            fallback_heartbeat_seconds: None,
            webhook_url_hint: None,
        };
        apply_schedule_fields(&mut state, &params);
        assert_eq!(state.polling.poll_interval_seconds, 60);
        assert!(state.polling.next_poll_at.is_some());

        state.webhook.fallback_heartbeat_seconds = Some(900);
        params.event_mode = Some(PrWatchEventMode::Webhook);
        params.poll_interval_seconds = None;
        apply_schedule_fields(&mut state, &params);
        assert_eq!(state.webhook.fallback_heartbeat_seconds, None);
    }

    #[test]
    fn webhook_refresh_params_do_not_clear_existing_heartbeat() {
        let entry = WebhookWatchIndexEntry {
            watch_id: "owner~2frepo-pr-12".to_string(),
            repo: "owner/repo".to_string(),
            pr: 12,
            root_dir: "/tmp/repo".to_string(),
            state_path: "/tmp/state.json".to_string(),
            event_mode: PrWatchEventMode::Webhook,
            active: true,
            updated_at: "2026-06-19T00:00:00Z".to_string(),
        };
        let params = webhook_refresh_params(&entry, true);

        assert_eq!(params.action, PrWatchAction::PollNow);
        assert_eq!(params.event_mode, None);
        assert_eq!(params.fallback_heartbeat_seconds, None);
    }

    #[test]
    fn webhook_repo_matching_is_case_insensitive() {
        assert!(repos_match("Owner/Repo", "owner/repo"));
        assert!(repos_match("owner/repo", "Owner/Repo"));
        assert!(!repos_match("owner/repo", "owner/other"));
    }

    #[test]
    fn active_webhook_entries_for_repo_filters_before_status_lookup() {
        let index = WebhookWatchIndex {
            entries: vec![WebhookWatchIndexEntry {
                watch_id: "owner~2frepo-pr-12".to_string(),
                repo: "Owner/Repo".to_string(),
                pr: 12,
                root_dir: "/tmp/repo".to_string(),
                state_path: "/tmp/state.json".to_string(),
                event_mode: PrWatchEventMode::Webhook,
                active: true,
                updated_at: "2026-06-19T00:00:00Z".to_string(),
            }],
        };

        assert_eq!(
            active_webhook_entries_for_repo(&index, "owner/repo").len(),
            1
        );
        assert!(active_webhook_entries_for_repo(&index, "other/repo").is_empty());
    }

    #[test]
    fn resolve_addressed_requires_non_empty_commit_or_reason() {
        let mut params = PrWatchInput {
            action: PrWatchAction::ResolveAddressed,
            repo: None,
            pr: None,
            watch_id: Some("owner~2frepo-pr-12".into()),
            dry_run: Some(true),
            schedule_next: false,
            poll_interval_seconds: None,
            quiet_cycles_required: None,
            max_runtime_seconds: None,
            target: None,
            scopes: None,
            reason: None,
            expires_in_minutes: None,
            single_use: None,
            grant_id: None,
            thread_ids: vec!["THREAD".into()],
            head_sha: Some("head".into()),
            commit_sha: None,
            validation: Vec::new(),
            expected_fingerprint: None,
            expected_cycle_number: None,
            no_code_resolution: false,
            event_mode: None,
            fallback_heartbeat_seconds: None,
            webhook_url_hint: None,
        };
        assert!(!has_non_empty_commit_or_reason(&params));

        params.commit_sha = Some("   ".into());
        assert!(!has_non_empty_commit_or_reason(&params));

        params.reason = Some("\t".into());
        assert!(!has_non_empty_commit_or_reason(&params));

        params.commit_sha = Some("abc123".into());
        assert!(has_non_empty_commit_or_reason(&params));

        params.commit_sha = None;
        params.reason = Some("no-code resolution because reviewer asked for verification".into());
        assert!(has_non_empty_commit_or_reason(&params));
    }

    #[test]
    fn code_fix_commit_sha_must_match_current_head_unless_no_code() {
        let mut params = PrWatchInput {
            action: PrWatchAction::ResolveAddressed,
            repo: None,
            pr: None,
            watch_id: Some("owner~2frepo-pr-12".into()),
            dry_run: Some(false),
            schedule_next: false,
            poll_interval_seconds: None,
            quiet_cycles_required: None,
            max_runtime_seconds: None,
            target: None,
            scopes: None,
            reason: Some("addressed by code fix".into()),
            expires_in_minutes: None,
            single_use: None,
            grant_id: None,
            thread_ids: vec!["THREAD".into()],
            head_sha: Some("head".into()),
            commit_sha: Some("other".into()),
            validation: Vec::new(),
            expected_fingerprint: None,
            expected_cycle_number: None,
            no_code_resolution: false,
            event_mode: None,
            fallback_heartbeat_seconds: None,
            webhook_url_hint: None,
        };

        assert!(!has_explicit_no_code_reason(&params));
        assert!(!commit_sha_matches_current_head(&params, "head"));

        params.commit_sha = Some("head".into());
        assert!(commit_sha_matches_current_head(&params, "head"));

        params.commit_sha = None;
        params.reason = Some("no-code resolution because reviewer asked for verification".into());
        assert!(!has_explicit_no_code_reason(&params));
        params.no_code_resolution = true;
        assert!(has_explicit_no_code_reason(&params));
        assert!(!commit_sha_matches_current_head(&params, "head"));
    }

    #[test]
    fn resolve_addressed_rejects_duplicate_thread_ids() {
        assert!(!has_duplicate_thread_ids(&[
            "THREAD_A".to_string(),
            "THREAD_B".to_string(),
        ]));
        assert!(has_duplicate_thread_ids(&[
            "THREAD_A".to_string(),
            "THREAD_A".to_string(),
        ]));
    }

    #[test]
    fn skipped_resolution_attempt_records_complete_audit_context() {
        let params = PrWatchInput {
            action: PrWatchAction::ResolveAddressed,
            repo: None,
            pr: None,
            watch_id: Some("owner~2frepo-pr-12".into()),
            dry_run: Some(false),
            schedule_next: false,
            poll_interval_seconds: None,
            quiet_cycles_required: None,
            max_runtime_seconds: None,
            target: None,
            scopes: None,
            reason: Some("addressed by fix".into()),
            expires_in_minutes: None,
            single_use: None,
            grant_id: None,
            thread_ids: vec!["THREAD_A".into(), "THREAD_B".into()],
            head_sha: Some("head".into()),
            commit_sha: Some("fixsha".into()),
            validation: vec![ValidationEvidence {
                at: "2026-06-18T18:00:00Z".into(),
                command: "cargo test".into(),
                status: "passed".into(),
                summary: Some("unit tests passed".into()),
            }],
            expected_fingerprint: None,
            expected_cycle_number: None,
            no_code_resolution: false,
            event_mode: None,
            fallback_heartbeat_seconds: None,
            webhook_url_hint: None,
        };

        let attempt = skipped_resolution_attempt("THREAD_B", "head", &params);
        assert_eq!(attempt.thread_id, "THREAD_B");
        assert_eq!(attempt.status, ResolutionAttemptStatus::Skipped);
        assert_eq!(attempt.head_sha.as_deref(), Some("head"));
        assert_eq!(attempt.commit_sha.as_deref(), Some("fixsha"));
        assert_eq!(attempt.validation.len(), 1);
        assert_eq!(attempt.reason, "addressed by fix");
        assert_eq!(
            attempt.error.as_deref(),
            Some("skipped due to previous failure in batch")
        );
    }

    #[test]
    fn prior_resolution_attempt_detection_is_thread_specific() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 12,
        });
        assert!(!review_thread_had_prior_resolution_attempt(
            &state, "THREAD_A"
        ));

        state
            .last_resolution_attempts
            .push(ThreadResolutionAttempt {
                thread_id: "THREAD_A".into(),
                attempted_at: "2026-06-18T18:00:00Z".into(),
                status: ResolutionAttemptStatus::Failed,
                head_sha: Some("head".into()),
                commit_sha: Some("fixsha".into()),
                validation: Vec::new(),
                reason: "first attempt failed".into(),
                error: Some("already resolved race".into()),
            });

        assert!(review_thread_had_prior_resolution_attempt(
            &state, "THREAD_A"
        ));
        assert!(!review_thread_had_prior_resolution_attempt(
            &state, "THREAD_B"
        ));
        assert!(!review_thread_had_successful_resolution_attempt(
            &state, "THREAD_A"
        ));

        state
            .last_resolution_attempts
            .push(ThreadResolutionAttempt {
                thread_id: "THREAD_A".into(),
                attempted_at: "2026-06-18T18:01:00Z".into(),
                status: ResolutionAttemptStatus::Resolved,
                head_sha: Some("head".into()),
                commit_sha: Some("fixsha".into()),
                validation: Vec::new(),
                reason: "second attempt resolved".into(),
                error: None,
            });
        assert!(review_thread_had_successful_resolution_attempt(
            &state, "THREAD_A"
        ));
    }

    #[test]
    fn tool_description_names_grant_gated_resolve_action() {
        let tool = PrWatchTool::new();
        let description = tool.description();
        assert!(description.contains("resolve_addressed"));
        assert!(description.contains("grant-gated"));
        assert!(description.contains("watch cycles"));
    }

    #[test]
    fn validation_evidence_requires_passing_statuses() {
        assert!(validation_status_is_passing("passed"));
        assert!(validation_status_is_passing(" SUCCESS "));
        assert!(validation_status_is_passing("ok"));
        assert!(!validation_status_is_passing("failed"));
        assert!(!validation_status_is_passing(""));

        let passing = vec![
            ValidationEvidence {
                at: "2026-06-18T18:00:00Z".into(),
                command: "cargo test".into(),
                status: "passed".into(),
                summary: None,
            },
            ValidationEvidence {
                at: "2026-06-18T18:01:00Z".into(),
                command: "cargo check".into(),
                status: "success".into(),
                summary: None,
            },
        ];
        assert!(all_validation_evidence_is_passing(&passing));

        let mut failing = passing;
        failing.push(ValidationEvidence {
            at: "2026-06-18T18:02:00Z".into(),
            command: "cargo fmt --check".into(),
            status: "failed".into(),
            summary: Some("format failed".into()),
        });
        assert!(!all_validation_evidence_is_passing(&failing));
    }

    #[test]
    fn single_use_resolution_grants_consume_after_remote_success_only() {
        let remote_success = vec![
            ThreadResolutionAttempt {
                thread_id: "THREAD_A".into(),
                attempted_at: "2026-06-18T18:00:00Z".into(),
                status: ResolutionAttemptStatus::Resolved,
                head_sha: Some("head".into()),
                commit_sha: Some("fixsha".into()),
                validation: Vec::new(),
                reason: "addressed".into(),
                error: None,
            },
            ThreadResolutionAttempt {
                thread_id: "THREAD_B".into(),
                attempted_at: "2026-06-18T18:00:01Z".into(),
                status: ResolutionAttemptStatus::Failed,
                head_sha: Some("head".into()),
                commit_sha: Some("fixsha".into()),
                validation: Vec::new(),
                reason: "addressed".into(),
                error: Some("later failure".into()),
            },
        ];
        let remote_resolved_count = remote_success
            .iter()
            .filter(|attempt| matches!(attempt.status, ResolutionAttemptStatus::Resolved))
            .count();
        assert_eq!(remote_resolved_count, 1);

        let already_resolved_only = vec![ThreadResolutionAttempt {
            thread_id: "THREAD_C".into(),
            attempted_at: "2026-06-18T18:00:02Z".into(),
            status: ResolutionAttemptStatus::AlreadyResolved,
            head_sha: Some("head".into()),
            commit_sha: Some("fixsha".into()),
            validation: Vec::new(),
            reason: "addressed".into(),
            error: None,
        }];
        let remote_resolved_count = already_resolved_only
            .iter()
            .filter(|attempt| matches!(attempt.status, ResolutionAttemptStatus::Resolved))
            .count();
        assert_eq!(remote_resolved_count, 0);
    }

    #[test]
    fn merge_resolution_attempts_retains_prior_successes() {
        let prior = vec![ThreadResolutionAttempt {
            thread_id: "THREAD_A".into(),
            attempted_at: "2026-06-18T18:00:00Z".into(),
            status: ResolutionAttemptStatus::Resolved,
            head_sha: Some("head".into()),
            commit_sha: Some("fixsha".into()),
            validation: Vec::new(),
            reason: "addressed".into(),
            error: None,
        }];
        let current = vec![ThreadResolutionAttempt {
            thread_id: "THREAD_B".into(),
            attempted_at: "2026-06-18T18:01:00Z".into(),
            status: ResolutionAttemptStatus::Failed,
            head_sha: Some("head".into()),
            commit_sha: Some("fixsha".into()),
            validation: Vec::new(),
            reason: "retry failed".into(),
            error: Some("still failed".into()),
        }];

        let merged = merge_resolution_attempts(&prior, current);
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(|attempt| {
            attempt.thread_id == "THREAD_A" && attempt.status == ResolutionAttemptStatus::Resolved
        }));
        assert!(merged.iter().any(|attempt| {
            attempt.thread_id == "THREAD_B" && attempt.status == ResolutionAttemptStatus::Failed
        }));
    }

    #[test]
    fn failed_resolution_attempts_remain_actionable_until_resolved() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 12,
        });
        state
            .last_resolution_attempts
            .push(ThreadResolutionAttempt {
                thread_id: "THREAD_FAILED".into(),
                attempted_at: "2026-06-18T18:00:00Z".into(),
                status: ResolutionAttemptStatus::Failed,
                head_sha: Some("head".into()),
                commit_sha: Some("fixsha".into()),
                validation: Vec::new(),
                reason: "retry failed".into(),
                error: Some("GitHub response reported isResolved=false".into()),
            });
        state.last_seen.review_threads.insert(
            "THREAD_FAILED".into(),
            ReviewThreadMarker {
                id: "THREAD_FAILED".into(),
                updated_at: Some("2026-06-18T18:00:00Z".into()),
                resolved: false,
                outdated: false,
                body_hash: Some("hash:failed".into()),
                url: Some("https://thread-failed".into()),
            },
        );

        let mut pending = Vec::new();
        requeue_failed_resolution_threads(&state, &mut pending);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "THREAD_FAILED");
        assert_eq!(
            pending[0].reason.as_deref(),
            Some("failed_resolution_retry")
        );

        state
            .last_seen
            .review_threads
            .get_mut("THREAD_FAILED")
            .unwrap()
            .resolved = true;
        let mut pending = Vec::new();
        requeue_failed_resolution_threads(&state, &mut pending);
        assert!(pending.is_empty());
    }

    #[test]
    fn resolve_addressed_retry_requires_post_resolution_poll_to_be_cleared() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 12,
        });
        assert!(ensure_post_resolution_poll_cleared(&state).is_ok());

        state.resolution_requires_post_poll = true;
        let err = ensure_post_resolution_poll_cleared(&state).unwrap_err();
        assert!(err.to_string().contains("post-resolution poll"));
    }

    #[test]
    fn post_resolution_poll_clears_only_after_review_thread_refresh() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 12,
        });
        assert!(!review_threads_fetch_succeeded_at(
            &state,
            "2026-06-18T18:00:00Z"
        ));

        state
            .last_successful_fetch
            .insert("metadata".into(), "2026-06-18T18:00:00Z".into());
        assert!(!review_threads_fetch_succeeded_at(
            &state,
            "2026-06-18T18:00:00Z"
        ));

        state
            .last_successful_fetch
            .insert("review_threads".into(), "2026-06-18T18:00:00Z".into());
        assert!(review_threads_fetch_succeeded_at(
            &state,
            "2026-06-18T18:00:00Z"
        ));
        assert!(!review_threads_fetch_succeeded_at(
            &state,
            "2026-06-18T18:01:00Z"
        ));
    }

    #[test]
    fn resolve_addressed_requires_current_fingerprint_and_cycle() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 12,
        });
        state.polling.cycle_number = 7;
        state.pending_actionable.push(ActionableItem {
            id: "THREAD_A".into(),
            surface: "review_threads".into(),
            summary: "Please fix stale handoff resolution".into(),
            url: Some("https://thread".into()),
            path: Some("src/lib.rs".into()),
            status: Some("unresolved".into()),
            reason: Some("new_unresolved_thread".into()),
        });
        let fingerprint = actionable_fingerprint(&state.pending_actionable).unwrap();
        let mut params = PrWatchInput {
            action: PrWatchAction::ResolveAddressed,
            repo: None,
            pr: None,
            watch_id: Some(state.watch_id.clone()),
            dry_run: Some(false),
            schedule_next: false,
            poll_interval_seconds: None,
            quiet_cycles_required: None,
            max_runtime_seconds: None,
            target: None,
            scopes: None,
            reason: Some("addressed".into()),
            expires_in_minutes: None,
            single_use: None,
            grant_id: None,
            thread_ids: vec!["THREAD_A".into()],
            head_sha: Some("head".into()),
            commit_sha: Some("head".into()),
            validation: Vec::new(),
            expected_fingerprint: Some(fingerprint.clone()),
            expected_cycle_number: Some(7),
            no_code_resolution: false,
            event_mode: None,
            fallback_heartbeat_seconds: None,
            webhook_url_hint: None,
        };
        assert!(ensure_resolve_freshness_matches(&state, &params).is_ok());

        params.expected_cycle_number = Some(6);
        assert!(
            ensure_resolve_freshness_matches(&state, &params)
                .unwrap_err()
                .to_string()
                .contains("expected_cycle_number is stale")
        );

        state.last_seen.review_threads.insert(
            "THREAD_A".into(),
            ReviewThreadMarker {
                id: "THREAD_A".into(),
                updated_at: Some("2026-06-18T18:00:00Z".into()),
                resolved: false,
                outdated: false,
                body_hash: Some("hash:abc".into()),
                url: Some("https://thread".into()),
            },
        );
        params.expected_cycle_number = Some(7);
        params.expected_fingerprint = Some("stale".into());
        assert!(
            ensure_resolve_freshness_matches(&state, &params)
                .unwrap_err()
                .to_string()
                .contains("expected_fingerprint is stale")
        );

        state.pending_actionable.clear();
        params.expected_fingerprint = Some(
            review_thread_marker_fingerprint(&state, &params.thread_ids)
                .expect("thread marker fingerprint"),
        );
        assert!(ensure_resolve_freshness_matches(&state, &params).is_ok());
    }

    #[test]
    fn scheduled_prompt_is_read_only_and_specific() {
        let state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 13,
        });
        let prompt = scheduled_poll_prompt(&state);
        assert!(prompt.contains("action=ack_baseline"));
        assert!(prompt.contains("repo=owner/repo"));
        assert!(prompt.contains("pr=13"));
        assert!(prompt.contains("Do not push"));
        assert!(prompt.contains("merge"));

        let mut baselined = state;
        baselined
            .last_successful_fetch
            .insert("metadata".to_string(), "2026-05-13T21:00:00Z".to_string());
        assert!(scheduled_poll_prompt(&baselined).contains("action=poll_now"));
    }
    #[test]
    fn ack_baseline_marks_current_feedback_seen_without_actionable() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 11,
        });
        let collection = GhCollection {
            metadata: Ok(jcode_pr_watch_core::PrMetadata {
                identity: jcode_pr_watch_core::PrIdentity {
                    repo: "owner/repo".into(),
                    number: 11,
                    url: Some("https://github.com/owner/repo/pull/11".into()),
                    state: Some("OPEN".into()),
                    base_ref: Some("main".into()),
                    head_ref: Some("feature".into()),
                    head_sha: Some("sha1".into()),
                    merge_state: Some("CLEAN".into()),
                    review_decision: None,
                },
                is_draft: Some(false),
            }),
            checks: Ok(Vec::new()),
            review_comments: Ok(vec![jcode_pr_watch_core::ReviewComment {
                id: "RC_BASE".into(),
                path: Some("src/lib.rs".into()),
                line: Some(1),
                url: Some("https://comment".into()),
                updated_at: Some("2026-05-13T18:00:00Z".into()),
                author: Some("reviewer".into()),
                body: Some("Existing comment".into()),
            }]),
            issue_comments: Ok(Vec::new()),
            reviews: Ok(Vec::new()),
            review_threads: Ok(vec![jcode_pr_watch_core::ReviewThread {
                id: "THREAD_BASE".into(),
                is_resolved: false,
                is_outdated: false,
                path: Some("src/lib.rs".into()),
                line: Some(2),
                url: Some("https://thread".into()),
                updated_at: Some("2026-05-13T18:00:00Z".into()),
                author: Some("reviewer".into()),
                body: Some("Existing thread".into()),
            }]),
        };
        let partial =
            apply_baseline_from_collection(&mut state, collection, "2026-05-13T18:00:00Z");
        assert!(!partial);
        assert!(state.pending_actionable.is_empty());
        assert_eq!(
            state.last_cycle.status,
            jcode_pr_watch_core::CycleStatus::BaselineEstablished
        );
        assert_eq!(state.baseline.head_sha.as_deref(), Some("sha1"));
        assert_eq!(state.baseline.review_comment_count, 1);
        assert_eq!(
            state.baseline.unresolved_thread_ids,
            vec!["THREAD_BASE".to_string()]
        );
        assert!(state.last_seen.review_comments.contains_key("RC_BASE"));
        assert!(state.last_seen.review_threads.contains_key("THREAD_BASE"));

        let collection = GhCollection {
            metadata: Err(SurfaceError::transient("metadata", "skip")),
            checks: Ok(Vec::new()),
            review_comments: Ok(vec![jcode_pr_watch_core::ReviewComment {
                id: "RC_BASE".into(),
                path: Some("src/lib.rs".into()),
                line: Some(1),
                url: Some("https://comment".into()),
                updated_at: Some("2026-05-13T18:00:00Z".into()),
                author: Some("reviewer".into()),
                body: Some("Existing comment".into()),
            }]),
            issue_comments: Ok(Vec::new()),
            reviews: Ok(Vec::new()),
            review_threads: Ok(vec![jcode_pr_watch_core::ReviewThread {
                id: "THREAD_BASE".into(),
                is_resolved: false,
                is_outdated: false,
                path: Some("src/lib.rs".into()),
                line: Some(2),
                url: Some("https://thread".into()),
                updated_at: Some("2026-05-13T18:00:00Z".into()),
                author: Some("reviewer".into()),
                body: Some("Existing thread".into()),
            }]),
        };
        update_state_from_collection(&mut state, collection, "2026-05-13T18:05:00Z");
        assert_eq!(state.pending_actionable.len(), 0);

        let updated_collection = GhCollection {
            metadata: Err(SurfaceError::transient("metadata", "skip")),
            checks: Ok(Vec::new()),
            review_comments: Ok(Vec::new()),
            issue_comments: Ok(Vec::new()),
            reviews: Ok(Vec::new()),
            review_threads: Ok(vec![jcode_pr_watch_core::ReviewThread {
                id: "THREAD_BASE".into(),
                is_resolved: false,
                is_outdated: false,
                path: Some("src/lib.rs".into()),
                line: Some(2),
                url: Some("https://thread".into()),
                updated_at: Some("2026-05-13T18:10:00Z".into()),
                author: Some("reviewer".into()),
                body: Some("Existing thread with new reply".into()),
            }]),
        };
        update_state_from_collection(&mut state, updated_collection, "2026-05-13T18:10:00Z");
        assert_eq!(state.pending_actionable.len(), 1);
        assert_eq!(state.pending_actionable[0].id, "THREAD_BASE");
        assert_eq!(
            state.pending_actionable[0].reason.as_deref(),
            Some("changed_unresolved_thread")
        );
    }
    #[test]
    fn unresolved_review_threads_are_actionable_and_resolved_are_not() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 10,
        });
        let collection = GhCollection {
            metadata: Err(SurfaceError::transient("metadata", "skip")),
            checks: Ok(Vec::new()),
            review_comments: Ok(Vec::new()),
            issue_comments: Ok(Vec::new()),
            reviews: Ok(Vec::new()),
            review_threads: Ok(vec![
                jcode_pr_watch_core::ReviewThread {
                    id: "THREAD_OPEN".into(),
                    is_resolved: false,
                    is_outdated: false,
                    path: Some("src/lib.rs".into()),
                    line: Some(12),
                    url: Some("https://thread-open".into()),
                    updated_at: Some("2026-05-13T17:00:00Z".into()),
                    author: Some("reviewer".into()),
                    body: Some("Please address this thread".into()),
                },
                jcode_pr_watch_core::ReviewThread {
                    id: "THREAD_RESOLVED".into(),
                    is_resolved: true,
                    is_outdated: false,
                    path: Some("src/lib.rs".into()),
                    line: Some(20),
                    url: Some("https://thread-resolved".into()),
                    updated_at: Some("2026-05-13T17:00:00Z".into()),
                    author: Some("reviewer".into()),
                    body: Some("Already resolved".into()),
                },
            ]),
        };
        let outcome = update_state_from_collection(&mut state, collection, "2026-05-13T17:00:00Z");
        assert!(outcome.partial_failure);
        assert_eq!(state.pending_actionable.len(), 1);
        assert_eq!(state.pending_actionable[0].id, "THREAD_OPEN");
        assert_eq!(
            state.baseline.unresolved_thread_ids,
            vec!["THREAD_OPEN".to_string()]
        );
        assert!(state.last_seen.review_threads.contains_key("THREAD_OPEN"));
        assert!(
            state
                .last_seen
                .review_threads
                .contains_key("THREAD_RESOLVED")
        );
    }
    #[test]
    fn automation_chatter_is_not_actionable() {
        assert!(!is_automation_chatter(
            Some("github-actions[bot]"),
            Some("Progress update")
        ));
        assert!(is_automation_chatter(
            Some("human"),
            Some("fix-summary: addressed feedback")
        ));
        assert!(is_automation_chatter(
            Some("shopify"),
            Some(
                "Oxygen deployed a preview of your `feature` branch. Details:\n| Storefront | Status | Preview link | Deployment details |"
            )
        ));
        assert!(!is_automation_chatter(
            Some("reviewer"),
            Some("Please fix this")
        ));
        assert!(!is_automation_chatter(
            Some("shopify"),
            Some("Please fix the deployment configuration")
        ));
        assert!(is_automation_chatter(
            Some("vercel[bot]"),
            Some("Vercel deployment completed. Visit Preview: https://example.vercel.app")
        ));
        assert!(is_automation_chatter(
            Some("google-labs-jules"),
            Some("👋 Jules, reporting for duty! I'm here to lend a hand with this pull request.")
        ));
        assert!(!is_automation_chatter(
            Some("vercel[bot]"),
            Some("[high] The preview failed because auth is broken. Please fix before merge.")
        ));
    }
}
