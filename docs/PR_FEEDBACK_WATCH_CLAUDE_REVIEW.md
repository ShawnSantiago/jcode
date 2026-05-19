# Claude Review: PR Feedback Watch Integration Plan

Date: 2026-05-13
Reviewed document: `docs/PR_FEEDBACK_WATCH_INTEGRATION_PLAN.md`
Reviewer model: Claude Sonnet 4.6 via Jcode subagent
Status: review completed, blocker/major findings incorporated into the plan

## Summary

Claude reviewed the initial plan as a CTO/architect and found the idea sound but identified foundational issues that needed resolution before implementation. The amended plan now addresses the blocker and major issues by tightening authorization, scheduler durability, state consistency, schema semantics, per-surface collector errors, phase order, and v1 compatibility.

## Blockers identified

### B-1: Write authorization was dangerously under-specified

Original issue:

- `authorize` existed as a tool action without scope, expiry, session binding, restart behavior, or revocation semantics.
- The state schema included a persisted `merge` policy field, conflicting with the non-goal of never auto-merging.

Disposition:

- Replaced bare write authorization with an authorization envelope.
- Added required fields: grant ID, grant timestamp, expiry, session ID, scopes, single-use flag, and reason.
- Remote mutation grants expire, are single-use by default, and must not silently survive restart.
- Removed `merge` from policy schema and declared it never grantable.
- Added untrusted-input rule: PR/review content cannot trigger authorization.

### B-2: Scheduler integration did not map to actual Jcode behavior

Original issue:

- The plan said to use Jcode scheduling but did not choose between ambient scheduling and session wakeups.
- Ambient scheduling can be disabled, while session delivery can fail if the session closes.

Disposition:

- Chose the user-visible `schedule`/wakeup path for the initial implementation.
- Added a durability contract for Jcode running, Jcode not running, closed original sessions, and missed wakeups.
- Made the state file's `polling.next_poll_at` authoritative over scheduler entries.

### B-3: Atomic writes and index consistency were race-prone

Original issue:

- The plan introduced `index.json` without explaining transaction semantics.
- Concurrent poll cycles could race and corrupt or stale-write state.

Disposition:

- Declared per-PR state files the source of truth.
- Made `index.json` reconstructable cache only.
- Added stale-write detection using `updated_at` and `polling.cycle_number`.
- Required deduplication of overlapping scheduled cycles.
- Directed implementation to reuse Jcode's existing JSON storage/backup helpers.

## Major findings identified

### M-1: `last_seen` marker semantics were undefined

Disposition:

- Added typed marker rules for comments, reviews, timeline events, and review threads.
- Required `id`, `updated_at`, `author`, `body_hash`, and URL for comment-like markers.
- Required `resolved` and `outdated` for review-thread markers.

### M-2: Baseline and `last_seen` interaction was under-specified

Disposition:

- Added baseline update rules for baseline establishment, expected head changes, unexpected head changes, historical marker preservation, and baseline events.

### M-3: `events` array lacked schema and bounds

Disposition:

- Defined events as `{ at, kind, data }`.
- Bounded event history to newest 50 entries.
- Clarified `last_cycle` is canonical latest state, while `events` are audit breadcrumbs.

### M-4: Collector trait collapsed partial failures

Disposition:

- Replaced single `Result<PrSnapshot, PrWatchError>` with `PrSnapshotResult` containing per-surface `Result` values.
- Preserves partial successes and prevents partial fetches from being treated as quiet.

### M-5: Remediation came before UI/status visibility

Disposition:

- Swapped phase order.
- Minimal terminal/side-panel visibility now ships before autonomous remediation.

### M-6: Migration was too late

Disposition:

- Moved v1 read-compatibility into Phase 1.
- Phase 7 is now bulk migration/cleanup only.

## Minor and suggestion disposition

- `authorize` remains listed, but only after the authorization-envelope design is implemented.
- `next_poll_at` is authoritative; `final_poll_due_at` is metadata and must match `next_poll_at` when final poll is scheduled.
- `not_ready_validation_stale` is scoped to local-fix/remediation modes where validation is recorded or required.
- Checks moved out of `last_seen` into `last_checks_for_sha`.
- Tool examples use seconds, not mixed minute/second units.
- State-store section now warns about `.jcode/pr-feedback-watch/` gitignore behavior.
- Handoff example no longer hard-codes `--squash`; it presents merge strategy as a human choice.
- Open questions were reduced to unresolved implementation choices only.

## Remaining open questions after incorporation

1. Start with `gh` CLI or add direct GitHub API immediately? Current decision: `gh` first, direct API later.
2. Normal tool or background-task subtype? Current decision: normal tool first, designed to become a background-task subtype later.
3. Mechanical Python port vs. typed Rust redesign depth.
4. Whether readiness reports should include project memory-bank context.
5. Whether ignored automation authors should be global config, project config, or both.

## Review verdict

After the amendments, the plan is suitable to use as the basis for Phase 1 design work. The most important condition is that Phase 1 must include v1 read-compatibility, authorization-envelope data types, bounded events, typed markers, baseline rules, and per-surface collector result types before any networked GitHub collector is implemented.
