# Overnight Autonomous Runbook

This runbook defines the durable start, monitor, stop, and resume workflow for long autonomous sessions that coordinate background tasks, PR watches, validation, and handoff artifacts.

## Safety contract

An overnight run stops only when either:

1. every recorded success criterion is complete, or
2. the time budget has elapsed.

Do not stop early for partial success. Partial success becomes a handoff.

For continuous-progress overnights, the configured time budget is authoritative:

1. Do not stop early just because the current backlog, milestone, or PR set completes.
2. When the active backlog completes and time remains, immediately generate the next prioritized bounded backlog and continue.
3. Stop before the time budget only for a documented hard blocker that prevents safe work under the safety constraints.

Never deploy, force-push, delete branches/data, expose secrets, or perform destructive cleanup without explicit approval. PR merges must follow the configured quiet-cycle protocol.

## Durable run record

Each run stores inspectable state under:

```text
.jcode/overnight-runs/<run-id>/status.md
.jcode/overnight-runs/current -> <run-id>
```

The status file should include:

- start time and run id
- stop condition
- safety constraints
- git snapshots for active repos
- active PR/watch/background task identifiers
- 30-minute watchdog checkpoints
- blockers and next safe action
- validation commands and results
- final handoff or completion report

## Start command

From the Jcode repo:

```bash
RUN_ID="overnight-$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR=".jcode/overnight-runs/$RUN_ID"
mkdir -p "$RUN_DIR"
ln -sfn "$RUN_ID" .jcode/overnight-runs/current
cat > "$RUN_DIR/status.md" <<EOF
# 12h Overnight Run

- Run id: $RUN_ID
- Start UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)
- Stop condition: all goals complete OR time budget elapsed.
- Safety: no deploy, force-push, destructive cleanup, secret exposure, or PR merge outside quiet-cycle protocol.

## Checkpoints
- $(date -u +%Y-%m-%dT%H:%M:%SZ) initialized run record.
EOF
```

Then create or update a `goal` record with the milestones and success criteria for the run.

## Monitor command

Use the helper script for a read-only checkpoint summary:

```bash
python3 scripts/overnight_status.py
```

The monitor should inspect:

- `.jcode/overnight-runs/current/status.md`
- current goal progress
- active background tasks
- PR watch state files under `.jcode/pr-feedback-watch/`
- git status for the Jcode repo and target repos
- recent validation output and blockers

Every 30 minutes, append a checkpoint like:

```text
- 2026-05-16T01:30:00Z watchdog: on_track. Active PR #30 waiting quiet cycle 1/3. Next action: poll at 01:34Z.
```

If stalled but safe to nudge, resume the bounded action or schedule it. If blocked, record the exact blocker and next required action.

## Anti-stall watchdog invariants

Every 30-minute watchdog must enforce these invariants before reporting the run healthy:

1. **No early stop:** if the time budget has not elapsed and no hard blocker exists, the run must have one of:
   - an active worker,
   - an open PR in quiet-cycle/final-gate protocol, or
   - a freshly generated next bounded backlog with a worker started.
2. **Stale PR-watch recovery:** compare each PR watch `next_poll` time against current UTC. If `next_poll` is in the past, run an immediate read-only `poll_now`, recompute readiness, and reschedule the next poll.
3. **Source-of-truth PR verification:** do not trust local watch state alone. Before each quiet-cycle/final-gate/merge decision, query GitHub directly for current head SHA, top-level comments, inline review threads, reviews, checks, draft status, and merge state.
4. **Idle recovery:** if no worker is active, no PR is waiting in protocol, and time remains, update the scorecard/status, choose the next highest-value bounded slice, and start a worker immediately.
5. **Checkpoint completeness:** every checkpoint must record current PR numbers and heads, active worker/session ids if known, next scheduled watchdog id/time, next PR poll time, current blocker or next backlog, and whether source-of-truth GitHub verification was used.
6. **Gate reset after mutation:** any new push to a PR resets quiet cycles from the new head. Record the new head SHA and schedule fresh 5-minute cycles plus the final 10-minute gate.

Use this watchdog prompt suffix for future continuous overnights:

```text
Anti-stall requirements: do not stop early before the target end. If the active backlog is complete and time remains, generate/start the next prioritized bounded backlog. Inspect PR watch next_poll timestamps; if stale, poll_now and reschedule. Verify PR state from GitHub source-of-truth before any quiet-cycle, final-gate, or merge decision. If no active worker and no PR in protocol, spawn/recover a worker. Checkpoint PR heads, active workers, next watchdog, next PR polls, blockers, and next backlog.
```

## Stop command

Stopping is only allowed when all goals are complete or the time budget elapsed. Append a final section:

```text
## Final report

- End UTC: ...
- Stop reason: all_goals_complete | time_budget_elapsed
- Completed milestones: ...
- Open blockers: ...
- PRs: ...
- Validation: ...
- Next safe action: ...
```

Do not delete run artifacts.

## Resume command

Resume from the current symlink:

```bash
python3 scripts/overnight_status.py
sed -n '1,220p' .jcode/overnight-runs/current/status.md
```

Then continue the next safe action from the latest checkpoint. Before mutating code, re-check git status and active PR watch state.

## PR quiet-cycle protocol

For each bounded PR:

1. Implement a bounded change.
2. Run relevant validation.
3. Commit and open/update a PR.
4. Start a local PR watch with 3 quiet cycles and 300 second poll interval.
5. Monitor inline review comments and top-level PR comments.
6. Resolve actionable comments with commits where needed.
7. Require 3 full quiet cycles of 5 minutes each.
8. After cycle 3, wait one final 10-minute quiet period.
9. If still quiet and checks are passing or clearly non-blocking per repo policy, merge.
10. Sync local state and continue the next bounded slice.

## Bounded slice guidance

Prefer slices that are easy to validate and review independently:

- one reliability/doc/script improvement
- one UI primitive extraction
- one Storybook coverage expansion
- one visual snapshot harness
- one accessibility gate improvement

Keep generated artifacts out of commits unless explicitly intended.
