use chrono::{DateTime, Utc};

use super::{
    OvernightManifest, OvernightPreflight, OvernightRunStatus, format_minutes, preflight_summary,
};

const ANTI_STALL_CONTRACT: &str = r#"Anti-stall defaults learned from prior overnight runs:
- Treat the target wake time as the reporting/handoff time, not permission to stop early. If time remains and no hard blocker exists, continue with the next bounded, verified slice.
- Every health checkpoint must include: manifest/status, cancellation state, git branch/status for each touched repo, active worker/session ids, open PR numbers and head SHAs, next scheduled watchdog time, next PR poll time, current blocker or next backlog, and latest validation evidence.
- Avoid rapid duplicate auto-pokes. If a watchdog wakes before a PR poll/quiet-cycle is due, record a lightweight checkpoint and keep the scheduled cadence instead of burning quota with duplicate polls.
- Recover stale PR watches. Compare next_poll/final-gate times with current UTC; if overdue, run a read-only poll, verify GitHub source-of-truth state, then reschedule.
- Verify PR gates from GitHub directly before quiet-cycle, merge-ready, or auto-merge decisions: head SHA, draft/open/merged state, checks, top-level comments, inline review threads, reviews, and mergeability.
- After any push to a PR, reset quiet cycles from the new head SHA and record the new head plus the next poll time.
- If no worker is active and no PR is waiting in protocol, immediately score/select the next safe bounded backlog instead of waiting for the user.
- Before changing repos or worktrees, snapshot `git status --short --branch`, `git worktree list` when relevant, and avoid destructive cleanup unless explicitly approved.
- Keep environment/setup blockers explicit: missing credentials, `.env` paths, services, or ports should be recorded with the exact non-secret key/path and a safe alternate task should be chosen.
"#;

pub(crate) fn overnight_phase(manifest: &OvernightManifest, now: DateTime<Utc>) -> &'static str {
    match manifest.status {
        OvernightRunStatus::Completed => "completed",
        OvernightRunStatus::Failed => "failed",
        OvernightRunStatus::CancelRequested => "cancelling",
        OvernightRunStatus::Running => {
            if now < manifest.handoff_ready_at {
                "running"
            } else if now < manifest.target_wake_at {
                "wind-down"
            } else if manifest.morning_report_posted_at.is_none() {
                "morning report"
            } else if now < manifest.post_wake_grace_until {
                "post-wake"
            } else {
                "finalizing"
            }
        }
    }
}

pub(crate) fn time_relation_to_target(manifest: &OvernightManifest, now: DateTime<Utc>) -> String {
    let minutes = manifest
        .target_wake_at
        .signed_duration_since(now)
        .num_minutes();
    if minutes >= 0 {
        format!("target in {}", format_minutes(minutes as u32))
    } else {
        format!("target passed {} ago", format_minutes((-minutes) as u32))
    }
}

pub(crate) fn relative_time(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = now.signed_duration_since(then).num_minutes();
    if minutes >= 0 {
        format!("{} ago", format_minutes(minutes as u32))
    } else {
        format!("in {}", format_minutes((-minutes) as u32))
    }
}

pub(crate) fn next_prompt_label(manifest: &OvernightManifest, now: DateTime<Utc>) -> String {
    if !matches!(manifest.status, OvernightRunStatus::Running) {
        return "none".to_string();
    }
    if now < manifest.handoff_ready_at {
        return format!(
            "handoff mode in {} or after current turn",
            format_minutes(
                manifest
                    .handoff_ready_at
                    .signed_duration_since(now)
                    .num_minutes()
                    .max(0) as u32
            )
        );
    }
    if now < manifest.target_wake_at {
        return format!(
            "morning report in {} or after current turn",
            format_minutes(
                manifest
                    .target_wake_at
                    .signed_duration_since(now)
                    .num_minutes()
                    .max(0) as u32
            )
        );
    }
    if manifest.morning_report_posted_at.is_none() {
        return "morning report after current turn".to_string();
    }
    if now < manifest.post_wake_grace_until {
        return format!(
            "final wrap by {} or after current turn",
            manifest.post_wake_grace_until.format("%H:%M UTC")
        );
    }
    "final wrap after current turn".to_string()
}

