---
name: pr-feedback-watch
summary: Monitor a GitHub pull request for repeated review feedback cycles, implement actionable fixes, and stop after quiet cycles.
---

# PR Feedback Watch

Use this workflow when the user asks to monitor a GitHub pull request for repeated rounds of review feedback, including polling every 5 minutes, inspecting new review comments or unresolved threads across all GitHub PR feedback surfaces, fixing actionable feedback, verifying fixes, optionally pushing and resolving addressed threads, and stopping after 3 consecutive quiet cycles.

## Default behavior

1. Confirm the exact target repo and PR from the user's request or current context. Do not accidentally watch an upstream fork when the user named a fork.
2. Establish or read local watch state under `.jcode/pr-feedback-watch/<watch-id>-state.json`.
3. Poll all read-only GitHub surfaces:
   - PR metadata and head SHA
   - check runs/status rollup
   - pull request review comments
   - issue/PR comments
   - submitted reviews
   - unresolved review threads, including new replies on existing threads
4. Treat new unresolved review threads, new review comments, requested changes, failed checks, and stale validation as actionable.
5. Implement local fixes for actionable feedback, run targeted validation, commit the fix, and push only when the user's request or an active authorization grant allows pushing.
6. Never merge. Only provide a human merge handoff after the required quiet cycles are satisfied.
7. Resolve review threads or post comments only when explicitly authorized for the current session and scope.
8. Stop automatically after 3 consecutive quiet cycles with no new actionable feedback, no failed checks, no pending checks, and no transient collection failures.

## Watchdog requirements

Create a detached watchdog whenever starting a monitor loop. The watchdog must check the watch state independently of the poller and alert or reschedule if:

- `updated_at` or `last_cycle.completed_at` is older than 2 poll intervals.
- `next_poll_at` is in the past and no poll is running.
- `consecutive_transient_failures` is non-zero.
- The PR head SHA changed without a subsequent validation record.
- The monitor terminated before reaching quiet-cycle success or a user-requested stop.

## Safety boundaries

- Read-only polling is safe by default.
- Local edits and commits are allowed when they address review feedback.
- Pushing, commenting, and resolving threads require explicit user authorization or an active scoped grant.
- Closing PRs, merging, deleting branches, or force-pushing require separate explicit confirmation.

## Recommended status report

Report each cycle as:

- PR target and head SHA
- surfaces checked and counts
- actionable item count with links
- check status counts
- quiet cycle progress, for example `1/3`
- validation commands run
- next poll time
- watchdog health
