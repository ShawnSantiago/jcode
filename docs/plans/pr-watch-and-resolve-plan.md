# PR Watch-and-Resolve Implementation Plan

## Status
Approved by Codex pass 2 and Claude final review. Minor Claude clarifications folded in.

## Codex pass 1 revision summary
Codex returned `ITERATE`. This revision tightens: batch resolution semantics, separate remediation payload boundaries, root-directory hardening as a prerequisite, complete audit logging for attempted/resolved/skipped/failed thread actions, and additional fake-gh tests for malformed/partial/idempotent cases.

## Goal
Add a safe, explicit PR watch-and-resolve workflow so Jcode can autonomously run reviewer-feedback loops:

1. watch a PR,
2. detect actionable feedback or failed checks,
3. remediate when authorized,
4. validate,
5. commit and push when authorized,
6. resolve addressed GitHub review threads when authorized,
7. continue watching for the next reviewer pass.

This solves the current stall where an agent fixes/pushes changes but does not resolve addressed review threads, so external reviewer agents do not continue their loop or the user must manually remind the agent.

## Current repo state and evidence

### Existing implementation
- `crates/jcode-app-core/src/tool/pr_watch.rs` defines `PrWatchAction` with `start`, `status`, `list`, `poll_now`, `monitor`, `ack_baseline`, `authorize`, `revoke`, `reschedule`, `stop`, `readiness`, and `handoff`.
- Tool description currently states that `pr_watch` performs no pushes, comments, thread resolution, or merges.
- `PrWatchSchedulePayload::validate_against_state` only permits read-only scheduled actions: `ack_baseline`, `poll_now`, and `monitor`.
- `PrWatchHandoffPayload` exists and can schedule an immediate action-required handoff to the origin session.
- `handoff_prompt` tells the agent not to resolve review threads without a `resolve_threads` grant, but resolution is not a first-class completion requirement.
- `WriteScope` in `crates/jcode-pr-watch-core/src/lib.rs` already includes `ResolveThreads` alongside `LocalFix`, `Commit`, `Push`, and `Comment`.
- Authorization grants are session-bound and expiry-bound through `AuthorizationGrant::grants`.

### Observed operational problem
For PR `ShawnSantiago/cakepage#245`, the watcher correctly detected action required and scheduled/ran follow-up. Remediation completed, but the workflow depends on the agent remembering to resolve addressed review threads. Without a first-class resolve step, the loop can stall.

### Known adjacent issue
Watch state is stored under the current repo root at `.jcode/pr-feedback-watch`. Querying a cakepage watch from the Jcode repo fails to find the state file. This is not the core watch-and-resolve feature, but the plan should include a small hardening slice so scheduled/remediation tasks use the correct working directory and status errors are clearer.

## RALPLAN-DR summary

### Principles
1. Safety by explicit mode: keep `monitor` read-only and add a separate mutating workflow.
2. Grant-gated mutation: no commit, push, comment, or resolve without an active grant for that scope.
3. Evidence before resolution: only resolve review threads after validation and a pushed fix are recorded.
4. Conservative resolution: resolve only threads confidently mapped to addressed actionable feedback.
5. Continuous loop: after remediation/resolution, always re-poll and reschedule until quiet cycles are satisfied.

### Decision drivers
1. User wants autonomous PR review loops without manual reminders.
2. Existing safety contract promises scheduled monitors are read-only.
3. GitHub review-thread resolution is remote mutation and must be auditable.

### Viable options

#### Option A: Strengthen handoff prompt only
- Pros: fastest, minimal code.
- Cons: still relies on model compliance; not machine-enforced; stalls can persist.
- Decision: insufficient alone, but useful as an MVP sub-slice.

#### Option B: Add `resolve_addressed` action plus prompt-orchestrated remediation
- Pros: small, explicit, grant-gated; lets agents call a concrete tool step after validation.
- Cons: still relies on agent to provide addressed thread IDs and evidence.
- Decision: best MVP.

#### Option C: Full `watch_resolve` executor that performs the whole remediation lifecycle
- Pros: most autonomous end state.
- Cons: larger scope; hard to encode arbitrary code edits inside one tool; higher safety risk.
- Decision: target architecture, implemented after Option B primitives are proven.

## Scope

### In scope
- Add explicit mutating workflow support without changing `monitor` read-only behavior.
- Add structured resolution action for addressed GitHub review threads.
- Add state fields for remediation/resolution evidence.
- Add status/readiness output that reports missing grants and resolution blockers.
- Update action-required handoff prompts so resolving addressed threads is a required completion criterion when authorized.
- Add tests for grant enforcement, prompt content, state transitions, and GitHub mutation command construction.