pub fn build_coordinator_prompt(
    manifest: &OvernightManifest,
    preflight: &OvernightPreflight,
) -> String {
    let mission = manifest
        .mission
        .as_deref()
        .unwrap_or("Continue the current session's highest-value work, prioritizing verified, low-risk progress.");
    format!(
        r#"You are the Overnight Coordinator for Jcode run `{run_id}`.

The user expects to be away until approximately `{target_wake_at}`. This is a target wake/report time, not a hard stop. By that time, the run must be handoff-ready and the review page must explain what happened. You may continue past the target only to finish a bounded, safe, verifiable chunk. The default soft post-wake grace window ends at `{post_wake_grace_until}`.

Mission:
{mission}

Operating contract:
- Optimize for verified, low-risk progress.
- Prefer GH bug issues with objective reproduction, failing tests, static-analysis findings, regression tests, bounded code-quality fixes, and clear crash/panic/wrong-output bugs.
- Avoid taste-based work, vague product decisions, broad rewrites, risky migrations, payments, sending email, pushing to remotes, deleting data, or other external side effects unless explicitly allowed by the user.
- If a bug is found, reproduce/prove it before fixing it.
- Only fix issues that are important, bounded, and verifiable. Otherwise draft a high-quality issue in `{issue_drafts}`.
- You own the run. Spawn swarm/helper agents only if the expected value exceeds usage/resource cost. Default to one coordinator plus at most one helper. Read-only scouts/verifiers are preferred over multiple editors.
- Be aware of RAM/load/battery, especially around compiles, browser automation, indexing, and full test suites. Do not run multiple heavy activities at once unless resources are clearly healthy.
- Do not wait for the user. If you need user judgment/credentials/taste, record it and switch to another useful task.
- Continue finding useful verified work until the target wake/report time unless usage/resources make that unreasonable.

Review/log requirements:
- Keep `{review_notes}` updated as you work.
- For each meaningful task, maintain one structured JSON task card in `{task_cards}` using the schema in `{task_card_schema}`. These cards drive the live TUI progress card and the generated review page.
- Each task card must include clear Before/After, evidence, validation, files changed, risk, status, and outcome. Keep the current task marked `active`, completed verified work marked `completed`, user/taste/credential stalls marked `blocked`, and considered-but-not-pursued work marked `deferred` or `skipped`.
- Put reproduction/test/command outputs in `{validation}` when useful.
- The generated review page is `{review_html}` and will be regenerated from logs plus your review notes.

{anti_stall_contract}

Preflight summary:
{preflight_summary}

Initial steps:
1. Inspect current repo/session state and git status.
2. Build a ranked queue of verifiable candidate tasks.
3. Pick the highest-confidence bounded task.
4. Prove/reproduce before fixing.
5. Validate and update review notes.
6. If done early, repeat discovery and continue.
"#,
        run_id = manifest.run_id,
        target_wake_at = manifest.target_wake_at.to_rfc3339(),
        post_wake_grace_until = manifest.post_wake_grace_until.to_rfc3339(),
        mission = mission,
        issue_drafts = manifest.issue_drafts_dir.display(),
        review_notes = manifest.review_notes_path.display(),
        task_cards = manifest.task_cards_dir.display(),
        task_card_schema = manifest
            .task_cards_dir
            .join("task-card-schema.md")
            .display(),
        validation = manifest.validation_dir.display(),
        review_html = manifest.review_path.display(),
        preflight_summary = preflight_summary(preflight),
        anti_stall_contract = ANTI_STALL_CONTRACT,
    )
}

