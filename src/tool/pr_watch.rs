use super::{Tool, ToolContext, ToolOutput};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::Utc;
use jcode_pr_watch_core::{
    ActionableItem, CheckRunState, CycleOutcome, Marker, PrTarget, PrWatchState, SurfaceError,
    WatchEvent, normalize_watch_state_json, parse_gh_checks, parse_gh_issue_comments,
    parse_gh_pr_view, parse_gh_review_comments, parse_gh_review_threads, parse_gh_reviews,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    AckBaseline,
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
                    "enum": ["start", "status", "list", "poll_now", "ack_baseline", "stop", "readiness", "handoff"],
                    "description": "Action. poll_now performs read-only gh CLI collection and updates local state; no mutations are performed."
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
            PrWatchAction::PollNow => poll_now(&root, &store, params),
            PrWatchAction::AckBaseline => ack_baseline(&root, &store, params),
            PrWatchAction::Status | PrWatchAction::Readiness | PrWatchAction::Handoff => {
                status_like(&store, params)
            }
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
        "PR watch initialized: {}\nPath: {}\nMode: local state initialized. Use poll_now for read-only gh collection{}",
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

fn ack_baseline(root: &Path, store: &Path, params: PrWatchInput) -> Result<ToolOutput> {
    let mut state = load_state_for_params(store, &params)?;
    let path = state_path(store, &state.watch_id);
    let collected_at = now_iso();
    let collection = collect_with_gh(root, &state.pr.repo, state.pr.number);
    let partial_failure = apply_baseline_from_collection(&mut state, collection, &collected_at);
    let would_write = !params.dry_run.unwrap_or(false);
    if would_write {
        fs::write(&path, serde_json::to_vec_pretty(&state)?)?;
    }
    let text = format!(
        "PR watch baseline acknowledged: {}\nRepo: {}\nPR: #{}\nUnresolved threads: {}\nReview comments seen: {}\nIssue comments seen: {}\nReviews seen: {}\nPartial failure: {}{}",
        state.watch_id,
        state.pr.repo,
        state.pr.number,
        state.baseline.unresolved_thread_ids.len(),
        state.last_seen.review_comments.len(),
        state.last_seen.issue_comments.len(),
        state.last_seen.reviews.len(),
        partial_failure,
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

fn poll_now(root: &Path, store: &Path, params: PrWatchInput) -> Result<ToolOutput> {
    let mut state = load_state_for_params(store, &params)?;
    let path = state_path(store, &state.watch_id);
    let collected_at = now_iso();
    let result = collect_with_gh(root, &state.pr.repo, state.pr.number);
    let outcome = update_state_from_collection(&mut state, result, &collected_at);
    let would_write = !params.dry_run.unwrap_or(false);
    if would_write {
        fs::write(&path, serde_json::to_vec_pretty(&state)?)?;
    }
    let readiness = state.readiness();
    let text = format!(
        "PR watch polled: {}\nRepo: {}\nPR: #{}\nState: {:?}\nReadiness: {:?}\nQuiet cycles: {}/{}\nActionable: {}\nPending checks: {}\nFailed checks: {}\nPartial failure: {}\nFailed surfaces: {}{}",
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

fn collect_with_gh(root: &Path, repo: &str, pr: u64) -> GhCollection {
    GhCollection {
        metadata: run_gh(root, &["pr", "view", &pr.to_string(), "--repo", repo, "--json", "url,state,baseRefName,headRefName,headRefOid,mergeStateStatus,reviewDecision,isDraft"])
            .and_then(|stdout| parse_gh_pr_view(repo, pr, &stdout).map_err(|err| SurfaceError::transient("metadata", err.to_string()))),
        checks: run_gh(root, &["pr", "checks", &pr.to_string(), "--repo", repo, "--json", "name,status,conclusion,detailsUrl"])
            .and_then(|stdout| parse_gh_checks(&stdout).map_err(|err| SurfaceError::transient("checks", err.to_string()))),
        review_comments: run_gh(root, &["api", &format!("repos/{repo}/pulls/{pr}/comments"), "--paginate"])
            .and_then(|stdout| parse_gh_review_comments(&stdout).map_err(|err| SurfaceError::transient("review_comments", err.to_string()))),
        issue_comments: run_gh(root, &["api", &format!("repos/{repo}/issues/{pr}/comments"), "--paginate"])
            .and_then(|stdout| parse_gh_issue_comments(&stdout).map_err(|err| SurfaceError::transient("issue_comments", err.to_string()))),
        reviews: run_gh(root, &["api", &format!("repos/{repo}/pulls/{pr}/reviews"), "--paginate"])
            .and_then(|stdout| parse_gh_reviews(&stdout).map_err(|err| SurfaceError::transient("reviews", err.to_string()))),
        review_threads: run_gh_graphql_review_threads(root, repo, pr)
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

fn run_gh_graphql_review_threads(root: &Path, repo: &str, pr: u64) -> Result<String, SurfaceError> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| SurfaceError::permanent("review_threads", "repo must be owner/name"))?;
    let pr_s = pr.to_string();
    let query = r#"
query($owner:String!, $name:String!, $number:Int!) {
  repository(owner:$owner, name:$name) {
    pullRequest(number:$number) {
      reviewThreads(first:100) {
        nodes {
          id
          isResolved
          isOutdated
          comments(first:1) {
            nodes {
              path
              line
              url
              body
              createdAt
              updatedAt
              author { login }
            }
          }
        }
      }
    }
  }
}
"#;
    run_gh(
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
}

fn run_gh(root: &Path, args: &[&str]) -> Result<String, SurfaceError> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|err| SurfaceError::transient("gh", format!("failed to run gh: {err}")))?;
    if !output.status.success() {
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

    match collection.metadata {
        Ok(metadata) => {
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
            state
                .last_cycle
                .surface_counts
                .insert("review_comments".to_string(), comments.len());
            state
                .last_successful_fetch
                .insert("review_comments".to_string(), collected_at.to_string());
            for comment in comments {
                let is_new = !state.last_seen.review_comments.contains_key(&comment.id);
                state.last_seen.review_comments.insert(
                    comment.id.clone(),
                    Marker {
                        id: comment.id.clone(),
                        updated_at: comment.updated_at.clone(),
                        author: comment.author.clone(),
                        body_hash: comment.body.as_ref().map(|body| stable_body_hash(body)),
                        url: comment.url.clone(),
                    },
                );
                if is_new
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
                        status: Some("new".to_string()),
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
            state
                .last_cycle
                .surface_counts
                .insert("issue_comments".to_string(), comments.len());
            state
                .last_successful_fetch
                .insert("issue_comments".to_string(), collected_at.to_string());
            for comment in comments {
                let is_new = !state.last_seen.issue_comments.contains_key(&comment.id);
                state.last_seen.issue_comments.insert(
                    comment.id.clone(),
                    Marker {
                        id: comment.id.clone(),
                        updated_at: comment.updated_at.clone(),
                        author: comment.author.clone(),
                        body_hash: comment.body.as_ref().map(|body| stable_body_hash(body)),
                        url: comment.url.clone(),
                    },
                );
                if is_new
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
                        status: Some("new".to_string()),
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
                let is_new = !state.last_seen.review_threads.contains_key(&thread.id);
                state.last_seen.review_threads.insert(
                    thread.id.clone(),
                    jcode_pr_watch_core::ReviewThreadMarker {
                        id: thread.id.clone(),
                        updated_at: thread.updated_at.clone(),
                        resolved: thread.is_resolved,
                        outdated: thread.is_outdated,
                        body_hash: thread.body.as_ref().map(|body| stable_body_hash(body)),
                        url: thread.url.clone(),
                    },
                );
                if is_new && !thread.is_resolved && !thread.is_outdated {
                    pending_actionable.push(ActionableItem {
                        id: thread.id,
                        surface: "review_threads".to_string(),
                        summary: thread
                            .body
                            .unwrap_or_else(|| "Unresolved review thread".to_string()),
                        url: thread.url,
                        path: thread.path,
                        status: Some("unresolved".to_string()),
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
                    });
                }
            }
        }
        Err(err) => {
            partial_failure = true;
            state.push_event(surface_error_event(collected_at, err));
        }
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
        "FAILURE" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED"
    ) || matches!(status.as_str(), "FAILURE" | "ERROR" | "FAILED")
}

fn is_automation_chatter(author: Option<&str>, body: Option<&str>) -> bool {
    let author = author.unwrap_or_default().to_ascii_lowercase();
    let body = body.unwrap_or_default().to_ascii_lowercase();
    author.ends_with("[bot]")
        || matches!(
            author.as_str(),
            "github-actions" | "claude" | "codex" | "gemini-code-assist"
        )
        || body.starts_with("fix-summary:")
        || body.contains("triggered the review bot")
        || body.contains("automation progress")
}

fn stable_body_hash(body: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    format!("hash:{:016x}", hasher.finish())
}

fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn status_like(store: &Path, params: PrWatchInput) -> Result<ToolOutput> {
    let state = load_state_for_params(store, &params)?;
    let readiness = state.readiness();
    let text = format!(
        "PR watch: {}\nRepo: {}\nPR: #{}\nState: {:?}\nReadiness: {:?}\nQuiet cycles: {}/{}\nActionable: {}\nPending checks: {}\nFailed checks: {}\nPolicy: local_fix={}, commit={}, push={}, comment={}, resolve_threads={}",
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
        assert!(actions.iter().any(|value| value == "ack_baseline"));
        assert!(!actions.iter().any(|value| value == "authorize"));
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
        assert!(state.pending_actionable.is_empty());
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
        assert!(is_automation_chatter(
            Some("github-actions[bot]"),
            Some("Progress update")
        ));
        assert!(is_automation_chatter(
            Some("human"),
            Some("fix-summary: addressed feedback")
        ));
        assert!(!is_automation_chatter(
            Some("reviewer"),
            Some("Please fix this")
        ));
    }
}