### Out of scope
- Merging PRs.
- Auto-resolving issue comments that are not GitHub review threads.
- Blindly resolving all unresolved threads after a push.
- Building a general autonomous code-edit engine inside `pr_watch`.
- Changing external reviewer tools such as Jules/Codex/Copilot.

## Proposed architecture

### Phase 1: Working-directory hardening prerequisite
Before implementing `resolve_addressed`, store `root_dir` or `state_root` in watch state when a watch is started. Scheduled payloads should include `working_dir`. `status`, `handoff`, and mutating actions should produce a specific error if the watch is being queried from the wrong repo root:

```text
Watch state not found in current working directory.
Expected root: /home/shawn/projects/cakepage
Current root: /home/shawn/business-projects/jcode
```

Do not silently create a new state in the wrong repo. This phase must land before or in the same implementation slice as `resolve_addressed`, because resolving from the wrong repo root is a safety failure.

### Phase 2: Model additions
Add types to `jcode-pr-watch-core`:

```rust
pub enum WatchMode {
    ReadOnly,
    WatchAndResolve,
}

pub enum ResolutionAttemptStatus {
    Planned,
    Skipped,
    AlreadyResolved,
    Resolved,
    Failed,
}

// Reuse the existing jcode_pr_watch_core::ValidationEvidence shape for validation records.
pub struct ThreadResolutionAttempt {
    pub thread_id: String,
    pub attempted_at: String,
    pub status: ResolutionAttemptStatus,
    pub head_sha: Option<String>,
    pub commit_sha: Option<String>,
    pub validation: Vec<ValidationEvidence>,
    pub reason: String,
    pub error: Option<String>,
}
```

Extend `PrWatchState` with optional/defaulted fields:

- `watch_mode: WatchMode` default `ReadOnly`.
- `last_resolution_attempts: Vec<ThreadResolutionAttempt>` with attempted, skipped, already-resolved, resolved, and failed entries.
- `last_resolution_error: Option<String>`.
- `resolution_requires_post_poll: bool` default `false`, set after any remote resolution attempt until a fresh poll completes.

Compatibility: defaults preserve existing state JSON.

### Phase 3: Add `resolve_addressed`
Add `PrWatchAction::ResolveAddressed` with input fields:

- `thread_ids: Vec<String>`
- `fingerprint: Option<String>`
- `head_sha: Option<String>`
- `commit_sha: Option<String>`
- `validation: Vec<ValidationEvidence>` or a compact validation evidence input
- `dry_run`

Behavior:
1. Resolve the watch state root before loading state. This is a prerequisite for mutating actions. If the state is not in the current working directory, fail with the recorded expected root and do not create new state.
2. Require active `resolve_threads` grant for current session.
3. Require non-empty validation evidence unless `dry_run`.
4. Require `head_sha` to match current watch `pr.head_sha` or fail with stale state.
5. For code-change resolutions, require either `commit_sha` or an explicit `reason` that the resolution is documentation/config/no-code and validated. If `push` grant exists and a code fix was made, require the pushed PR head SHA to match `head_sha`.
6. Prevalidate every supplied `thread_id` before any GitHub mutation: it must be present in `last_seen.review_threads`, must not be marked resolved in current state, and must be present in or explicitly linked to actionable feedback. If any local prevalidation fails, no remote mutation occurs.
7. Resolve threads sequentially with GitHub GraphQL `resolveReviewThread`, recording a `ThreadResolutionAttempt` for every requested thread.
8. Treat already-resolved GitHub responses as idempotent success with status `AlreadyResolved` only if the thread was previously attempted by this watch or a fresh poll shows it resolved. If a third party resolves the thread between snapshot and mutation, the first call should fail/record uncertainty, the mandatory post-mutation poll should observe the resolved state, and only a retry may classify it as `AlreadyResolved`.
9. If a later thread fails after earlier successes, persist all attempt results, return a partial-failure status, set `resolution_requires_post_poll=true`, and require an immediate `poll_now` before retry. Retries must skip `Resolved`/`AlreadyResolved` attempts unless a fresh poll proves the thread is still unresolved.
10. After any remote mutation attempt, schedule or run a mandatory post-mutation poll. The watch is not considered healthy until that poll clears `resolution_requires_post_poll`.

Failure mode before remote mutation: if any grant/evidence/head/local-prevalidation check is missing, no remote mutation occurs and no thread is resolved. Failure mode after remote mutation begins: partial success is allowed only with persisted per-thread attempt evidence and mandatory post-poll.

### Phase 4: GitHub mutation helper
Add a helper in `pr_watch.rs`:

