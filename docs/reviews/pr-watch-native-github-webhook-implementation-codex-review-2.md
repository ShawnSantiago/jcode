verdict: ITERATE

blockers:
- `crates/jcode-app-core/src/tool/pr_watch.rs:2279-2290`, `2398-2401`, `2448-2462`: verified deliveries are routed directly into immediate refreshes, and lock contention only returns `locked_followup_requested` without persisting or scheduling the required single queued follow-up refresh. There is still no per-watch debounce/coalescing window, bounded collapsed-reason tracking, global/per-repo refresh concurrency, or retry/backoff path. This violates the approved debounce/backpressure and refresh-lock boundary requirements.
- `crates/jcode-app-core/src/tool/pr_watch.rs:4467-4479`, `4482-4511`, `4514-4554`: tool-level `pr_watch status`/`webhook_status` still only report watch-local state and do not distinguish daemon down, tunnel down, or GitHub hook failure. The tool-level `webhook_doctor` explicitly says it reports “state-side readiness only” and does not inspect daemon health, tunnel reachability, GitHub hook last response, or required hook events. This violates acceptance criteria 4 and 8.
- `crates/jcode-app-core/src/tool/pr_watch.rs:1993-2009`, `2030-2060`: CLI doctor is improved but incomplete against the plan. It checks indexed root/state file existence and scans hooks for any non-2xx `last_response`, but it does not load indexed state to verify `root_dir`/repo/PR match, does not verify required hook events, and does not validate public URL/tunnel reachability. This still would not fully satisfy the doctor checks listed in the approved plan.
- `crates/jcode-app-core/src/tool/pr_watch.rs:2184-2195`, `2279-2286`, `2357-2358`: ignored/unroutable deliveries are returned as strings and last-result health only; there is no structured daemon delivery log with reason, delivery ID, event, repo, and timestamp for ignored deliveries. The plan requires ignored deliveries, including unknown/unwatched/check-suite-without-PR cases, to be logged diagnostically without raw payloads.

non_blocking:
- Event-specific routing is materially improved: `issue_comment` now requires `issue.pull_request`, `check_run` and `check_suite` iterate associated PRs, and `status` resolves SHA via `repos/{repo}/commits/{sha}/pulls`.
- Persistent delivery dedupe is materially improved with a local delivery store capped at 10,000 IDs and 7 days.
- The refresh path now acquires the watch lock before loading state and validates root/repo/PR/watch ID/terminal state before writing.
- Heartbeat payload distinctness is mostly addressed with `PrWatchWebhookHeartbeatPayload`, `heartbeat_seconds`, `pr_watch.webhook_heartbeat`, and a dedicated schedule key.
- Untracked PNG files are not in `git diff --name-status`; treating them as excluded from the PR as requested.

validation_reviewed:
- Reported validations reviewed: `scripts/dev_cargo.sh test -p jcode-pr-watch-core -p jcode-app-core pr_watch --lib`, `scripts/dev_cargo.sh check -p jcode --bin jcode`. These are useful compile/unit signals, but they do not close the remaining plan/safety gaps above.
