verdict: APPROVE

blockers:
- None. Codex pass 1's two blockers (partial remote mutation semantics + remediation payload boundary) are now resolved in the plan. Codex pass 2 already approved, and its three non-blocking cleanups have been folded in: phases are renumbered 1–7 sequentially (no stray "Phase 0"), `run_gh_resolve_review_thread` now returns a typed `ResolveReviewThreadOutcome`, and the acceptance criteria now mirror the stronger fake-gh/payload-boundary test matrix (lines 345–347).

major_findings:
- Read-only monitor invariant is preserved correctly. `PrWatchSchedulePayload` continues to reject `readonly=false` and accept only `ack_baseline | poll_now | monitor`. The mutating path is a distinct `PrWatchRemediationPayload` with its own schedule kind (`pr_watch.watch_resolve`) and parser, with tests required in both directions (lines 203–229). This is the right structural separation.
- Grant gating is enforced at runtime, not schedule time (line 227), with explicit handling for session/expiry mismatch and a documented (if slightly hedged) preference for atomic `single_use` consumption (line 252).
- Partial-batch semantics are now tight: pre-mutation local prevalidation aborts before any GitHub call; post-mutation partial failure persists every per-thread `ThreadResolutionAttempt`, returns non-success, sets `resolution_requires_post_poll=true`, and mandates a `poll_now` before retry; idempotency rule guards against masking a third-party resolve.
- Audit coverage is comprehensive: every requested thread produces an attempt record with status, head/commit SHA, validation evidence, reason, and API error (lines 266–276).
- Root-directory hardening is correctly placed as Phase 1 / Slice 2, before `resolve_addressed`, with explicit fail-loud diagnostics instead of silent state creation (lines 89–98).
- The plan does solve the user's stated loop. MVP (Slice 2/3) gives a first-class `resolve_addressed` primitive plus hardened handoff completion criteria; Slice 4 then layers `watch_resolve` orchestration on top. The acceptance criteria correctly describe the MVP as "resolved by the explicit `resolve_addressed` workflow step" rather than overclaiming full automation (line 339).

minor_findings:
- `validation: Vec<ValidationEvidence>` is referenced in Phase 2 and Phase 3 inputs but `ValidationEvidence` is not defined or cross-referenced in the plan. Presumably reused from `jcode-pr-watch-core`; add a one-line pointer so the implementer is not guessing the shape.
- `single_use` grant handling still hedges ("preferred implementation is atomic consumption … or explicitly treated as advisory in the MVP with a tracked follow-up"). Pick one for Slice 3 and add a unit test for it; leaving both branches open invites divergence.
- Phase 3 step 8's idempotency rule is correct but subtle (third-party resolution between snapshot and call → first call fails → mandatory `poll_now` → retry sees `AlreadyResolved`). Worth a one-line note in the audit section so reviewers don't read the first failure as a real defect.
- Acceptance criterion for "no PR merge path" is asserted but no negative test is listed. A schema test that the action enum does not expose a merge variant would be cheap insurance.
- The `comment_addressed` follow-up is mentioned but the plan does not say what happens today if the watcher detects actionable issue comments (not review threads). Worth one sentence: those remain blocked-with-reason in the MVP.

implementation_readiness:
Ready. The slice ordering is implementable in the stated sequence (Slice 1 prompt/status → Slice 2 root-dir → Slice 3 `resolve_addressed` + fake-gh tests → Slice 4 `watch_resolve` scheduling → Slice 5 dogfood). Types, grant checks, batch semantics, audit fields, and the test matrix are concrete enough to begin coding. The boundary between agent-driven remediation and tool primitives is clear, so scope creep into a "general autonomous code-edit engine" is structurally prevented.

final_recommendation:
Approve for implementation. The two Codex pass 1 blockers and the three Codex pass 2 non-blocking cleanups are addressed. Proceed with Slice 1, then Slice 2 root-dir hardening before any mutating code lands. Resolve the five minor findings inline during Slice 3 rather than holding the plan: (1) cross-reference `ValidationEvidence`, (2) commit to atomic `single_use` consumption with a test, (3) document the third-party-resolution idempotency path, (4) add a negative schema test asserting no merge action, (5) state explicit MVP behavior for non-review-thread issue comments.
