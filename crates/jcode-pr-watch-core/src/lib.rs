use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA_VERSION: u32 = 2;
pub const DEFAULT_MAX_EVENTS: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrTarget {
    pub repo: String,
    pub number: u64,
}

impl PrTarget {
    pub fn watch_id(&self) -> String {
        format!(
            "{}-pr-{}",
            escape_watch_id_component(&self.repo),
            self.number
        )
    }
}

fn escape_watch_id_component(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                escaped.push(byte as char);
            }
            _ => escaped.push_str(&format!("~{byte:02x}")),
        }
    }
    escaped
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrIdentity {
    pub repo: String,
    pub number: u64,
    pub url: Option<String>,
    pub state: Option<String>,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    pub head_sha: Option<String>,
    pub merge_state: Option<String>,
    pub review_decision: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum WriteScope {
    Push,
    Comment,
    ResolveThreads,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WritePolicy {
    pub local_fix: bool,
    pub commit: bool,
    pub push: bool,
    pub comment: bool,
    pub resolve_threads: bool,
}

impl Default for WritePolicy {
    fn default() -> Self {
        Self {
            local_fix: true,
            commit: false,
            push: false,
            comment: false,
            resolve_threads: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizationGrant {
    pub grant_id: String,
    pub granted_at: String,
    pub expires_at: String,
    pub granted_by_session_id: String,
    pub scopes: BTreeSet<WriteScope>,
    pub single_use: bool,
    pub reason: Option<String>,
}

impl AuthorizationGrant {
    pub fn grants(&self, scope: WriteScope, now: &str, current_session_id: &str) -> bool {
        self.scopes.contains(&scope)
            && now <= self.expires_at.as_str()
            && self.granted_by_session_id == current_session_id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AuthorizationState {
    pub active_grants: Vec<AuthorizationGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Marker {
    pub id: String,
    pub updated_at: Option<String>,
    pub author: Option<String>,
    pub body_hash: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewThreadMarker {
    pub id: String,
    pub updated_at: Option<String>,
    pub resolved: bool,
    pub outdated: bool,
    pub body_hash: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LastSeen {
    pub review_threads: BTreeMap<String, ReviewThreadMarker>,
    pub review_comments: BTreeMap<String, Marker>,
    pub issue_comments: BTreeMap<String, Marker>,
    pub reviews: BTreeMap<String, Marker>,
    pub timeline: BTreeMap<String, Marker>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CheckRunState {
    pub id: Option<String>,
    pub name: String,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LastChecksForSha {
    pub head_sha: Option<String>,
    pub runs: Vec<CheckRunState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Baseline {
    pub head_sha: Option<String>,
    pub established_at: Option<String>,
    pub unresolved_thread_ids: Vec<String>,
    pub review_comment_count: usize,
    pub issue_comment_count: usize,
    pub review_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollingState {
    pub cycle_number: u64,
    pub quiet_cycles: u64,
    pub required_quiet_cycles: u64,
    pub poll_interval_seconds: u64,
    pub final_poll_due_at: Option<String>,
    pub next_poll_at: Option<String>,
    pub consecutive_transient_failures: u64,
}

impl Default for PollingState {
    fn default() -> Self {
        Self {
            cycle_number: 0,
            quiet_cycles: 0,
            required_quiet_cycles: 3,
            poll_interval_seconds: 300,
            final_poll_due_at: None,
            next_poll_at: None,
            consecutive_transient_failures: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CycleStatus {
    BaselineEstablished,
    Collecting,
    QuietPending,
    QuietSatisfied,
    FinalPollPending,
    ActionRequired,
    ChecksPending,
    ChecksFailed,
    TransientFailure,
    OperationalError,
    Stopped,
    TerminalReadyForHuman,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CycleSummary {
    pub completed_at: Option<String>,
    pub status: CycleStatus,
    pub surfaces_checked: Vec<String>,
    pub surface_counts: BTreeMap<String, usize>,
    pub actionable_count: usize,
    pub pending_check_count: usize,
    pub failed_check_count: usize,
}

impl Default for CycleSummary {
    fn default() -> Self {
        Self {
            completed_at: None,
            status: CycleStatus::Collecting,
            surfaces_checked: Vec::new(),
            surface_counts: BTreeMap::new(),
            actionable_count: 0,
            pending_check_count: 0,
            failed_check_count: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchEvent {
    pub at: String,
    pub kind: String,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionableItem {
    pub id: String,
    pub surface: String,
    pub summary: String,
    pub url: Option<String>,
    pub path: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationEvidence {
    pub at: String,
    pub command: String,
    pub status: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrWatchState {
    pub schema_version: u32,
    pub watch_id: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub terminal: bool,
    pub stop_reason: Option<String>,
    pub pr: PrIdentity,
    pub policy: WritePolicy,
    pub authorization: AuthorizationState,
    pub polling: PollingState,
    pub baseline: Baseline,
    pub last_seen: LastSeen,
    pub last_checks_for_sha: LastChecksForSha,
    pub last_successful_fetch: BTreeMap<String, String>,
    pub last_cycle: CycleSummary,
    pub pending_actionable: Vec<ActionableItem>,
    pub last_validation: Vec<ValidationEvidence>,
    pub events: Vec<WatchEvent>,
}

impl PrWatchState {
    pub fn new(target: PrTarget) -> Self {
        let watch_id = target.watch_id();
        Self {
            schema_version: SCHEMA_VERSION,
            watch_id,
            created_at: None,
            updated_at: None,
            terminal: false,
            stop_reason: None,
            pr: PrIdentity {
                repo: target.repo,
                number: target.number,
                url: None,
                state: None,
                base_ref: None,
                head_ref: None,
                head_sha: None,
                merge_state: None,
                review_decision: None,
            },
            policy: WritePolicy::default(),
            authorization: AuthorizationState::default(),
            polling: PollingState::default(),
            baseline: Baseline::default(),
            last_seen: LastSeen::default(),
            last_checks_for_sha: LastChecksForSha::default(),
            last_successful_fetch: BTreeMap::new(),
            last_cycle: CycleSummary::default(),
            pending_actionable: Vec::new(),
            last_validation: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn push_event(&mut self, event: WatchEvent) {
        self.events.push(event);
        if self.events.len() > DEFAULT_MAX_EVENTS {
            let excess = self.events.len() - DEFAULT_MAX_EVENTS;
            self.events.drain(0..excess);
        }
    }

    pub fn apply_cycle_outcome(&mut self, outcome: CycleOutcome) {
        self.polling.cycle_number = self.polling.cycle_number.saturating_add(1);
        self.polling.consecutive_transient_failures = 0;
        self.pending_actionable = outcome.pending_actionable;
        self.last_cycle.actionable_count = self.pending_actionable.len();
        self.last_cycle.pending_check_count = outcome.pending_check_count;
        self.last_cycle.failed_check_count = outcome.failed_check_count;

        if !self.pending_actionable.is_empty() {
            self.polling.quiet_cycles = 0;
            self.last_cycle.status = CycleStatus::ActionRequired;
        } else if outcome.failed_check_count > 0 {
            self.polling.quiet_cycles = 0;
            self.last_cycle.status = CycleStatus::ChecksFailed;
        } else if outcome.pending_check_count > 0 {
            self.polling.quiet_cycles = 0;
            self.last_cycle.status = CycleStatus::ChecksPending;
        } else if outcome.partial_failure {
            self.last_cycle.status = CycleStatus::TransientFailure;
            self.polling.consecutive_transient_failures = self
                .polling
                .consecutive_transient_failures
                .saturating_add(1);
        } else {
            self.polling.quiet_cycles = self.polling.quiet_cycles.saturating_add(1);
            self.last_cycle.status =
                if self.polling.quiet_cycles >= self.polling.required_quiet_cycles {
                    CycleStatus::QuietSatisfied
                } else {
                    CycleStatus::QuietPending
                };
        }
    }

    pub fn readiness(&self) -> Readiness {
        if self.terminal && self.stop_reason.as_deref() != Some("quiet_cycles_satisfied") {
            return Readiness::BlockedByPolicy;
        }
        if self.pr.state.as_deref() != Some("OPEN") && self.pr.state.is_some() {
            return Readiness::BlockedByClosedPr;
        }
        if self.last_cycle.status == CycleStatus::TransientFailure
            || self.polling.consecutive_transient_failures > 0
        {
            return Readiness::NotReadyValidationStale;
        }
        if !self.pending_actionable.is_empty() {
            return Readiness::NotReadyActionRequired;
        }
        if self.last_cycle.failed_check_count > 0 {
            return Readiness::NotReadyChecksFailed;
        }
        if self.last_cycle.pending_check_count > 0 {
            return Readiness::NotReadyChecksPending;
        }
        if matches!(
            self.pr.review_decision.as_deref(),
            Some("CHANGES_REQUESTED" | "REVIEW_REQUIRED")
        ) {
            return Readiness::NotReadyActionRequired;
        }
        if matches!(
            self.pr.merge_state.as_deref(),
            Some("DIRTY" | "BLOCKED" | "UNKNOWN" | "UNSTABLE" | "HAS_HOOKS" | "BEHIND" | "DRAFT")
        ) {
            return Readiness::BlockedByPolicy;
        }
        if self.polling.quiet_cycles >= self.polling.required_quiet_cycles {
            return Readiness::ReadyForHumanMerge;
        }
        Readiness::ReadyForHumanReview
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CycleOutcome {
    pub pending_actionable: Vec<ActionableItem>,
    pub pending_check_count: usize,
    pub failed_check_count: usize,
    pub partial_failure: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    NotReadyActionRequired,
    NotReadyChecksPending,
    NotReadyChecksFailed,
    NotReadyValidationStale,
    ReadyForHumanReview,
    ReadyForHumanPush,
    ReadyForHumanMerge,
    BlockedByPolicy,
    BlockedByClosedPr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeError {
    InvalidJson(String),
    MissingPrIdentity,
}

impl std::fmt::Display for NormalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(message) => write!(f, "invalid JSON: {message}"),
            Self::MissingPrIdentity => write!(f, "missing PR repo or number"),
        }
    }
}

impl std::error::Error for NormalizeError {}

pub fn normalize_watch_state_json(input: &str) -> Result<PrWatchState, NormalizeError> {
    let value: Value =
        serde_json::from_str(input).map_err(|err| NormalizeError::InvalidJson(err.to_string()))?;
    if value.get("schema_version").and_then(Value::as_u64) == Some(2) {
        return serde_json::from_value(value)
            .map_err(|err| NormalizeError::InvalidJson(err.to_string()));
    }
    normalize_v1_value(&value)
}

fn normalize_v1_value(value: &Value) -> Result<PrWatchState, NormalizeError> {
    let pr = value
        .get("pr")
        .and_then(Value::as_object)
        .ok_or(NormalizeError::MissingPrIdentity)?;
    let repo = pr
        .get("repo")
        .and_then(Value::as_str)
        .ok_or(NormalizeError::MissingPrIdentity)?
        .to_string();
    let number = pr
        .get("number")
        .and_then(Value::as_u64)
        .ok_or(NormalizeError::MissingPrIdentity)?;
    let mut state = PrWatchState::new(PrTarget {
        repo: repo.clone(),
        number,
    });
    state.created_at = value
        .get("created_at")
        .and_then(Value::as_str)
        .map(str::to_string);
    state.updated_at = value
        .get("updated_at")
        .or_else(|| value.pointer("/last_cycle/completed_at"))
        .and_then(Value::as_str)
        .map(str::to_string);
    state.terminal = value
        .get("terminal")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    state.stop_reason = value
        .get("stop_reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    state.pr.url = pr.get("url").and_then(Value::as_str).map(str::to_string);
    state.pr.state = pr.get("state").and_then(Value::as_str).map(str::to_string);
    state.pr.base_ref = pr
        .get("baseRefName")
        .or_else(|| pr.get("base_ref"))
        .and_then(Value::as_str)
        .map(str::to_string);
    state.pr.head_ref = pr
        .get("headRefName")
        .or_else(|| pr.get("head_ref"))
        .and_then(Value::as_str)
        .map(str::to_string);
    state.pr.head_sha = pr
        .get("head_sha")
        .or_else(|| pr.get("headRefOid"))
        .and_then(Value::as_str)
        .map(str::to_string);
    state.pr.merge_state = pr
        .get("mergeStateStatus")
        .or_else(|| pr.get("merge_state"))
        .and_then(Value::as_str)
        .map(str::to_string);

    state.polling.cycle_number = value
        .get("cycle_number")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    state.polling.quiet_cycles = value
        .get("quiet_cycles")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    state.polling.consecutive_transient_failures = value
        .get("consecutive_transient_failures")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    if let Some(last_cycle) = value.get("last_cycle").and_then(Value::as_object) {
        state.last_cycle.completed_at = last_cycle
            .get("completed_at")
            .and_then(Value::as_str)
            .map(str::to_string);
        state.last_cycle.actionable_count = last_cycle
            .get("actionable_count")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        state.last_cycle.surfaces_checked = last_cycle
            .get("surfaces_checked")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        state.last_cycle.surface_counts = last_cycle
            .get("surface_counts")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n as usize)))
                    .collect()
            })
            .unwrap_or_default();
        state.last_cycle.status = match last_cycle.get("status").and_then(Value::as_str) {
            Some("quiet_validation") => CycleStatus::QuietPending,
            Some("actionable_locally_fixed_waiting_for_write_approval") => {
                CycleStatus::ActionRequired
            }
            Some("quiet_satisfied") => CycleStatus::QuietSatisfied,
            Some("action_required") => CycleStatus::ActionRequired,
            Some("checks_failed") => CycleStatus::ChecksFailed,
            Some("checks_pending") => CycleStatus::ChecksPending,
            Some("transient_failure") => CycleStatus::TransientFailure,
            _ => CycleStatus::Collecting,
        };
    }

    if let Some(fetch) = value
        .get("surface_last_successful_fetch")
        .and_then(Value::as_object)
    {
        state.last_successful_fetch = fetch
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
    }

    if let Some(markers) = value
        .get("last_seen_review_markers")
        .and_then(Value::as_object)
    {
        for (surface, bucket) in markers {
            if let Some(bucket) = bucket.as_object() {
                for (id, updated_value) in bucket {
                    let updated_at = updated_value
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    match surface.as_str() {
                        "review_threads" => {
                            state.last_seen.review_threads.insert(
                                id.clone(),
                                ReviewThreadMarker {
                                    id: id.clone(),
                                    updated_at,
                                    resolved: false,
                                    outdated: false,
                                    body_hash: None,
                                    url: None,
                                },
                            );
                        }
                        "review_comments" => {
                            insert_marker(&mut state.last_seen.review_comments, id, updated_at)
                        }
                        "issue_comments" => {
                            insert_marker(&mut state.last_seen.issue_comments, id, updated_at)
                        }
                        "reviews" => insert_marker(&mut state.last_seen.reviews, id, updated_at),
                        "timeline" => insert_marker(&mut state.last_seen.timeline, id, updated_at),
                        _ => {}
                    }
                }
            }
        }
    }

    if let Some(items) = value.get("pending_actionable").and_then(Value::as_array) {
        state.pending_actionable = items
            .iter()
            .enumerate()
            .map(|(index, item)| ActionableItem {
                id: item
                    .get("thread_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("legacy-{index}")),
                surface: "review_threads".to_string(),
                summary: item
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("legacy actionable item")
                    .to_string(),
                url: item.get("url").and_then(Value::as_str).map(str::to_string),
                path: item.get("path").and_then(Value::as_str).map(str::to_string),
                status: item
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect();
    }

    if let Some(events) = value.get("events").and_then(Value::as_array) {
        for event in events.iter().rev().take(DEFAULT_MAX_EVENTS).rev() {
            if let Some(at) = event
                .get("at")
                .or_else(|| event.get("completed_at"))
                .and_then(Value::as_str)
            {
                state.events.push(WatchEvent {
                    at: at.to_string(),
                    kind: event
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("legacy_event")
                        .to_string(),
                    data: event.clone(),
                });
            }
        }
    }

    if state.last_cycle.actionable_count == 0 {
        state.last_cycle.actionable_count = state.pending_actionable.len();
    }
    Ok(state)
}

fn insert_marker(map: &mut BTreeMap<String, Marker>, id: &str, updated_at: Option<String>) {
    map.insert(
        id.to_string(),
        Marker {
            id: id.to_string(),
            updated_at,
            author: None,
            body_hash: None,
            url: None,
        },
    );
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SurfaceError {
    pub surface: String,
    pub message: String,
    pub transient: bool,
}

impl SurfaceError {
    pub fn transient(surface: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            surface: surface.into(),
            message: message.into(),
            transient: true,
        }
    }

    pub fn permanent(surface: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            surface: surface.into(),
            message: message.into(),
            transient: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewThread {
    pub id: String,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub url: Option<String>,
    pub updated_at: Option<String>,
    pub author: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewComment {
    pub id: String,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub url: Option<String>,
    pub updated_at: Option<String>,
    pub author: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueComment {
    pub id: String,
    pub url: Option<String>,
    pub updated_at: Option<String>,
    pub author: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Review {
    pub id: String,
    pub state: Option<String>,
    pub submitted_at: Option<String>,
    pub author: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimelineEvent {
    pub id: String,
    pub kind: Option<String>,
    pub created_at: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrMetadata {
    pub identity: PrIdentity,
    pub is_draft: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSnapshotResult {
    pub metadata: Result<PrMetadata, SurfaceError>,
    pub checks: Result<Vec<CheckRunState>, SurfaceError>,
    pub review_threads: Result<Vec<ReviewThread>, SurfaceError>,
    pub review_comments: Result<Vec<ReviewComment>, SurfaceError>,
    pub issue_comments: Result<Vec<IssueComment>, SurfaceError>,
    pub reviews: Result<Vec<Review>, SurfaceError>,
    pub timeline: Result<Vec<TimelineEvent>, SurfaceError>,
}

impl PrSnapshotResult {
    pub fn has_partial_failure(&self) -> bool {
        self.metadata.is_err()
            || self.checks.is_err()
            || self.review_threads.is_err()
            || self.review_comments.is_err()
            || self.issue_comments.is_err()
            || self.reviews.is_err()
            || self.timeline.is_err()
    }

    pub fn failed_surfaces(&self) -> Vec<String> {
        let mut failed = Vec::new();
        if let Err(err) = &self.metadata {
            failed.push(err.surface.clone());
        }
        if let Err(err) = &self.checks {
            failed.push(err.surface.clone());
        }
        if let Err(err) = &self.review_threads {
            failed.push(err.surface.clone());
        }
        if let Err(err) = &self.review_comments {
            failed.push(err.surface.clone());
        }
        if let Err(err) = &self.issue_comments {
            failed.push(err.surface.clone());
        }
        if let Err(err) = &self.reviews {
            failed.push(err.surface.clone());
        }
        if let Err(err) = &self.timeline {
            failed.push(err.surface.clone());
        }
        failed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhParseError {
    InvalidJson(String),
    ExpectedObject(&'static str),
    ExpectedArray(&'static str),
    MissingField(&'static str),
}

impl std::fmt::Display for GhParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(message) => write!(f, "invalid gh JSON: {message}"),
            Self::ExpectedObject(surface) => write!(f, "expected object for {surface}"),
            Self::ExpectedArray(surface) => write!(f, "expected array for {surface}"),
            Self::MissingField(field) => write!(f, "missing field {field}"),
        }
    }
}

impl std::error::Error for GhParseError {}

pub fn parse_gh_pr_view(repo: &str, number: u64, stdout: &str) -> Result<PrMetadata, GhParseError> {
    let value: Value =
        serde_json::from_str(stdout).map_err(|err| GhParseError::InvalidJson(err.to_string()))?;
    let object = value
        .as_object()
        .ok_or(GhParseError::ExpectedObject("pr_view"))?;
    let identity = PrIdentity {
        repo: repo.to_string(),
        number,
        url: string_field(object, "url"),
        state: string_field(object, "state"),
        base_ref: string_field(object, "baseRefName").or_else(|| string_field(object, "base_ref")),
        head_ref: string_field(object, "headRefName").or_else(|| string_field(object, "head_ref")),
        head_sha: string_field(object, "headRefOid").or_else(|| string_field(object, "head_sha")),
        merge_state: string_field(object, "mergeStateStatus")
            .or_else(|| string_field(object, "mergeable")),
        review_decision: string_field(object, "reviewDecision"),
    };
    Ok(PrMetadata {
        identity,
        is_draft: object.get("isDraft").and_then(Value::as_bool),
    })
}

pub fn parse_gh_checks(stdout: &str) -> Result<Vec<CheckRunState>, GhParseError> {
    let value: Value = parse_json_values(stdout)?;
    let array = value
        .as_array()
        .ok_or(GhParseError::ExpectedArray("checks"))?;
    Ok(array
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let obj = item.as_object();
            CheckRunState {
                id: obj
                    .and_then(|o| string_field(o, "id"))
                    .or_else(|| obj.and_then(|o| string_field(o, "databaseId")))
                    .or_else(|| Some(idx.to_string())),
                name: obj
                    .and_then(|o| string_field(o, "name"))
                    .or_else(|| obj.and_then(|o| string_field(o, "context")))
                    .unwrap_or_else(|| format!("check-{idx}")),
                status: obj
                    .and_then(|o| string_field(o, "status"))
                    .or_else(|| obj.and_then(|o| string_field(o, "bucket")))
                    .or_else(|| obj.and_then(|o| string_field(o, "state"))),
                conclusion: obj
                    .and_then(|o| string_field(o, "conclusion"))
                    .or_else(|| obj.and_then(|o| string_field(o, "bucket")))
                    .or_else(|| obj.and_then(|o| string_field(o, "state"))),
                url: obj
                    .and_then(|o| string_field(o, "detailsUrl"))
                    .or_else(|| obj.and_then(|o| string_field(o, "targetUrl"))),
            }
        })
        .collect())
}

pub fn parse_gh_review_comments(stdout: &str) -> Result<Vec<ReviewComment>, GhParseError> {
    let value = parse_json_values(stdout)?;
    let array = value
        .as_array()
        .ok_or(GhParseError::ExpectedArray("review_comments"))?;
    Ok(array
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let id = string_field(obj, "node_id").or_else(|| string_field(obj, "id"))?;
            Some(ReviewComment {
                id,
                path: string_field(obj, "path"),
                line: obj
                    .get("line")
                    .and_then(Value::as_u64)
                    .or_else(|| obj.get("original_line").and_then(Value::as_u64)),
                url: string_field(obj, "html_url").or_else(|| string_field(obj, "url")),
                updated_at: string_field(obj, "updated_at"),
                author: login_from_user(obj.get("user")),
                body: string_field(obj, "body"),
            })
        })
        .collect())
}

pub fn parse_gh_issue_comments(stdout: &str) -> Result<Vec<IssueComment>, GhParseError> {
    let value = parse_json_values(stdout)?;
    let array = value
        .as_array()
        .ok_or(GhParseError::ExpectedArray("issue_comments"))?;
    Ok(array
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let id = string_field(obj, "node_id").or_else(|| string_field(obj, "id"))?;
            Some(IssueComment {
                id,
                url: string_field(obj, "html_url").or_else(|| string_field(obj, "url")),
                updated_at: string_field(obj, "updated_at"),
                author: login_from_user(obj.get("user")),
                body: string_field(obj, "body"),
            })
        })
        .collect())
}

pub fn parse_gh_reviews(stdout: &str) -> Result<Vec<Review>, GhParseError> {
    let value = parse_json_values(stdout)?;
    let array = value
        .as_array()
        .ok_or(GhParseError::ExpectedArray("reviews"))?;
    Ok(array
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let id = string_field(obj, "node_id").or_else(|| string_field(obj, "id"))?;
            Some(Review {
                id,
                state: string_field(obj, "state"),
                submitted_at: string_field(obj, "submitted_at")
                    .or_else(|| string_field(obj, "submittedAt")),
                author: login_from_user(obj.get("user"))
                    .or_else(|| login_from_user(obj.get("author"))),
                body: string_field(obj, "body"),
            })
        })
        .collect())
}

pub fn parse_gh_review_threads(stdout: &str) -> Result<Vec<ReviewThread>, GhParseError> {
    let value: Value =
        serde_json::from_str(stdout).map_err(|err| GhParseError::InvalidJson(err.to_string()))?;
    let nodes = value
        .pointer("/data/repository/pullRequest/reviewThreads/nodes")
        .or_else(|| value.pointer("/reviewThreads/nodes"))
        .and_then(Value::as_array)
        .ok_or(GhParseError::ExpectedArray("review_threads"))?;
    Ok(nodes
        .iter()
        .filter_map(|thread| {
            let obj = thread.as_object()?;
            let id = string_field(obj, "id")?;
            let latest_comment = thread
                .pointer("/comments/nodes")
                .and_then(Value::as_array)
                .and_then(|comments| comments.last())
                .and_then(Value::as_object);
            Some(ReviewThread {
                id,
                is_resolved: obj
                    .get("isResolved")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                is_outdated: obj
                    .get("isOutdated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                path: latest_comment.and_then(|c| string_field(c, "path")),
                line: latest_comment.and_then(|c| c.get("line").and_then(Value::as_u64)),
                url: latest_comment.and_then(|c| string_field(c, "url")),
                updated_at: latest_comment.and_then(|c| {
                    string_field(c, "updatedAt").or_else(|| string_field(c, "createdAt"))
                }),
                author: latest_comment.and_then(|c| login_from_user(c.get("author"))),
                body: latest_comment.and_then(|c| string_field(c, "body")),
            })
        })
        .collect())
}

fn parse_json_values(stdout: &str) -> Result<Value, GhParseError> {
    let text = stdout.trim();
    if text.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let decoder = serde_json::Deserializer::from_str(text);
    let values: Result<Vec<Value>, _> = decoder.into_iter::<Value>().collect();
    let values = values.map_err(|err| GhParseError::InvalidJson(err.to_string()))?;
    if values.len() == 1 {
        return Ok(values.into_iter().next().unwrap());
    }
    if values.iter().all(Value::is_array) {
        let mut merged = Vec::new();
        for value in values {
            merged.extend(value.as_array().cloned().unwrap_or_default());
        }
        return Ok(Value::Array(merged));
    }
    Ok(Value::Array(values))
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(|value| match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    })
}

fn login_from_user(value: Option<&Value>) -> Option<String> {
    let object = value?.as_object()?;
    string_field(object, "login").or_else(|| string_field(object, "name"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_v1() -> &'static str {
        r#"{
          "consecutive_transient_failures": 0,
          "cycle_number": 2,
          "last_cycle": {
            "actionable_count": 0,
            "completed_at": "2026-05-11T10:30:50Z",
            "status": "quiet_validation",
            "surface_counts": {"timeline": 3},
            "surfaces_checked": ["review_threads", "timeline"]
          },
          "last_seen_review_markers": {
            "review_comments": {"3183715059": "2026-05-04T18:43:54Z"},
            "review_threads": {"PRRT_kwDOSQE1Ec5_cHz3": "2026-05-04T18:43:54Z"},
            "timeline": {"cross-referenced:2026-05-11T06:00:53Z": "2026-05-11T06:00:53Z"}
          },
          "pr": {"baseRefName": "master", "headRefName": "fix/example", "number": 188, "repo": "1jehuang/jcode", "state": "OPEN", "url": "https://github.com/1jehuang/jcode/pull/188"},
          "quiet_cycles": 2,
          "schema_version": 1,
          "terminal": true,
          "write_authorized": false
        }"#
    }

    #[test]
    fn new_state_does_not_include_merge_policy() {
        let state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 7,
        });
        let json = serde_json::to_value(&state.policy).unwrap();
        assert!(json.get("merge").is_none());
        assert_eq!(state.watch_id, "owner~2frepo-pr-7");
    }

    #[test]
    fn authorization_grant_is_session_and_expiry_bound() {
        let grant = AuthorizationGrant {
            grant_id: "g1".into(),
            granted_at: "2026-05-13T17:00:00Z".into(),
            expires_at: "2026-05-13T18:00:00Z".into(),
            granted_by_session_id: "session_a".into(),
            scopes: BTreeSet::from([WriteScope::Push]),
            single_use: true,
            reason: None,
        };
        assert!(grant.grants(WriteScope::Push, "2026-05-13T17:30:00Z", "session_a"));
        assert!(!grant.grants(WriteScope::Push, "2026-05-13T18:30:00Z", "session_a"));
        assert!(!grant.grants(WriteScope::Push, "2026-05-13T17:30:00Z", "session_b"));
        assert!(!grant.grants(WriteScope::Comment, "2026-05-13T17:30:00Z", "session_a"));
    }

    #[test]
    fn push_event_keeps_newest_fifty() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 1,
        });
        for i in 0..60 {
            state.push_event(WatchEvent {
                at: format!("{i:02}"),
                kind: "test".into(),
                data: Value::Null,
            });
        }
        assert_eq!(state.events.len(), DEFAULT_MAX_EVENTS);
        assert_eq!(state.events.first().unwrap().at, "10");
        assert_eq!(state.events.last().unwrap().at, "59");
    }

    #[test]
    fn v1_state_normalizes_markers_and_pr_identity() {
        let state = normalize_watch_state_json(sample_v1()).unwrap();
        assert_eq!(state.schema_version, 2);
        assert_eq!(state.watch_id, "1jehuang~2fjcode-pr-188");
        assert_eq!(state.pr.repo, "1jehuang/jcode");
        assert_eq!(state.pr.number, 188);
        assert_eq!(state.pr.base_ref.as_deref(), Some("master"));
        assert_eq!(state.polling.quiet_cycles, 2);
        assert!(
            state
                .last_seen
                .review_threads
                .contains_key("PRRT_kwDOSQE1Ec5_cHz3")
        );
        assert!(state.last_seen.review_comments.contains_key("3183715059"));
        assert!(
            state
                .last_seen
                .timeline
                .contains_key("cross-referenced:2026-05-11T06:00:53Z")
        );
        assert_eq!(state.last_cycle.status, CycleStatus::QuietPending);
    }

    #[test]
    fn partial_failure_does_not_increment_quiet_cycles_or_ready() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 1,
        });
        state.pr.state = Some("OPEN".into());
        state.polling.quiet_cycles = 2;
        state.apply_cycle_outcome(CycleOutcome {
            partial_failure: true,
            ..CycleOutcome::default()
        });
        assert_eq!(state.polling.quiet_cycles, 2);
        assert_eq!(state.last_cycle.status, CycleStatus::TransientFailure);
        assert_eq!(state.readiness(), Readiness::NotReadyValidationStale);
    }

    #[test]
    fn clean_cycles_reach_ready_for_human_merge() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 1,
        });
        state.pr.state = Some("OPEN".into());
        state.polling.required_quiet_cycles = 2;
        state.apply_cycle_outcome(CycleOutcome::default());
        assert_eq!(state.last_cycle.status, CycleStatus::QuietPending);
        state.apply_cycle_outcome(CycleOutcome::default());
        assert_eq!(state.last_cycle.status, CycleStatus::QuietSatisfied);
        assert_eq!(state.readiness(), Readiness::ReadyForHumanMerge);
    }

    #[test]
    fn actionable_or_failed_checks_reset_quiet_cycles() {
        let mut state = PrWatchState::new(PrTarget {
            repo: "owner/repo".into(),
            number: 1,
        });
        state.polling.quiet_cycles = 2;
        state.apply_cycle_outcome(CycleOutcome {
            pending_actionable: vec![ActionableItem {
                id: "t1".into(),
                surface: "review_threads".into(),
                summary: "fix it".into(),
                url: None,
                path: None,
                status: None,
            }],
            ..CycleOutcome::default()
        });
        assert_eq!(state.polling.quiet_cycles, 0);
        assert_eq!(state.last_cycle.status, CycleStatus::ActionRequired);
        assert_eq!(state.readiness(), Readiness::NotReadyActionRequired);

        state.pending_actionable.clear();
        state.polling.quiet_cycles = 2;
        state.apply_cycle_outcome(CycleOutcome {
            failed_check_count: 1,
            ..CycleOutcome::default()
        });
        assert_eq!(state.polling.quiet_cycles, 0);
        assert_eq!(state.last_cycle.status, CycleStatus::ChecksFailed);
    }

    #[test]
    fn snapshot_result_preserves_per_surface_failures() {
        let snapshot = PrSnapshotResult {
            metadata: Ok(PrMetadata {
                identity: PrIdentity {
                    repo: "owner/repo".into(),
                    number: 1,
                    url: None,
                    state: Some("OPEN".into()),
                    base_ref: None,
                    head_ref: None,
                    head_sha: None,
                    merge_state: None,
                    review_decision: None,
                },
                is_draft: Some(false),
            }),
            checks: Err(SurfaceError::transient("checks", "timeout")),
            review_threads: Ok(Vec::new()),
            review_comments: Ok(Vec::new()),
            issue_comments: Ok(Vec::new()),
            reviews: Ok(Vec::new()),
            timeline: Err(SurfaceError::permanent("timeline", "unsupported")),
        };
        assert!(snapshot.has_partial_failure());
        assert_eq!(
            snapshot.failed_surfaces(),
            vec!["checks".to_string(), "timeline".to_string()]
        );
    }

    #[test]
    fn parses_gh_pr_view_metadata_fixture() {
        let metadata = parse_gh_pr_view(
            "owner/repo",
            42,
            r#"{
              "url":"https://github.com/owner/repo/pull/42",
              "state":"OPEN",
              "baseRefName":"main",
              "headRefName":"feature/pr-watch",
              "headRefOid":"abc123",
              "mergeStateStatus":"CLEAN",
              "reviewDecision":"REVIEW_REQUIRED",
              "isDraft":false
            }"#,
        )
        .unwrap();
        assert_eq!(metadata.identity.repo, "owner/repo");
        assert_eq!(metadata.identity.number, 42);
        assert_eq!(metadata.identity.head_sha.as_deref(), Some("abc123"));
        assert_eq!(metadata.identity.merge_state.as_deref(), Some("CLEAN"));
        assert_eq!(metadata.is_draft, Some(false));
    }

    #[test]
    fn parses_paginated_gh_arrays_for_comments_and_checks() {
        let checks = parse_gh_checks(
            r#"[{"name":"ci","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"https://ci"}]
               [{"name":"lint","status":"IN_PROGRESS","conclusion":null}]"#,
        )
        .unwrap();
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "ci");
        assert_eq!(checks[0].conclusion.as_deref(), Some("SUCCESS"));
        assert_eq!(checks[1].status.as_deref(), Some("IN_PROGRESS"));

        let comments = parse_gh_review_comments(
            r#"[{"id":123,"path":"src/lib.rs","line":9,"html_url":"https://comment","updated_at":"2026-05-13T17:00:00Z","user":{"login":"reviewer"},"body":"Please fix"}]"#,
        )
        .unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].id, "123");
        assert_eq!(comments[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(comments[0].author.as_deref(), Some("reviewer"));
    }

    #[test]
    fn parses_gh_issue_comments_and_reviews() {
        let issue_comments = parse_gh_issue_comments(
            r#"[{"node_id":"IC_1","html_url":"https://issue-comment","updated_at":"2026-05-13T17:00:00Z","user":{"login":"alice"},"body":"Top level"}]"#,
        )
        .unwrap();
        assert_eq!(issue_comments[0].id, "IC_1");
        assert_eq!(issue_comments[0].author.as_deref(), Some("alice"));

        let reviews = parse_gh_reviews(
            r#"[{"id":55,"state":"CHANGES_REQUESTED","submitted_at":"2026-05-13T17:01:00Z","user":{"login":"bob"},"body":"Needs work"}]"#,
        )
        .unwrap();
        assert_eq!(reviews[0].id, "55");
        assert_eq!(reviews[0].state.as_deref(), Some("CHANGES_REQUESTED"));
        assert_eq!(reviews[0].author.as_deref(), Some("bob"));
    }

    #[test]
    fn parses_graphql_review_threads_fixture() {
        let threads = parse_gh_review_threads(
            r#"{
              "data": {"repository": {"pullRequest": {"reviewThreads": {"nodes": [
                {
                  "id":"PRRT_1",
                  "isResolved":false,
                  "isOutdated":false,
                  "comments":{"nodes":[{
                    "path":"src/main.rs",
                    "line":12,
                    "url":"https://thread-comment",
                    "updatedAt":"2026-05-13T17:02:00Z",
                    "author":{"login":"reviewer"},
                    "body":"Thread body"
                  }]}
                }
              ]}}}}
            }"#,
        )
        .unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "PRRT_1");
        assert!(!threads[0].is_resolved);
        assert_eq!(threads[0].path.as_deref(), Some("src/main.rs"));
        assert_eq!(threads[0].line, Some(12));
        assert_eq!(threads[0].author.as_deref(), Some("reviewer"));
    }

    #[test]
    fn parser_rejects_wrong_surface_shape() {
        let err = parse_gh_checks(r#"{"not":"array"}"#).unwrap_err();
        assert_eq!(err, GhParseError::ExpectedArray("checks"));
    }
}
