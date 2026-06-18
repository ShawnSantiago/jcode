verdict: APPROVE

blockers:
- None.

non_blocking:
- `crates/jcode-app-core/src/tool/pr_watch.rs:2403-2423`, `2629-2677`, `3254-3284`: the MVP coalescing path now schedules one deduped follow-up per watch, and that follow-up executes `webhook_heartbeat`, which calls `webhook_refresh_watch`.
- `crates/jcode-app-core/src/tool/pr_watch.rs:2546-2627`: `webhook_refresh_watch` now owns the lock-before-load refresh boundary with root/repo/PR/watch validation before collection and write.
- `crates/jcode-app-core/src/tool/pr_watch.rs:4791-4893`: tool-level `webhook_doctor` now checks required hook events and emits daemon, hook, and tunnel signals. Full live tunnel probing beyond the configured URL/last-response heuristic can remain follow-up polish.
- Full bounded collapsed-reason detail, retry/backoff policy, and richer status metadata can remain follow-ups beyond the accepted single-follow-up MVP debounce/coalesce mechanism.

validation_reviewed:
- Reviewed reported validations: `scripts/dev_cargo.sh test -p jcode-pr-watch-core -p jcode-app-core pr_watch --lib`, `scripts/dev_cargo.sh check -p jcode --bin jcode`.