pub fn build_visible_current_session_prompt(manifest: &OvernightManifest) -> String {
    let mission = manifest
        .mission
        .as_deref()
        .unwrap_or("Continue the current session's highest-value work, prioritizing verified, low-risk progress.");
    format!(
        r#"You are now the visible Overnight Coordinator for Jcode run `{run_id}`.

The user expects this current session to become the overnight session. Keep all work visible here: your normal tool calls, any spawned/swarm helper agents, their reports, and validation should be observable from this session like a normal interactive run.

Important: because this is the visible current-session mode, there is no separate hidden supervisor loop running additional turns for you. You must self-manage the overnight lifecycle from this visible turn: check the target wake time yourself, post a morning report when it is reached, avoid continuing past the grace window except for a bounded safe wrap-up, and check the manifest for cancellation before starting each major new task.

Target wake/report time: `{target_wake_at}`
Soft post-wake grace window ends: `{post_wake_grace_until}`

Mission:
{mission}

Operating contract:
- Do not wait for the user. If you need user judgment/credentials/taste, record it and switch to another useful task.
- Optimize for verified, low-risk progress. Prefer objective bugs, repros, regression tests, bounded quality fixes, and clear validation.
- Avoid broad rewrites, taste-based decisions, risky migrations, payments, sending email, pushing to remotes, deleting data, or external side effects unless explicitly allowed.
- Spawn helper/swarm agents only when valuable, and keep their work headed/visible from this session. Prefer read-only scouts/verifiers over many editors.
- Watch RAM/load/battery and avoid concurrent heavy builds or tests unless resources are clearly healthy.

Review/log requirements:
- Keep `{review_notes}` updated as you work.
- For each meaningful task, maintain one task-card JSON in `{task_cards}` using `{task_card_schema}`.
- Task cards should include Before/After, evidence, validation, files changed, risk, status, and outcome.
- Put useful command outputs in `{validation}`.
- The generated review page is `{review_html}`.
- Manifest path: `{manifest_path}`. If cancellation is requested or the run completes, update the manifest/status consistently when safe.

{anti_stall_contract}

Initial steps:
1. Inspect current repo/session state, including git status and current todos.
2. Build a ranked queue of verifiable candidate tasks.
3. Pick the highest-confidence bounded task.
4. Prove/reproduce before fixing.
5. Validate, update review notes/task cards, and continue with the next bounded task until the target wake/report time.
"#,
        run_id = manifest.run_id,
        target_wake_at = manifest.target_wake_at.to_rfc3339(),
        post_wake_grace_until = manifest.post_wake_grace_until.to_rfc3339(),
        mission = mission,
        review_notes = manifest.review_notes_path.display(),
        task_cards = manifest.task_cards_dir.display(),
        task_card_schema = manifest
            .task_cards_dir
            .join("task-card-schema.md")
            .display(),
        validation = manifest.validation_dir.display(),
        review_html = manifest.review_path.display(),
        manifest_path = manifest.run_dir.join("manifest.json").display(),
        anti_stall_contract = ANTI_STALL_CONTRACT,
    )
}

pub fn build_continuation_prompt(manifest: &OvernightManifest) -> String {
    let remaining = manifest
        .target_wake_at
        .signed_duration_since(Utc::now())
        .num_minutes()
        .max(0) as u32;
    format!(
        "Overnight continuation: there is about {} remaining until the target wake/report time. If your current task is complete, run another discovery/scoring pass and choose another high-confidence, verifiable task. If you are stuck, record why in `{}` and the relevant task-card JSON, then switch to a smaller bounded task. Enforce stale PR-watch recovery, avoid duplicate pre-due polls, verify PR gates from GitHub source-of-truth, snapshot git/worktree state before repo changes, and update review notes/task cards before continuing.",
        format_minutes(remaining),
        manifest.review_notes_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        GitSnapshot, OVERNIGHT_VERSION, OvernightManifest, OvernightRunStatus,
        ResourceSnapshot, UsageProjection,
    };
    use chrono::Utc;
    use std::path::PathBuf;

    fn test_manifest() -> OvernightManifest {
        let now = Utc::now();
        let run_dir = PathBuf::from("/tmp/overnight-run");
        OvernightManifest {
            version: OVERNIGHT_VERSION,
            run_id: "run-1".to_string(),
            parent_session_id: "parent".to_string(),
            coordinator_session_id: "coord".to_string(),
            coordinator_session_name: "coordinator".to_string(),
            started_at: now,
            target_wake_at: now + chrono::Duration::hours(8),
            handoff_ready_at: now + chrono::Duration::hours(7),
            post_wake_grace_until: now + chrono::Duration::hours(10),
            morning_report_posted_at: None,
            completed_at: None,
            cancel_requested_at: None,
            status: OvernightRunStatus::Running,
            mission: Some("ship safe slices".to_string()),
            working_dir: Some("/tmp/project".to_string()),
            provider_name: "provider".to_string(),
            model: "model".to_string(),
            max_agents_guidance: 1,
            process_id: 123,
            run_dir: run_dir.clone(),
            events_path: run_dir.join("events.jsonl"),
            human_log_path: run_dir.join("run.log"),
            review_path: run_dir.join("review.html"),
            review_notes_path: run_dir.join("review-notes.md"),
            preflight_path: run_dir.join("preflight.json"),
            task_cards_dir: run_dir.join("task-cards"),
            issue_drafts_dir: run_dir.join("issue-drafts"),
            validation_dir: run_dir.join("validation"),
            last_activity_at: now,
        }
    }

    fn test_preflight() -> OvernightPreflight {
        let now = Utc::now();
        OvernightPreflight {
            captured_at: now,
            usage: UsageProjection {
                captured_at: now,
                risk: "low".to_string(),
                confidence: "medium".to_string(),
                projected_delta_min_percent: None,
                projected_delta_max_percent: None,
                projected_end_min_percent: None,
                projected_end_max_percent: None,
                providers: Vec::new(),
                notes: Vec::new(),
            },
            resources: ResourceSnapshot {
                captured_at: now,
                ..Default::default()
            },
            git: GitSnapshot {
                captured_at: now,
                branch: Some("main".to_string()),
                dirty_count: Some(0),
                dirty_summary: Vec::new(),
                error: None,
            },
        }
    }

    #[test]
    fn coordinator_prompt_includes_anti_stall_defaults() {
        let manifest = test_manifest();
        let preflight = test_preflight();
        let prompt = build_coordinator_prompt(&manifest, &preflight);

        assert!(prompt.contains("Anti-stall defaults learned from prior overnight runs"));
        assert!(prompt.contains("Recover stale PR watches"));
        assert!(prompt.contains("Verify PR gates from GitHub directly"));
        assert!(prompt.contains("git worktree list"));
    }

    #[test]
    fn visible_and_continuation_prompts_include_operational_guards() {
        let manifest = test_manifest();
        let visible = build_visible_current_session_prompt(&manifest);
        let continuation = build_continuation_prompt(&manifest);

        assert!(visible.contains("Anti-stall defaults learned from prior overnight runs"));
        assert!(continuation.contains("stale PR-watch recovery"));
        assert!(continuation.contains("GitHub source-of-truth"));
        assert!(continuation.contains("snapshot git/worktree state"));
    }
}

