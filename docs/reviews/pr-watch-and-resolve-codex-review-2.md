approval_class: APPROVE

blockers:
- None.

major_findings:
- Revision 2 resolves the prior partial remote mutation blocker. It now distinguishes pre-mutation validation failure from post-mutation partial failure, requires per-thread persisted attempt evidence, returns non-success on partial failure, and mandates a post-mutation `poll_now` before retry.
- The separate remediation scheduled payload boundary is now explicit enough: `PrWatchSchedulePayload` remains read-only, `PrWatchRemediationPayload` has a distinct `pr_watch.watch_resolve` schedule kind, runtime grant rechecks, required scopes, and parser/test requirements.
- Root-dir hardening is correctly moved before `resolve_addressed` in the rollout as Slice 2, and Phase 2 also says mutating actions must resolve the watch state root before loading state.
- The audit model is now strong enough for GitHub review-thread resolution: every requested thread gets a recorded attempt, including skipped and failed entries, with head/commit evidence, validation, reason, API error, and post-poll state.
- The plan remains incremental and grounded in current code. It preserves the existing read-only monitor contract, adds `resolve_addressed` before broader orchestration, and defers the full `watch_resolve` executor.

minor_findings:
- The document still has “Phase 0” placed after Phase 5 in the architecture section. The rollout order is correct, but the architecture ordering should be cleaned up to avoid implementer confusion.
- The helper signature `run_gh_resolve_review_thread(...) -> Result<()>` is too lossy for the stated parse behavior. It should return a typed outcome that can distinguish resolved, already resolved, malformed response, and `isResolved=false`.
- Acceptance criteria say new fake-gh tests cover “resolution success and failure,” while the detailed test section is stronger. Mirror the stronger list in acceptance criteria so partial batch, malformed GraphQL, `isResolved=false`, and idempotent retry cannot be dropped later.

required_revisions:
- Non-blocking: move the “Phase 0: Working-directory hardening prerequisite” section before Phase 1 or rename it consistently with Slice 2.
- Non-blocking: specify a typed GitHub mutation result instead of `Result<()>`.
- Non-blocking: strengthen the final acceptance criteria to include the full fake-gh and payload-boundary test matrix already described earlier.

final_recommendation:
Approve revision 2 for implementation. The prior blockers are resolved; remaining revisions are cleanup and precision improvements, not reasons to hold the plan.
