# PR Watch Dogfood Improvement Plan

Date: 2026-06-10
Status: draft for Codex/Claude review
Owner: Jcode self-development
Scope: `crates/jcode-app-core/src/tool/pr_watch.rs`, `jcode_pr_watch_core` integration, tests, and user-facing workflow docs.

## 1. Session review summary

Recent dogfooding sessions show `pr_watch` is useful but still creates user friction in exactly the workflows it is meant to automate.

Evidence from session history:

1. Stale scheduled wakeups
   - PR #177 had `Next poll: 2026-06-08T20:04:11Z` while the user asked at 20:08 why watch was not working.
   - PR #166 had `Next poll: 2026-06-07T17:51:28Z` while status was checked at 19:32.
   - PR #150 had `Next poll: 2026-06-05T15:39:32Z` while status was checked at 16:12.
   - Current scheduling only schedules a visible task. It can be missed, and status does not clearly say the watch is overdue or offer a one-call recovery.

2. Policy mismatch with user intent
   - Multiple sessions show users asking whether watch is “checking and resolving”, then seeing `commit=false`, `push=false`, `comment=false` while `local_fix=true` and `resolve_threads=true`.
   - Users repeatedly asked to set `commit,push,comment` to true, and agents edited `.jcode/pr-feedback-watch/*-state.json` directly because the tool schema exposes no policy-granting action.
   - Tool description and scheduled prompts emphasize read-only behavior, while session/operator expectations include “watch and resolve comments”.

3. Bot/comment noise counted as actionable
   - Sessions for PR #108/#109 show “actionable” items that needed manual inspection to separate real feedback from Vercel deployment comments or other bot chatter.
   - This wastes cycles and prevents quiet-cycle progress.

4. Read-only monitor is safe but insufficiently actionable
   - `monitor` and scheduled prompts correctly avoid unauthorized pushes/comments/resolution.
   - However, `blocked_by_policy` currently tells the agent what cannot happen, not how to safely proceed. It lacks a first-class grant/handoff path.

5. Manual state mutation is an anti-pattern
   - Direct JSON edits bypass schema validation, audit trails, concurrency/stale-write checks, and future migration logic.
   - The tool should provide explicit, safe state transitions for grant/policy updates and stale schedule recovery.

## 2. Goals

1. Make stale watches self-diagnosing and one-call recoverable.
2. Replace manual policy JSON edits with a first-class authorization/grant action.
3. Reduce false actionable items from common automation/bot comments.
4. Clarify the distinction between read-only watch, authorized remediation, and merge gating.
5. Preserve strict safety boundaries: no auto-merge, no remote mutation without explicit user authorization, no authorization triggered by PR content.
6. Add tests that reproduce the observed dogfooding failures.

## 3. Non-goals

1. Do not implement automatic merging.
2. Do not silently default commit/push/comment to true.
3. Do not add a daemon or unbounded loop.
4. Do not make `pr_watch poll_now` mutate GitHub.
5. Do not delete or rewrite existing watch state without migration/read compatibility.

## 4. Proposed design

### 4.1 Add explicit watch modes

Introduce a user-visible mode concept in state and output:

- `read_only`: collect feedback, maintain state, schedule next polls, never mutate repo/GitHub.
- `local_fix_pending`: the watch has actionable items and the operator may choose to invoke a separate remediation workflow, but the watch itself remains read-only.
- `grant_available`: an explicit operator grant exists for a future remediation action. This is not permission for `poll_now`, `ack_baseline`, `monitor`, or scheduled follow-ups to mutate remotely.

Implementation detail: this can be derived from policy/grant fields initially, but display it as one consolidated mode to avoid confusing `local_fix=true, commit=false, push=false` combinations. Existing watch actions remain read-only.

### 4.2 Add a first-class authorization grant recorder

Add `pr_watch action="authorize"` as a grant-recording action, not as a mutation executor, with explicit fields:

- `scopes`: array enum: `local_fix`, `commit`, `push`, `comment`, `resolve_threads`
- `reason`: required string
- `expires_in_minutes`: optional, default 120, max 24h
- `single_use`: default false for a watch loop, true allowed
- `repo`, `pr`, `watch_id`