```rust
enum ResolveReviewThreadOutcome {
    Resolved,
    AlreadyResolved,
    NotResolved,
    MalformedResponse(String),
}

async fn run_gh_resolve_review_thread(root: &Path, thread_id: &str) -> Result<ResolveReviewThreadOutcome>;
```

Implementation uses `gh api graphql` with mutation:

```graphql
mutation($threadId: ID!) {
  resolveReviewThread(input: {threadId: $threadId}) {
    thread { id isResolved }
  }
}
```

Testing should validate command construction and parse behavior without live GitHub calls.

### Phase 5: Handoff prompt hardening
Update `handoff_prompt` to make completion criteria explicit:

- inspect `pending_actionable`,
- remediate,
- validate,
- commit/push if grants permit,
- call `pr_watch action=resolve_addressed` for addressed review thread IDs if `resolve_threads` grant exists,
- explicitly record a blocked reason if not resolving,
- poll/reschedule afterward,
- do not report done until addressed threads are resolved or blocked/skipped reasons are recorded.

This improves behavior even before full `watch_resolve` exists.


### Phase 6: Separate remediation schedule payload boundary
Do not relax `PrWatchSchedulePayload`. It remains read-only and continues to reject `readonly=false` and actions other than `ack_baseline | poll_now | monitor`.

Add a distinct payload and parser:

```rust
pub struct PrWatchRemediationPayload {
    pub tool: String,
    pub watch_id: String,
    pub repo: String,
    pub pr: u64,
    pub action: String, // initially only "watch_resolve"
    pub state_file: String,
    pub required_scopes: BTreeSet<WriteScope>,
    pub fingerprint: Option<String>,
    pub cycle_number: u64,
    pub readonly: bool, // must be false
}
```

Rules:
- `schedule_kind` must be `pr_watch.watch_resolve`.
- The read-only scheduled-item parser must ignore or reject this payload without treating it as monitor work.
- The remediation parser accepts only `action="watch_resolve"`, `readonly=false`, and a non-empty `required_scopes`.
- Execution rechecks all required grants at runtime; schedule-time grants are not enough.
- Tests must prove remediation payloads cannot be run by the read-only monitor executor and read-only payloads cannot request mutation.
- Audit output must include schedule id, required scopes, grant ids used, fingerprint, and current session id.

### Phase 7: Add `watch_resolve` mode/scheduler
After `resolve_addressed` is reliable, add `PrWatchAction::WatchResolve` as an orchestration action.

`watch_resolve` should:
1. Run a normal poll.
2. If no actionable items, schedule next cycle.
3. If actionable items exist, check grants and produce a remediation checklist.
4. If running in an agent session with sufficient grants, perform the normal agent-driven remediation workflow and require `resolve_addressed` before completion.
5. Continue scheduling next watch cycle after push/resolution.

Important: keep `PrWatchSchedulePayload` read-only. Add a separate `PrWatchRemediationPayload` for mutating scheduled work, with `readonly: false` and required scopes embedded for audit. Scheduled mutating work must refuse to run if required grants are not active.

## Safety model

### Grants
- `local_fix`: may edit files locally.
- `commit`: may create commits.
- `push`: may push branch commits.
- `comment`: may post PR comments.
- `resolve_threads`: may resolve GitHub review threads.

All mutating actions must check grants at execution time, not schedule time only. Expired or session-mismatched grants block mutation. `single_use` grants must be consumed atomically after a successful mutating action, with state recording the consumed grant id and a unit test proving a second mutation attempt is blocked.

### No partial remote mutation
`resolve_addressed` should perform all local validation checks before calling GitHub. If checks fail, no thread is resolved.

For multiple threads, if GitHub resolves some and then one fails, record per-thread success/failure and return non-success with clear evidence. Prefer resolving sequentially with state updates after the batch completes.

### Conservative thread selection
Only resolve review thread IDs that the remediation agent explicitly supplies and that correspond to known unresolved review threads. Do not resolve:
- issue comments,
- outdated but unresolved threads unless explicitly present and addressed,
- threads not represented in the current watch state,
- threads with ambiguous or unvalidated fixes.

### Audit trail
Every requested thread gets a `ThreadResolutionAttempt`, including skipped and failed entries. Each entry records:
- thread ID,
- attempted time,
- status: skipped, already_resolved, resolved, or failed,
- current head SHA,
- commit SHA or explicit no-code reason,
- validation evidence,
- reason,
- GitHub/API error if any,
- whether a post-mutation poll is required.

## Tests and validation

