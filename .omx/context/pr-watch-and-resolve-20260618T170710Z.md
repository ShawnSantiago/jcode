# Context: PR watch-and-resolve mode

## Task statement
Create an implementation plan to improve `pr_watch` so it can support an autonomous review loop: watch PR feedback, remediate actionable comments/checks, push validated fixes when authorized, resolve addressed review threads, and continue watching for new feedback from Jules/Codex/Copilot/Gemini-style reviewers.

## Desired outcome
A reviewed and approved plan, not implementation code yet. The feature should eliminate stalls where the agent fixes feedback but fails to resolve the addressed GitHub review thread, requiring the user to remind it.

## Repo evidence
- Current branch: `master`.
- User/untracked files present and unrelated: `membership-desktop-starter-cad.png`, `membership-desktop-usd.png`, `membership-mobile-starter-cad.png`.
- Current implementation entrypoint: `crates/jcode-app-core/src/tool/pr_watch.rs`.
- Current state/model crate: `crates/jcode-pr-watch-core/src/lib.rs`.
- Existing docs: `docs/PR_FEEDBACK_WATCH_INTEGRATION_PLAN.md`, `docs/plans/pr-watch-dogfood-improvement-plan.md`, `docs/PR_WATCH_BOUNDED_MONITOR_DESIGN.md`.

## Key current behavior
- `PrWatchAction` supports `start`, `status`, `list`, `poll_now`, `monitor`, `ack_baseline`, `authorize`, `revoke`, `reschedule`, `stop`, `readiness`, and `handoff`.
- Tool description says no pushes, comments, thread resolution, or merges are performed.
- Scheduled `PrWatchSchedulePayload` requires `readonly: true` and accepts only `ack_baseline | poll_now | monitor`.
- `WriteScope` includes `ResolveThreads`, but it is only a grant scope today.
- `handoff_prompt` says no review-thread resolution without an active `resolve_threads` grant, but does not guarantee resolution.
- `maybe_schedule_action_required_handoff` schedules a high-priority handoff to the origin session when `pending_actionable` exists.
- State is repo-root relative under `.jcode/pr-feedback-watch`, which can fail when status/handoff is queried from a different working directory.

## Constraints
- Keep existing scheduled monitor read-only by default.
- Never merge PRs.
- Do not introduce remote GitHub mutations without explicit grant checks.
- Resolve only confidently addressed GitHub review threads, not every comment.
- Preserve user trust: missing grants should block with a clear message, not half-act.

## Unknowns / open questions
- Whether `gh api graphql` mutation should live directly in `pr_watch` or in a smaller `resolve_addressed` sub-action consumed by agents.
- How much of remediation should be tool-enforced versus prompt-orchestrated for MVP.
- How to represent addressed-thread evidence compactly in watch state.

## Likely touchpoints
- `crates/jcode-app-core/src/tool/pr_watch.rs`
- `crates/jcode-pr-watch-core/src/lib.rs`
- Tests embedded in `pr_watch.rs` and `jcode-pr-watch-core`.
- Possibly docs under `docs/plans` and PR-watch docs.
