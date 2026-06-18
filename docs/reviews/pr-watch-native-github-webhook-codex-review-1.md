verdict: ITERATE

blockers

1. `webhook_register_delivery` cannot be a normal model-callable `pr_watch` action. The plan leaves a path where a tool call can claim signature verification happened, which collapses the trust boundary. Evidence: docs/plans/pr-watch-native-github-webhook-plan.md:141-145. Required fix: make delivery ingestion daemon-only, or require an unforgeable verified-delivery envelope over a local privileged channel that ordinary agent/tool calls cannot mint.

2. The shared refresh boundary is underspecified for current `pr_watch` safety invariants. Existing refresh paths acquire the watch lock, re-read state before write, reject stale writes, schedule handoffs, and may schedule follow-up monitors. Evidence: crates/jcode-app-core/src/tool/pr_watch.rs:2228-2287 and 2411-2461. Required fix: define a dedicated `webhook_refresh_watch` boundary that explicitly preserves lock/stale-write/handoff behavior while disabling normal monitor scheduling in webhook mode.

3. Daemon cwd/root routing is not safe enough. Watch state records `root_dir` on start, but ordinary state loading is store-relative, and root mismatch is currently enforced only for `resolve_addressed`. Evidence: crates/jcode-app-core/src/tool/pr_watch.rs:427-428, 849-890, 3669-3677. Required fix: specify how the daemon finds the correct watch store/root per delivery and rejects or quarantines mismatched roots before writing state.

4. Heartbeat scheduling is not concrete enough to preserve schedule validation. The plan says “existing schedule payload validation or a new read-only heartbeat payload,” but current validation only permits `ack_baseline`, `poll_now`, and `monitor`. Evidence: docs/plans/pr-watch-native-github-webhook-plan.md:198-201 and crates/jcode-app-core/src/tool/pr_watch.rs:178-220. Required fix: define the exact heartbeat payload, action enum, schedule kind/key, validation whitelist, dedupe/cancel behavior, and tests proving it cannot schedule mutations or normal 5-minute monitor cycles.

required revisions

- Resolve the MVP open questions inside the plan instead of leaving them open. In particular, daemon vs internal registration, heartbeat default, auto-start behavior, hook installation timing, and global vs per-repo health storage affect security and routing. Evidence: docs/plans/pr-watch-native-github-webhook-plan.md:337-345.

- Add daemon lifecycle requirements: single-instance behavior, pid/health file location, stale pid handling, graceful shutdown, port collision behavior, log location under the repo’s logging conventions, and how `webhook status/doctor` distinguishes daemon-down from tunnel-down.

- Add event authorization/routing constraints: only accept configured GitHub repos, validate `X-GitHub-Event`, `X-GitHub-Delivery`, content type, body size before parse, and treat repo/PR extracted from payload as untrusted until matched to an existing watch or explicit auto-start config.

- Add backpressure/rate-limit behavior for burst events. The plan has debounce, but it needs bounded queue size, per-repo concurrency limits, retry policy, and what status reports when events are dropped or collapsed.

- Tighten testability around the daemon. Add tests for unsigned body rejection before routing, ordinary tool-call spoof rejection, root mismatch rejection, lock contention follow-up refresh, heartbeat payload validation, daemon restart/dedupe persistence, and webhook mode not calling `maybe_schedule_next_monitor`.

optional improvements

- Split implementation into a smaller approval slice: event model, verifier/parser, daemon-only verified envelope, shared refresh boundary, and webhook mode for existing watches only. Defer GitHub hook doctor/install polish until after the trust boundary is proven.

- Store daemon delivery logs separately from watch state, then copy only sanitized delivery metadata into watch state. That keeps ignored/failed deliveries observable without mutating arbitrary watch files.

- Prefer default heartbeat disabled or explicitly user-selected during rollout; if enabled by default, make the 2-hour default a named constant with status output and tests.
