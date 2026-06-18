verdict: ITERATE

blockers:
- `crates/jcode-app-core/src/tool/pr_watch.rs:2354`, `2370-2373`, `2475-2555`, `2558-2605`: deliveries still route directly into immediate refreshes, with a follow-up schedule only when the watch lock is busy. There is no `WEBHOOK_DEBOUNCE_MS = 10_000` debounce/coalesce queue, no per-watch collapsed reason cap, no updates to `collapsed_event_count`/`dropped_event_count`, no global/per-repo refresh concurrency bound, and no retry/backoff for transient/rate-limit refresh failures. This still violates the approved debounce/backpressure requirements.
- `crates/jcode-app-core/src/tool/pr_watch.rs:2269-2271`, `2358-2373`, `2441`: structured delivery logging is incomplete. Rejected deliveries such as signature/content validation failures only update `health.last_result`, not the structured delivery log. Unknown events and `check_suite` deliveries without PRs collapse into `no_indexed_watch_target` instead of required diagnostic reasons like `unknown_event` or `check_suite_without_pr`. This violates the plan’s structured ignored/rejected delivery logging requirements.
- `crates/jcode-app-core/src/tool/pr_watch.rs:4618-4655`, `4662-4717`: tool-level `webhook_status` only reports daemon health and does not distinguish tunnel down or GitHub hook failure. Tool-level `webhook_doctor` checks hook last response but does not validate required hook events or public URL/tunnel reachability. This still fails acceptance criteria requiring distinct daemon down, tunnel down, and GitHub hook failing signals.
- `crates/jcode-app-core/src/tool/pr_watch.rs:2036-2068`, `2088-2140`: CLI doctor now validates indexed root/state matching and required hook events when `--repo` is provided, but all-index doctor mode skips GitHub hook validation entirely because hook inspection is gated behind `if let Some(repo) = repo`. It also still does not validate public URL/tunnel reachability. This leaves the doctor surface short of the approved global/repo diagnostics.

non_blocking:
- Persistent delivery dedupe is now implemented with a 10,000-ID/7-day store.
- Event routing is materially improved for PR-only issue comments, multi-PR check runs/suites, and status SHA lookup.
- The refresh path now acquires the watch lock before loading state and validates root/repo/PR/watch ID/terminal state before writing.
- Heartbeat has a distinct payload/action/schedule key and webhook mode suppresses normal monitor scheduling.
- Untracked PNG files are not present in `git diff --name-status`; treated as excluded.

validation_reviewed:
- Reported validations reviewed: `scripts/dev_cargo.sh test -p jcode-pr-watch-core -p jcode-app-core pr_watch --lib`, `scripts/dev_cargo.sh check -p jcode --bin jcode`. These are useful compile/unit signals, but they do not close the remaining plan/safety gaps above.
