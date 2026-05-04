# Jcode-native Workflow Modes Plan

Status: revised plan, not yet implemented  
Date: 2026-05-04

## Purpose

Add OMX-inspired workflow modes to Jcode while using Jcode's native typed runtime instead of copying OMX's tmux and ad hoc JSON state model.

Target workflows:

- `/ralplan`: consensus planning, Planner -> Architect -> Critic.
- `/ulw` / `/ultrawork`: bounded parallel execution over independent work lanes.
- `/ralph`: persistent completion loop with verification and stop/completion gates.

Non-goal: reimplement `omx team` tmux pane orchestration. Jcode already has native swarm/session/background execution primitives.

## Architectural decision

Jcode workflow modes must be built on existing typed primitives:

- `jcode-plan::PlanItem`
  - canonical plan/task graph item.
  - existing graph helpers: `summarize_plan_graph`, `next_runnable_item_ids`, `newly_ready_item_ids`.
- `jcode-task-types::Goal`
  - persistent user-facing goal/completion model.
  - existing milestones, success criteria, blockers, progress.
- `jcode-background-types::BackgroundTaskStatus`
  - canonical source for background task liveness.
- `jcode-agent-runtime::{SoftInterruptQueue, InterruptSignal, GracefulShutdownSignal}`
  - canonical stop/interrupt integration points.

Do **not** create `.jcode/state/workflows/*.json` as an independent source of truth for plans, todos, or completion. Workflow state may be persisted, but it must reference typed `Goal` and `PlanItem` data rather than duplicating them.

## Ownership

Create a new crate:

```text
crates/jcode-workflow/
```

Initial dependencies:

- `jcode-plan`
- `jcode-task-types`
- `jcode-background-types`
- `serde`
- `chrono`

Later, when implementing Ralph stop gates, add `jcode-agent-runtime` if needed.

Rationale: workflow modes are higher-level orchestration policy. Keeping them out of `jcode-agent-runtime` avoids mixing runtime signal primitives with planning/goal policy, while still allowing the main binary/server to compose the pieces.

## Core types

### WorkflowMode

```rust
pub enum WorkflowMode {
    Ralplan,
    Ultrawork,
    Ralph,
}
```

### WorkflowPhase

```rust
pub enum WorkflowPhase {
    Planning,
    Drafting,
    ArchitectReview,
    CriticReview,
    Executing,
    Verifying,
    Blocked,
    Completed,
    Cancelled,
    Failed,
}
```

### WorkflowState

```rust
pub struct WorkflowState {
    pub schema_version: u32,
    pub mode: WorkflowMode,
    pub active: bool,
    pub phase: WorkflowPhase,
    pub session_id: Option<String>,
    pub goal_id: Option<String>,
    pub plan_id: Option<String>,
    pub iteration: u32,
    pub max_iterations: u32,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}
```

Important: `WorkflowState` identifies the active workflow and links to canonical `Goal` and `PlanItem` collections. It does not own a separate TODO list.

### WorkflowActivation

```rust
pub struct WorkflowActivation {
    pub mode: WorkflowMode,
    pub source: WorkflowActivationSource,
    pub raw_trigger: String,
    pub task_text: String,
}

pub enum WorkflowActivationSource {
    SlashCommand,
    ExplicitKeyword,
}
```

## Mode composition rules

MVP rule: at most one primary workflow mode active per session.

Allowed transitions:

- `ralplan -> ultrawork`: allowed after plan approval.
- `ralplan -> ralph`: allowed after plan approval, once Ralph exists.
- `ultrawork -> completed/cancelled/failed`: normal terminal path.
- `ralph` may internally use ultrawork-style planning/execution lanes, but does not activate a separate `Ultrawork` workflow state in MVP.

Rejected transitions:

- Starting `/ralplan` while `/ulw` is active unless the current workflow is completed/cancelled.
- Starting `/ulw` while `/ralplan` is still reviewing unless explicitly using the approved plan handoff.
- Starting `/ralph` in MVP, except as a documented placeholder that explains Ralph is not enabled until stop gates exist.

## Activation policy