Also add `pr_watch action="revoke"` for immediate operator invalidation of a grant by `grant_id` or scope set.

Rules:

1. Authorization must come from the user/operator, not PR content.
2. Authorization is recorded in state as an envelope with timestamp, session id, scopes, expiry, reason, and grant id.
3. Expired or revoked grants are ignored in readiness, handoff, and remediation eligibility.
4. `merge` is not a valid scope.
5. Existing boolean policy fields remain read-compatible but are not manually edited. To avoid the legacy `base_policy && active_grant` trap, effective remote mutation policy is computed from active grant scopes plus hardcoded tool safety rules, while legacy booleans are displayed as compatibility data only.
6. `poll_now`, `ack_baseline`, `monitor`, scheduled follow-ups, `status`, `readiness`, and `handoff` must never push, comment, resolve threads, or merge, even when an active grant exists.
7. Handoff output should surface recent grant lifecycle events: created, expired, consumed, and revoked.
8. Document that grants do not make scheduled monitor cycles mutating; expiry is primarily a guardrail for the separate remediation workflow.

A later explicit remediation action/workflow may consume the grant, but that action is outside the read-only watch loop and must be invoked deliberately.

### 4.3 Add stale schedule detection and recovery

Enhance `status`, `readiness`, and `poll_now` output:

- Compute `schedule_overdue_by_seconds` when `polling.next_poll_at < now` and watch is nonterminal.
- Show a clear line: `Schedule: overdue by Xm; run pr_watch action="monitor" schedule_next=true to recover`.
- Add `pr_watch action="reschedule"` or extend `monitor schedule_next=true` to explicitly cancel stale due items and schedule a fresh structured monitor.
- Prefer structured monitor scheduling over prose poll scheduling for unattended watch loops.

### 4.4 Keep scheduled follow-ups read-only and mode-aware

Current scheduled prompts already say read-only. Preserve that invariant while making prompts more useful:

- Read-only watches schedule `monitor` with no mutation permissions.
- Watches with active grants still schedule read-only `monitor`; the prompt may mention that a grant exists for a separate explicit remediation command, but it must also say the scheduled watch must not mutate remotely.
- Prompt includes state path, watch id, repo, PR, current mode, and grant expiration.
- Prompt explicitly forbids push, comment, resolve threads, and merge in all scheduled watch modes.

### 4.5 Improve automation noise filtering

Add classifier rules and tests for common non-actionable automation comments:

- Vercel deployment/status comments
- GitHub Actions summary comments with no requested code change
- “Jules reporting for duty”/assistant handshake comments
- Generic deployment URLs and preview comment bots

The classifier must preserve real bot feedback when it contains severity markers, requested changes, file/line comments, failed check output, or review-thread context.

### 4.6 Add an operator-friendly recovery/handoff report

Enhance `handoff` and `readiness` output with:

- current mode and active/expired grants
- stale/next schedule status
- exact actionable count by surface after filtering
- safe next command suggestions:
  - `poll_now` for immediate read-only collection
  - `monitor schedule_next=true` for unattended read-only watching
  - `authorize scopes=[...]` to record an explicit grant for a future remediation action
  - an explicit remediation workflow/action, once implemented, for commit/push/comment/resolve behavior
  - never suggest merge as an automated action

## 5. Implementation slices

### Slice A: Status clarity and stale schedule diagnostics

Files likely affected:

- `crates/jcode-app-core/src/tool/pr_watch.rs`
- `jcode_pr_watch_core` readiness/status helpers, if applicable
- unit tests near existing pr_watch tests

Tasks:

1. Add overdue schedule computation from `polling.next_poll_at`.
2. Display stale schedule in `status_like`, `readiness_report`, and metadata.
3. Add tests for future, absent, terminal, and overdue schedule cases.
4. Do not change mutation semantics.

Validation:

- Targeted cargo test for pr_watch status/readiness.
- Selfdev TUI build.

### Slice B: First-class authorization envelope recorder

Tasks:

1. Add `Authorize` action and input fields.
2. Add `Revoke` action for explicit grant invalidation.
3. Add state serialization for authorization grants while preserving old state read compatibility.
4. Derive remediation eligibility from active grant scopes and expiry, but keep all existing watch actions read-only.
5. Update status/readiness/handoff output to show active/expired/revoked grants, recent grant lifecycle events, and explicit remediation next steps.
6. Replace documented/manual JSON-edit workflow with `pr_watch authorize` plus a separate remediation invocation.

Validation:

- Tests for grant creation, expiry, revocation, remediation eligibility, invalid merge scope, and legacy boolean compatibility.
- Tests proving `monitor`, `poll_now`, `ack_baseline`, and scheduled prompts remain read-only even with an active grant.

### Slice C: Read-only mode-aware scheduling and reschedule recovery

Tasks:

1. Add `reschedule` action or equivalent helper.
2. Schedule structured `monitor` cycles by default for unattended follow-up, not prose-only `poll_now` instructions.
3. Ensure stale scheduled items are cancelled and replaced safely.
4. Define `reschedule` as idempotent, watch-id scoped, concurrency-safe under the existing lock/state stale-write checks, and auditable through state events. The implementation PR must explicitly cite the lock/CAS mechanism it relies on.
5. Preserve `target=resume|spawn` behavior.
6. Ensure scheduled prompts never instruct push/comment/thread resolution/merge.

Validation:

- Tests for duplicate scheduled item detection, stale cancellation, mode-aware prompt content, and read-only prompt invariants.
- Manual dry-run with a temp watch state.

### Slice D: Automation noise filtering

Tasks:

1. Add/extend classifier for Vercel and assistant handshake noise.
2. Keep real review feedback actionable even from bots.
3. Add fixture-driven tests from observed sessions, including the PR #108/#109 Vercel/deployment-only comments and assistant handshake bodies that previously inflated actionable counts.

Validation:

- Unit tests with representative comment bodies.
- Confirm actionable counts exclude deployment-only comments.

### Slice E: Docs and migration notes

Tasks:

1. Update `docs/PR_WATCH_BOUNDED_MONITOR_DESIGN.md` with authorization/recovery addendum.
2. Add a short operator runbook section: “watch”, “authorize remediation”, “recover stale watch”.
3. Mention that direct `.jcode/pr-feedback-watch/*-state.json` edits are deprecated except emergency debugging.
4. Add a follow-up note to deprecate legacy boolean policy fields after the grant model has soaked and downstream state files have migrated.

Validation:

- Docs reviewed for consistency with tool schema.

## 6. Risks and mitigations

1. Risk: Authorization grants make remote mutation look too easy.
   - Mitigation: grants only record operator intent, existing watch actions remain read-only, explicit remediation invocation is required, expiry is mandatory, scope list excludes merge, and status surfaces the distinction.

2. Risk: Bot filtering hides real feedback.
   - Mitigation: conservative classifier with severity/request markers preserving actionability.

3. Risk: Scheduling changes regress existing read-only workflows.
   - Mitigation: keep `poll_now`/`ack_baseline` compatible, add structured monitor as preferred path, tests for existing outputs.

4. Risk: State schema migration breaks existing project watches.
   - Mitigation: read-compatible optional fields and fallback to legacy booleans.

## 7. Acceptance criteria

1. A stale watch status clearly reports overdue state and exact recovery command.
2. A user can grant commit/push/comment/resolve behavior without manual JSON edits.
3. `pr_watch` never auto-merges and `poll_now`, `ack_baseline`, `monitor`, and scheduled follow-ups never perform remote mutation, even with an active grant.
4. A separate, deliberately invoked remediation path can consume an active grant for commit/push/comment/resolve behavior without manual JSON edits. Ordinary `pr_watch` watch actions remain read-only.
5. Deployment-only bot comments are not counted as actionable.
6. Existing read-only `start`, `poll_now`, `ack_baseline`, `monitor`, `status`, `readiness`, `handoff`, and `stop` flows remain compatible.
7. Targeted tests and selfdev TUI build pass.

## 8. Suggested implementation order

1. Slice A first because it is low-risk and improves observability immediately.
2. Slice B next because it removes manual JSON edits and resolves the biggest operator pain.
3. Slice C after authorization so scheduled prompts can be mode-aware.
4. Slice D can run in parallel after classifier locations are identified.
5. Slice E after code behavior stabilizes.