### Unit tests
- `ResolveAddressed` action appears in schema.
- Missing `resolve_threads` grant blocks resolution.
- Expired/session-mismatched grant blocks resolution.
- Missing validation evidence blocks resolution.
- Stale head SHA blocks resolution.
- Unknown thread ID blocks resolution.
- Dry-run shows intended resolution without mutation.
- Already-resolved retry is idempotent only with prior attempt evidence or fresh poll evidence.
- Malformed GraphQL success and `isResolved=false` are failures.
- Unknown, outdated, or locally prevalidation-failing threads block before any remote mutation.
- Partial batch success persists all attempts and requires post-mutation poll before retry.
- Handoff prompt requires `resolve_addressed` or blocked reason.
- Existing read-only scheduled payload tests still reject mutating actions.
- New remediation payload tests require `readonly=false` and explicit required scopes.

### Integration-style tests with fake gh
Use a temporary fake `gh` binary on PATH to assert:
- GraphQL mutation command is invoked with expected variables.
- Successful response records `ThreadResolutionAttempt { status: Resolved }`.
- Failed response records `status: Failed`, preserves prior successes, sets `resolution_requires_post_poll=true`, and leaves unresolved state visible.
- Malformed response and `isResolved=false` are treated as failures.

### Manual dogfood acceptance
On a real PR with reviewer comments:
1. Start watch-and-resolve mode with grants for local_fix, commit, push, resolve_threads.
2. Reviewer posts actionable thread.
3. Watch detects action required.
4. Agent fixes and validates.
5. Agent commits/pushes.
6. Agent calls `resolve_addressed` and resolves only addressed thread IDs.
7. Watch re-polls and shows no actionable threads.
8. External reviewers continue and can add new feedback.

## Rollout plan

### Slice 1: Prompt and status hardening
- Add explicit completion criteria to handoff prompt.
- Add status output for grants and resolution blockers.
- No GitHub mutation yet.

### Slice 2: Root-dir hardening prerequisite
- Persist watch root and improve wrong-cwd diagnostics.
- Ensure scheduled and mutating actions use the correct `working_dir`.

### Slice 3: `resolve_addressed` primitive
- Add action, state evidence, grant checks, batch semantics, mandatory post-poll behavior, and fake-gh tests.
- Keep agent-driven remediation outside the tool.

### Slice 4: Watch-and-resolve scheduling mode
- Add mode and mutating remediation payload.
- Ensure scheduled remediation refuses to run without grants.
- Preserve read-only monitor invariants.

### Slice 5: Dogfood and refine
- Use on a Jcode or cakepage PR.
- Record gaps and iterate.

## Acceptance criteria
- Slice 2/3 MVP: addressed review threads are resolved by the explicit `resolve_addressed` workflow step when a valid `resolve_threads` grant exists and evidence is present. Full `watch_resolve` automation is reserved for the later orchestration slice.
- If the grant is missing, the agent reports an explicit blocked reason and does not claim the loop is complete.
- Read-only `monitor` remains read-only and tests enforce this.
- No PR merge path is introduced, and a schema/enum negative test asserts there is no merge action.
- Status/readiness clearly shows whether the loop is blocked on comments, checks, grants, validation, push, or resolution.
- Existing PR watch tests pass.
- New fake-gh tests cover resolution success, plain failure, partial batch success, malformed GraphQL success, `isResolved=false`, idempotent already-resolved retry, and unknown/outdated thread refusal.
- Payload-boundary tests prove read-only scheduled payloads cannot request mutation and remediation payloads cannot be executed by the read-only monitor path.

## ADR

### Decision
Implement a conservative two-step path: first add an explicit `resolve_addressed` primitive and hardened handoff completion criteria, then build `watch_resolve` orchestration on top of it.

### Drivers
- The user needs autonomous reviewer loops.
- Safety requires explicit grant-gated remote mutation.
- The existing monitor contract must remain read-only.

### Alternatives considered
- Prompt-only fix: rejected as insufficiently enforceable.
- Make `monitor` mutating: rejected because it breaks the existing safety contract.
- Full autonomous executor first: deferred because arbitrary remediation is better handled by agent workflow plus concrete tool primitives.

### Consequences
- The MVP will still rely on the agent to identify addressed thread IDs, but it gives the agent a first-class tool action and completion criterion.
- Later `watch_resolve` can become more autonomous without weakening safety.

### MVP behavior for non-review-thread comments
Actionable issue comments and other non-review-thread feedback are not resolved by `resolve_addressed`. In the MVP they remain blocked-with-reason unless a later `comment_addressed` action is implemented under a `comment` grant.

### Follow-ups
- Consider a separate `comment_addressed` action for issue comments when `comment` grant exists.
- Consider cross-repo watch-state index after `root_dir` hardening.
- Consider UI/status affordances for active watch-and-resolve loops.
