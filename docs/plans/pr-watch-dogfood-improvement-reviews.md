# PR Watch Dogfood Improvement Plan Reviews

Date: 2026-06-10
Plan: `docs/plans/pr-watch-dogfood-improvement-plan.md`
Context snapshot: `.omx/context/pr-watch-dogfood-improvement-20260610T024510Z.md`

## Codex ralplan-style review

### Architect pass v1

The first Codex Architect invocation inspected code but exceeded the bounded runtime before producing a final verdict. It was cancelled and replaced by a narrower embedded-evidence review.

### Critic pass v1

Verdict: `ITERATE`

Required changes:

1. Keep existing watch actions read-only, especially `monitor` and `poll_now`.
2. Make `authorize` record an explicit operator grant envelope only.
3. Require a separate explicit remediation workflow/action for commit, push, comment, or thread resolution.
4. Forbid scheduled prompts from mutating remotely, even with an active grant.
5. Clarify legacy boolean policy behavior to avoid a `base_policy && active_grant` trap.
6. Specify `reschedule` as idempotent, watch-id scoped, concurrency-safe, and auditable.
7. Add tests proving scheduled and unauthorized paths cannot push, comment, resolve threads, or merge.

### Critic pass v2

Verdict: `APPROVE`

Codex approved after the plan was revised to preserve read-only watch semantics, split grant recording from remediation, keep scheduled cycles read-only, make legacy booleans compatibility-only, and require safety tests.

Carry-forward caution: Acceptance Criterion 4 must mean a separately invoked remediation path can consume a grant, not that ordinary `pr_watch` watch actions gain remote mutation behavior. This wording was incorporated into the plan.

## Claude final review

Verdict: `APPROVE`

Blockers: none.

Major concerns: none.

Claude's summary:

> The plan is safe, scoped, evidence-driven, and preserves the read-only watch invariant. Codex's residual caution about `base_policy && active_grant` has been correctly absorbed into §4.2 rule 5 and acceptance criterion 4. Safety boundaries are explicit and tested.

Minor recommendations incorporated into the final plan:

1. Add explicit grant revocation.
2. Surface grant lifecycle audit entries in handoff/status output.
3. Clarify grant expiry and scheduled monitor read-only behavior.
4. Pin classifier tests to observed PR #108/#109 style bot-noise fixtures.
5. Require the reschedule implementation PR to cite its lock/CAS mechanism.
6. Track eventual deprecation of legacy boolean policy fields.

Implementation readiness: ready. Claude recommended proceeding with Slice A first.