Prefer explicit slash commands. Avoid broad natural-language triggers in MVP.

MVP accepted triggers:

- `/ralplan ...`
- `/ulw ...`
- `/ultrawork ...`

Deferred triggers:

- `/ralph ...`
- `ralph`
- `ulw` as a bare non-slash keyword
- `don't stop`
- `must complete`
- `consensus plan`

Reason: broad phrase triggers can activate workflows from incidental prose. If implicit triggers are added later, they must be high-confidence, visible to the user, and covered by negative tests.

## Canonical plan representation

`/ralplan` emits a canonical `Vec<PlanItem>`.

Use `PlanItem` fields as follows:

- `id`: stable item id, for example `plan-1`, `test-1`, `verify-1`.
- `content`: task description.
- `status`: `queued`, `ready`, `running`, `completed`, `blocked`, `failed`.
- `priority`: `high`, `medium`, `low`.
- `subsystem`: logical subsystem when known.
- `file_scope`: files or directories the item may edit/read.
- `blocked_by`: item ids that must complete first.
- `assigned_to`: swarm/subagent/session id once assigned.

If ralplan needs richer metadata, add typed extensions deliberately instead of using a parallel JSON shape. Candidate future additions:

- `acceptance_criteria: Vec<String>`
- `risk_notes: Vec<String>`
- `verification_commands: Vec<String>`

Markdown plans are rendered views. JSON `PlanItem` data is canonical.

## Verification policy

Verification must be explicit before Ralph ships.

MVP verification levels:

- Ralplan: graph validation only.
  - `summarize_plan_graph` has no cycles.
  - unresolved dependency ids are empty.
  - at least one ready/runnable item exists unless the plan is intentionally blocked.
- Ultrawork: lightweight evidence.
  - If all file scopes are within one Rust crate, run `cargo check -p <crate>` when feasible.
  - Otherwise run the smallest known relevant command discovered during planning or report why no command was run.
  - Never claim full completion guarantee.
- Ralph: deferred until stop gates and verification command policy are implemented.

## Stop/completion gates for Ralph

Ralph must not ship as an active workflow until gates are implemented.

Completion predicate:

```rust
RalphComplete ==
    all PlanItem statuses are terminal/completed as appropriate
    && required verification has passed
    && no tracked background task is Running
    && changed files review is recorded
    && final evidence is present
```

Stop behavior:

- On `GracefulShutdownSignal`, check `RalphComplete`.
- If complete, allow shutdown/completion.
- If incomplete, enqueue a `SoftInterruptMessage` explaining missing gates and continue.
- Provide an escape hatch for user intent, for example repeated interrupt or explicit `/cancel`, so Ralph never traps the user.

The exact turn-loop integration should be implemented and tested before `/ralph` activation is enabled.

## UI/status contract

Workflow status is not optional. Any multi-step workflow must be visible.

Minimum status fields:

- mode
- phase
- iteration/max_iterations when relevant
- linked goal id
- ready/blocked/active/completed counts from `summarize_plan_graph`
- age / last update
- blocker summary if failed or blocked

Example status strings:

```text
ralplan critic-review 2/3 ready:3 blocked:1
ulw executing ready:0 active:2 completed:4
ralph verifying 4/50 missing:tests,review
```

## Revised implementation sequence

### PR 1: Workflow foundations

Scope:

- Add `crates/jcode-workflow`.
- Add `WorkflowMode`, `WorkflowPhase`, `WorkflowState`, `WorkflowActivation`.
- Add conservative slash-command activation parser.
- Add mode transition validation.
- Add state serialization with `schema_version`.

Acceptance tests:

- State serde round-trip for all modes.
- Transition validation accepts and rejects expected mode changes.
- Slash activation detects `/ralplan`, `/ulw`, `/ultrawork`.
- Negative keyword tests do not activate on incidental prose.

### PR 2: Ralplan planning protocol

Scope:

- Implement ralplan orchestration policy.
- Planner creates initial `Vec<PlanItem>`.
- Architect reviews after planner.
- Critic reviews after architect.
- Iterate up to default max 3.
- Persist or attach canonical plan in the existing plan/swarm/session path, not separate workflow TODO JSON.

