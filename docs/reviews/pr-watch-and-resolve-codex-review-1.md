approval_class: ITERATE

blockers:
- Remote mutation partial-success semantics are not tight enough. The plan says “No partial remote mutation,” but then allows sequential multi-thread resolution where GitHub may resolve some threads before one fails. That can be acceptable, but the plan must define exact behavior: when state is persisted, how successes/failures are returned, whether already-resolved successes are idempotently skipped on retry, and that re-poll is mandatory after any partial batch. Evidence: [docs/plans/pr-watch-and-resolve-plan.md](/home/shawn/business-projects/jcode/docs/plans/pr-watch-and-resolve-plan.md:216).
- The mutating scheduled remediation path is under-specified relative to the existing read-only monitor contract. Current structured scheduled payloads explicitly reject `readonly=false` and non-read-only actions, while handoff is a separate payload path. A new `PrWatchRemediationPayload` needs a distinct schedule kind, parser, executor, audit fields, and tests proving it cannot be mistaken for `monitor`/`poll_now` scheduled work. Evidence: [pr_watch.rs](/home/shawn/business-projects/jcode/crates/jcode-app-core/src/tool/pr_watch.rs:154), [docs/plans/pr-watch-and-resolve-plan.md](/home/shawn/business-projects/jcode/docs/plans/pr-watch-and-resolve-plan.md:192).

major_findings:
- The plan mostly preserves the read-only monitor contract. The current code enforces `ack_baseline | poll_now | monitor` only for scheduled read-only payloads, and prompts explicitly say not to push/comment/resolve/merge. Keep that invariant as a non-regression gate. Evidence: [pr_watch.rs](/home/shawn/business-projects/jcode/crates/jcode-app-core/src/tool/pr_watch.rs:161), [pr_watch.rs](/home/shawn/business-projects/jcode/crates/jcode-app-core/src/tool/pr_watch.rs:1379).
- GitHub thread resolution is grant-gated in concept and grounded in existing scopes, but auditability needs stronger acceptance criteria. `ResolveThreads` and session/expiry-bound grants already exist, so the plan fits the codebase. However, it should require recording attempted, resolved, skipped, and failed thread IDs, not just final success evidence. Evidence: [lib.rs](/home/shawn/business-projects/jcode/crates/jcode-pr-watch-core/src/lib.rs:52), [lib.rs](/home/shawn/business-projects/jcode/crates/jcode-pr-watch-core/src/lib.rs:92).
- Root-directory hardening should move before or into `resolve_addressed`, not Slice 4. Phase 2 says “load watch state from the correct store,” but current loading is strictly based on `ctx.working_dir/.jcode/pr-feedback-watch`; resolving from the wrong repo would fail before producing the planned diagnostic. Evidence: [pr_watch.rs](/home/shawn/business-projects/jcode/crates/jcode-app-core/src/tool/pr_watch.rs:249), [pr_watch.rs](/home/shawn/business-projects/jcode/crates/jcode-app-core/src/tool/pr_watch.rs:3064).
- The plan avoids a giant autonomous code-edit engine. Option C is deferred, and Slice 2 keeps remediation agent-driven with a concrete tool primitive. That is the right risk boundary. Evidence: [docs/plans/pr-watch-and-resolve-plan.md](/home/shawn/business-projects/jcode/docs/plans/pr-watch-and-resolve-plan.md:77), [docs/plans/pr-watch-and-resolve-plan.md](/home/shawn/business-projects/jcode/docs/plans/pr-watch-and-resolve-plan.md:276).

minor_findings:
- `commit_sha` is optional, but the safety model says resolution happens after a pushed fix is recorded. Make the allowed exception explicit, or require `commit_sha`/current PR `head_sha` for code-change resolutions.
- Tests mention fake `gh` success/failure, but should also cover malformed GraphQL success, `isResolved=false`, already-resolved thread retry, and unknown/outdated thread refusal.
- Add tests for `single_use` grant consumption or explicitly state that `single_use` remains advisory until a follow-up slice.
- The acceptance criterion “resolved automatically” is slightly ahead of the MVP, which still relies on an agent calling `resolve_addressed`. Reword to “resolved by the explicit workflow/tool step” for Slice 2, then reserve “automatically” for `watch_resolve`.

required_revisions:
- Define batch resolution semantics precisely, including partial success persistence, retry/idempotency behavior, and mandatory post-mutation poll.
- Specify the separate remediation scheduled payload/executor boundary so the existing read-only `PrWatchSchedulePayload` remains untouched for monitor/poll scheduling.
- Move root-dir/state-root hardening into the first mutating slice or make it a prerequisite for `resolve_addressed`.
- Strengthen audit requirements to record every resolution attempt, including failed and skipped attempts, not only successful resolved evidence.
- Tighten tests around grant expiry/session mismatch, stale head SHA, already-resolved threads, malformed GitHub responses, `isResolved=false`, and no-mutation-on-local-validation-failure.

final_recommendation:
Iterate, then approve. The architecture is directionally sound, scoped well, and grounded in the current code. The remaining gaps are not conceptual blockers to the feature, but they are important safety details around remote mutation and scheduler boundaries that should be fixed before implementation starts.