pub fn build_handoff_ready_prompt(manifest: &OvernightManifest) -> String {
    format!(
        "Handoff-ready reminder: target wake/report time is in about 30 minutes. Do not abandon useful work, but make the run easy to understand. Update `{}` and task-card JSON with current task, completed work, validation state, files changed, risks, skipped work, and next steps. Avoid starting large/risky new changes unless they are isolated and clearly verifiable.",
        manifest.review_notes_path.display()
    )
}

pub fn build_morning_report_prompt(manifest: &OvernightManifest) -> String {
    format!(
        "Target wake/report time reached. Post a morning report now, even if work is still ongoing. Update `{}` plus task-card JSON and make sure `{}` is useful. Include completed work, current task, before/after evidence, files changed, validation, risks, usage/resource notes if relevant, and whether you plan to continue. You may continue only if the next chunk is bounded, safe, and verifiable.",
        manifest.review_notes_path.display(),
        manifest.review_path.display()
    )
}

pub fn build_post_wake_continuation_prompt(manifest: &OvernightManifest) -> String {
    format!(
        "Post-wake continuation: the target wake/report time has passed and the morning report should already be available. You may continue only with bounded, safe, verifiable work that is already in progress or clearly high-value. Do not start broad/risky new changes. Keep `{}` and task-card JSON current so the user can safely inspect or interrupt at any time. Soft grace window ends at `{}`.",
        manifest.review_notes_path.display(),
        manifest.post_wake_grace_until.to_rfc3339()
    )
}

pub fn build_final_wrapup_prompt(manifest: &OvernightManifest) -> String {
    format!(
        "Final overnight wrap-up: the post-wake grace window has expired. Stop starting new work. Finish only immediate cleanup, update `{}`, task-card JSON, and `{}` with final before/after evidence, validation status, dirty repo state, remaining risks, and next steps, then stop.",
        manifest.review_notes_path.display(),
        manifest.review_path.display()
    )
}

pub fn prompt_event_summary(prompt: &str) -> String {
    if prompt.starts_with("You are the Overnight Coordinator") {
        "Sending initial overnight coordinator mission".to_string()
    } else if prompt.starts_with("Handoff-ready") {
        "Sending handoff-ready poke".to_string()
    } else if prompt.starts_with("Target wake") {
        "Sending morning report poke".to_string()
    } else if prompt.starts_with("Post-wake continuation") {
        "Sending post-wake continuation poke".to_string()
    } else if prompt.starts_with("Final overnight wrap-up") {
        "Sending final wrap-up poke".to_string()
    } else {
        "Sending continuation poke".to_string()
    }
}