Acceptance tests:

- Ralplan produces non-empty `Vec<PlanItem>`.
- `summarize_plan_graph` reports no cycles for valid plan.
- Architect and Critic order is enforced.
- Non-approval reaches max iterations and returns failed/needs-user-review status.
- Markdown rendering, if present, is generated from canonical JSON.

### PR 3: Ultrawork execution protocol

Scope:

- Consume a `Vec<PlanItem>` or synthesize one for simple slash-command tasks.
- Use `next_runnable_item_ids` to select lanes.
- Spawn native Jcode agents only for independent tasks.
- Do not run concurrent tasks with overlapping `file_scope`.
- Track assignments through `PlanItem.assigned_to`.
- Close with lightweight verification evidence.

Acceptance tests:

- Independent non-overlapping items are eligible for parallel execution.
- Overlapping `file_scope` items are serialized or kept local.
- Blocked items are not assigned before dependencies complete.
- Failed verification yields clear final blocked/failed status.

### PR 4: UI/status integration

Scope:

- Render active workflow state in TUI/status surfaces.
- Link workflow state to swarm/background widgets where possible.
- Add `/workflow status` or equivalent status surface if appropriate.

Acceptance tests:

- Snapshot/status formatting for ralplan, ultrawork, and future Ralph.
- Completed/cancelled workflows clear or dim their status.

### PR 5: Ralph gates and loop

Scope:

- Implement `RalphCompletionGate` over canonical plan, verification evidence, background tasks, and review evidence.
- Integrate with `GracefulShutdownSignal` and `SoftInterruptQueue`.
- Add explicit `/cancel` escape hatch behavior.
- Enable `/ralph` activation only after gates are in place.
- Ralph may spawn helper agents, but completion waits for all tracked work to finish.

Acceptance tests:

- Ralph completion predicate returns false for each missing gate.
- Graceful shutdown during incomplete Ralph enqueues a soft interrupt and continues.
- Explicit cancel exits Ralph.
- Running background tasks block completion.
- Passing verification + completed plan + no active tasks allows completion.

### PR 6: Optional CLI entrypoints

Scope:

```bash
jcode ralplan "task"
jcode ultrawork "task"
jcode ralph "task"
```

These should start new Jcode sessions with workflow state pre-seeded. Keep this after prompt-side workflows are stable.

## Risks and mitigations

### Risk: workflow state drifts from session state

Mitigation: use typed references to `Goal` and `PlanItem`; avoid parallel TODO state.

### Risk: user loses interrupt control under Ralph

Mitigation: implement double-interrupt or explicit `/cancel` escape hatch; show blocked completion reason in TUI.

### Risk: activation surprises users

Mitigation: MVP slash commands only; broad keywords require explicit future tests and visible activation notices.

### Risk: role/model routing ambiguity

Mitigation: default to active model for MVP. Later add role routing policy for Planner/Architect/Critic if provider/model config supports it.

### Risk: Ralplan loops too long or silently

Mitigation: default max iterations 3; status updates each phase; final non-approval returns best plan plus review reasons.

## MVP boundary

MVP includes:

- `jcode-workflow` crate.
- Slash-command activation for `/ralplan`, `/ulw`, `/ultrawork`.
- Ralplan emitting canonical `Vec<PlanItem>`.
- Ultrawork consuming `PlanItem` graph and using safe native parallelism.
- Basic workflow status display.

MVP excludes:

- Broad implicit keyword activation.
- `/ralph` active mode.
- Hard stop/completion gates.
- CLI entrypoints.
- Role-specific model routing.

## Open questions before PR 1

1. Where should workflow state be stored for active sessions: embedded in session metadata, server-side swarm state, or a small typed file with session id references?
2. Should `PlanItem` gain acceptance criteria and verification command fields now, or should PR 2 keep those in adjacent typed metadata?
3. Which status surface should own workflow display first: TUI info widget, side panel, or server status event?
4. What is the minimal API to query background tasks by session id for Ralph gates?
