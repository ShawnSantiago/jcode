# Native GitHub Webhook PR Watch Plan

## Status
Codex pass 1 returned `ITERATE`. This revision closes the trust-boundary, refresh-boundary, root-routing, heartbeat, daemon lifecycle, event authorization, and backpressure blockers. Pending Codex pass 2 and Claude final review.

## Codex pass 1 revision summary
Codex identified four blockers:

1. Delivery ingestion could not be model-callable because that would let a normal tool call spoof signature verification.
2. The shared refresh boundary did not explicitly preserve current `pr_watch` lock, stale-write, handoff, and scheduling invariants.
3. Daemon repo/root routing was unsafe because current state loading is store-relative and root mismatch is mainly enforced for `resolve_addressed`.
4. Heartbeat scheduling was too vague and risked weakening schedule payload validation.

This revision resolves those blockers by making delivery ingestion daemon-only, defining a dedicated `webhook_refresh_watch` boundary, introducing a daemon watch-index/root-routing model, defining exact heartbeat payload/action semantics, and resolving all MVP open questions.

## Goal
Replace routine interval polling in `pr_watch` with a first-class GitHub webhook event source while preserving the existing PR watch state machine, grant-gated mutation safety, and watch-and-resolve workflow.

Desired end state:

1. GitHub events wake Jcode immediately when PR feedback, check status, review activity, or PR head changes occur.
2. Jcode coalesces bursty events and performs one authoritative read-only refresh for the affected `repo#pr`.
3. Native `.jcode/pr-feedback-watch/*-state.json` state is updated by the same classification logic used today.
4. Action-required handoffs and `resolve_addressed` continue to work through existing safety gates.
5. Routine 5-minute monitor polling is disabled for webhook-mode watches.
6. Optional low-frequency heartbeat is available as an explicitly configured read-only safety net.

## Current evidence

### Native Jcode PR watch
- `crates/jcode-app-core/src/tool/pr_watch.rs` implements `pr_watch` as a local state machine.
- The current collector uses `gh` and GitHub APIs for PR metadata, checks, issue comments, review comments, reviews, and GraphQL review threads.
- `PrWatchSchedulePayload::validate_against_state` deliberately allows only read-only scheduled actions: `ack_baseline`, `poll_now`, and `monitor`.
- Scheduled monitor cycles are read-only and must remain read-only.
- `resolve_addressed` is a separate grant-gated remote mutation path and must remain separate from event ingestion.

### Cakepage webhook bridge
- `/home/shawn/projects/cakepage/docs/development/github-pr-watch-webhook.md` documents a repo-local `adnanh/webhook` bridge.
- `/home/shawn/projects/cakepage/scripts/pr-watch-webhook/github-pr-watch.sh` verifies webhook delivery, writes markdown snapshots, and optionally starts `jcode run`.
- That shell bridge does not update native `pr_watch` state and does not integrate with grants, handoffs, or `resolve_addressed`.
- Current cakepage GitHub hook points to `https://helpless-nautical-bamboo.ngrok-free.dev/hooks/github-pr-watch`, is active, but GitHub reports last response `404 Invalid HTTP Response`.
- Local cakepage webhook and ngrok pid files are dead. Latest local bridge logs are stale.

## RALPLAN-DR summary

### Principles
1. GitHub webhooks are wake signals, not the source of truth.
2. Native `pr_watch` state remains authoritative.
3. Webhook-triggered refreshes are read-only and must not grant mutation authority.
4. Mutating actions stay explicit and grant-gated.
5. Event-driven mode should reduce polling, not reduce safety or observability.
6. Delivery systems must be diagnosable because webhook failure otherwise becomes silent stalling.
7. Model-callable tools must not be able to spoof verified GitHub deliveries.

### Decision drivers
1. The user wants to stop routine polling and use GitHub webhooks instead.
2. GitHub webhook payloads are partial, duplicated, bursty, and sometimes out of order.
3. Review-thread resolution state requires GraphQL and cannot be trusted from every event payload.
4. Existing PR watch state and tests already encode substantial safety behavior.
5. Local tunnels and repo-local shell bridges have demonstrated operational fragility.

### Final MVP decisions
1. **Daemon vs internal registration:** MVP includes a native daemon-only HTTP receiver. No model-callable `webhook_register_delivery` action will exist.
2. **Heartbeat default:** default disabled. Users may opt into a named constant `WEBHOOK_DEFAULT_HEARTBEAT_SECONDS = 7200` using `fallback_heartbeat_seconds=7200` or a CLI flag.
3. **Auto-start behavior:** disabled in MVP. Webhook deliveries route only to existing watches or explicitly configured repo watches in a daemon watch index. Unknown PRs are logged as ignored.
4. **GitHub hook installation:** doctor/status only in MVP. Hook install/update is deferred to a later separable phase.
5. **Health storage:** global daemon health plus per-watch sanitized delivery metadata. Raw/ignored deliveries stay in daemon logs, not arbitrary watch state files.

## Scope

### In scope
- Add a native daemon-only webhook event source for `pr_watch`.
- Add a local HTTP webhook daemon command for GitHub deliveries.
- Verify GitHub `X-Hub-Signature-256` using a configured secret before routing.
- Deduplicate GitHub deliveries by delivery ID.
- Map GitHub events to existing watched `repo#pr` refresh requests.
- Debounce/coalesce bursts of events per watch.
- Trigger a dedicated read-only `webhook_refresh_watch` boundary after verified deliveries.
- Add webhook-mode watch state and status/doctor visibility.
- Disable routine 5-minute polling for webhook-mode watches.
- Add optional low-frequency read-only heartbeat as a safety net.
- Keep scheduled monitor and mutation boundaries intact.
- Add tests using fake payloads and fake GitHub collection.

### Out of scope
- Merging PRs.
- Making webhook events perform commits, pushes, comments, or thread resolution.
- Replacing authoritative GitHub refresh with webhook payload-only state updates.
- Shipping a hosted cloud relay service.
- GitHub hook create/update in the MVP.
- Auto-starting watches for unknown PRs in the MVP.
- Removing `poll_now` or manual monitor support.

## Proposed architecture

```text
GitHub webhook delivery
  -> public URL / tunnel / reverse proxy
  -> jcode webhook daemon
  -> content-type/body-size/header checks
  -> HMAC signature verification
  -> daemon-only verified delivery envelope
  -> delivery dedupe store
  -> watch-index lookup and root validation
  -> per repo#pr debounce queue
  -> webhook_refresh_watch(read-only)
  -> existing state classification
  -> existing action-required handoff / quiet-cycle logic
```

## Trust boundary

### Daemon-only delivery ingestion
No public or model-callable `pr_watch` action may claim that a webhook delivery was verified.

The only component allowed to create a `VerifiedGithubDelivery` is the native daemon after all of these checks pass:

1. request body length is within `WEBHOOK_MAX_BODY_BYTES`,
2. `Content-Type` is exactly `application/json`, `application/json; charset=utf-8`, or starts with `application/json;` with only charset parameters; other media types are rejected,
3. `X-GitHub-Event` is present and in the supported/ignored set,
4. `X-GitHub-Delivery` is present and syntactically valid,
5. `X-Hub-Signature-256` is present,
6. HMAC SHA-256 verification passes with constant-time comparison,
7. body parses as JSON only after signature verification.

`VerifiedGithubDelivery` is an internal Rust type, not deserializable from tool input. If any later design needs out-of-process delivery forwarding, it must use a privileged local socket with OS file permissions and a daemon-minted nonce, not the normal model tool API.

### Webhook delivery never grants writes
Webhook receipt can only trigger read-only refresh and handoff scheduling. It must never add `push`, `comment`, or `resolve_threads` grants, and it must never call `resolve_addressed`.

## Watch index and root routing

The daemon cannot infer a watch store by its current working directory alone.

Add a daemon watch index under a global Jcode runtime directory, for example:

```text
~/.jcode/pr-watch/webhook-index.json
~/.jcode/pr-watch/webhook-deliveries.jsonl
~/.jcode/pr-watch/webhook-daemon-health.json
```

Each index entry contains sanitized routing information:

```json
{
  "watch_id": "ShawnSantiago~2fcakepage-pr-246",
  "repo": "ShawnSantiago/cakepage",
  "pr": 246,
  "root_dir": "/home/shawn/projects/cakepage",
  "state_path": "/home/shawn/projects/cakepage/.jcode/pr-feedback-watch/ShawnSantiago~2fcakepage-pr-246-state.json",
  "event_mode": "Webhook"
}
```

Index maintenance:
- `pr_watch start event_mode=webhook` registers or updates the entry.
- `pr_watch stop` removes or marks the entry inactive.
- state load validates that `state.root_dir` matches `root_dir` and that `state.pr.repo/pr` matches the delivery target before writing.
- if the state file is missing, root mismatched, repo mismatched, or watch terminalized, the daemon records an ignored/quarantined delivery and does not create a new state file.

This closes the cwd/root mismatch class seen when watches are inspected from the wrong repo.

## Shared refresh boundary

Add a dedicated internal function:

```rust
async fn webhook_refresh_watch(
    index_entry: &WebhookWatchIndexEntry,
    delivery: &VerifiedGithubDelivery,
    reason: WebhookRefreshReason,
) -> Result<WebhookRefreshOutcome>;
```

Required invariants:
1. Acquire the same per-watch lock used by `poll_now` before loading mutable state.
2. Re-read state after lock acquisition.
3. Validate root, repo, PR, watch ID, and non-terminal status before collection.
4. Run the same authoritative GitHub collection/classification logic as `poll_now`.
5. Preserve stale-write protection using loaded `updated_at` and `cycle_number` or an equivalent state version check.
6. Preserve action-required handoff scheduling behavior.
7. Do **not** call normal `maybe_schedule_next_monitor` for webhook-mode watches.
8. If `fallback_heartbeat_seconds` is configured, schedule only the heartbeat payload defined below.
9. Persist sanitized delivery metadata into the watch state only after successful routing validation.
10. If lock acquisition fails, mark one bounded follow-up refresh requested for the watch rather than dropping the event silently.

`poll_now` remains available and may be used manually. The webhook path must not bypass existing classification or handoff code.

## Core model changes

Add event source state to `jcode-pr-watch-core` with backward-compatible defaults:

```rust
pub enum PrWatchEventMode {
    Polling,
    Webhook,
    Hybrid,
}

pub struct WebhookWatchState {
    pub enabled: bool,
    pub mode: PrWatchEventMode,
    pub last_delivery_id: Option<String>,
    pub last_delivery_at: Option<String>,
    pub last_event_type: Option<String>,
    pub last_event_action: Option<String>,
    pub last_delivery_status: Option<String>,
    pub consecutive_delivery_failures: u64,
    pub fallback_heartbeat_seconds: Option<u64>,
    pub webhook_url_hint: Option<String>,
    pub collapsed_event_count: u64,
    pub dropped_event_count: u64,
}
```

Existing state JSON defaults to `Polling` with webhook disabled.

## New command surface

Add CLI/service commands:

```bash
jcode pr-watch webhook serve --port 9000 --secret-env GITHUB_WEBHOOK_SECRET
jcode pr-watch webhook status
jcode pr-watch webhook doctor --repo OWNER/REPO
```

Tool exposure may include read-only status/doctor actions later, but not delivery ingestion.

The daemon must run outside normal agent turns and should not require an interactive session to keep receiving deliveries.

## Daemon lifecycle requirements

- Startup fails closed if `GITHUB_WEBHOOK_SECRET` or the configured secret source is unset or empty. Secret rotation requires daemon restart; deliveries signed with a new secret fail verification until restart.
- Single-instance by bind address and port plus daemon lock file.
- Default bind address: `127.0.0.1`; non-local bind requires explicit flag.
- PID file: `~/.jcode/pr-watch/webhook-daemon.pid`.
- Health file: `~/.jcode/pr-watch/webhook-daemon-health.json`.
- Log file: `~/.jcode/logs/pr-watch-webhook-YYYY-MM-DD.log` or equivalent existing log convention.
- Stale PID handling: if PID file exists but process is dead, replace it and record stale PID cleanup.
- Port collision: fail with clear diagnostic and do not overwrite health as running.
- Graceful shutdown: handle SIGINT/SIGTERM, stop accepting new deliveries, finish in-flight refreshes up to timeout, write stopped status.
- Status distinguishes:
  - daemon down: no alive PID/health stale,
  - tunnel down: daemon alive but public URL cannot reach local daemon,
  - GitHub hook failing: `gh api repos/:repo/hooks` shows non-2xx last response such as 404,
  - auth failing: `gh auth status` or hook API access fails.

## Watch start mode

Extend watch start/reschedule options:

```text
pr_watch action=start repo=OWNER/REPO pr=N event_mode=webhook fallback_heartbeat_seconds=7200
```

Behavior:
- `Polling`: current behavior.
- `Webhook`: no routine 5-minute scheduled monitor. Verified webhook deliveries trigger refreshes. Optional heartbeat schedules low-frequency read-only checks.
- `Hybrid`: keep current polling/normal monitor scheduling and also accept webhook wakeups, useful during rollout. Webhook-triggered refresh itself must not schedule an extra normal monitor; the existing polling cadence remains responsible for monitor follow-ups. Tests must assert this distinction.

MVP defaults:
- `event_mode=Polling` unless explicitly set.
- `fallback_heartbeat_seconds=None` unless explicitly set.
- Unknown PR auto-start disabled.

## Webhook payload handling

The daemon handles these events:

| Event | Required extraction | Refresh behavior |
|---|---|---|
| `ping` | repo | health record only |
| `pull_request.opened/reopened/ready_for_review` | repo, PR | refresh only if existing watch/index entry exists |
| `pull_request.synchronize` | repo, PR, head SHA | refresh metadata/checks/threads, reset head-sensitive quiet state through existing classifier |
| `pull_request.closed` | repo, PR | refresh, then terminalize if merged/closed through existing classifier |
| `pull_request_review.submitted/edited/dismissed` | repo, PR | refresh reviews and review threads |
| `pull_request_review_comment.created/edited/deleted` | repo, PR | refresh review comments and review threads |
| `issue_comment.created/edited/deleted` | repo, issue number only if PR | refresh issue comments |
| `check_run.created/completed/rerequested/requested_action` | repo, associated PRs | refresh checks after debounce |
| `check_suite.completed` | repo, associated PRs if present | refresh indexed associated PRs after debounce; if no associated PRs are present, log ignored delivery with reason `check_suite_without_pr` and do not scan all open PRs |
| `status` | repo, SHA | resolve SHA to associated existing watched PRs, refresh checks |

Unknown events are logged as ignored and do not update watch files.

## Event authorization and routing constraints

- Only repos present in the daemon allowlist or watch index are routable.
- `repo` and `pr` from payload are untrusted until matched to an index entry.
- For `issue_comment`, only route if `issue.pull_request` exists.
- For `status`, resolve SHA to PRs, then intersect with indexed watched PRs.
- For `check_run`, route all associated PRs that match indexed watched PRs.
- All ignored deliveries are logged to daemon delivery logs with reason, delivery ID, event, repo if available, and timestamp.
- No raw payload body is logged by default.

## Authoritative refresh rule

Do not mutate watch state directly from webhook payload contents except sanitized delivery metadata after routing validation. After routing to a watch, invoke `webhook_refresh_watch`, which runs authoritative collection and classification.

Rationale:
- Review thread resolution and outdated status require GraphQL.
- Check events race with GitHub check-rollup consistency.
- GitHub can send duplicate or out-of-order events.

## Debounce, dedupe, and backpressure

Requirements:
- Deduplicate by `X-GitHub-Delivery` using a bounded persistent local store.
- Store enough delivery IDs to survive daemon restart without replay storms, retaining entries until either the 10,000-ID cap is exceeded or entries are older than 7 days; bounded memory takes priority over indefinite replay protection.
- Coalesce events per `repo#pr` with default debounce `WEBHOOK_DEBOUNCE_MS = 10_000`.
- Bound pending per-watch reasons, e.g. maximum 100 collapsed reasons; after that increment `dropped_event_count` and keep a summary reason.
- Global daemon refresh concurrency is bounded, with per-repo concurrency defaulting to 2 refreshes so one repo cannot starve all others.
- Per-watch concurrency limit is 1 active refresh plus 1 queued follow-up refresh.
- If GitHub API rate limit/transient failure occurs, schedule one exponential backoff retry capped at the fallback heartbeat interval or 5 minutes if heartbeat disabled.
- Status reports collapsed, dropped, last retry, and last refresh failure counts.

## Heartbeat scheduling

Heartbeat is optional and disabled by default.

Add a distinct read-only action and payload:

```rust
PrWatchAction::WebhookHeartbeat

pub struct PrWatchWebhookHeartbeatPayload {
    pub tool: String,              // "pr_watch"
    pub watch_id: String,
    pub repo: String,
    pub pr: u64,
    pub action: String,            // "webhook_heartbeat"
    pub state_file: String,
    pub heartbeat_seconds: u64,
    pub readonly: bool,            // must be true
}
```

Validation:
- Add `webhook_heartbeat` to the read-only schedule whitelist only if `readonly=true`.
- It uses schedule kind `pr_watch.webhook_heartbeat`.
- It uses schedule key `pr_watch:{watch_id}:webhook_heartbeat`.
- It dedupes/cancels the same way normal monitor schedules do, but only for heartbeat keys.
- It invokes the same read-only refresh/classification boundary as manual poll or webhook refresh.
- It must not schedule normal 5-minute monitor cycles afterward.
- It must not create action-required remediation grants.

Tests must prove heartbeat payloads cannot schedule mutations and webhook mode does not call `maybe_schedule_next_monitor`.

## Webhook health and diagnostics

`pr_watch status` should include per-watch fields:

```text
Event source: webhook
Webhook daemon: running|down|stale|unknown
Last delivery: timestamp / delivery id / event/action
Last delivery result: accepted|ignored|signature_failed|routed|refresh_failed
Collapsed events: N
Dropped events: N
Fallback heartbeat: disabled|2h|overdue
```

`webhook doctor` should include global/repo checks:
- daemon pid/socket is alive,
- secret configured and non-empty,
- public URL configured or discoverable,
- GitHub hook exists for repo,
- GitHub hook events include required PR/check/comment events,
- last GitHub delivery is 2xx,
- `gh auth status` works,
- local state store is writable,
- watch index entries point to existing readable root/state files,
- root_dir in state matches the index.

This would have surfaced cakepage's current failure: active GitHub hook with 404 last response plus dead local webhook/ngrok PIDs.

## Security requirements

- Verify `X-Hub-Signature-256` using constant-time comparison.
- Reject unsigned or mismatched deliveries before parsing as trusted input.
- Limit request body size before buffering.
- Avoid logging raw payload bodies by default.
- Redact secrets and tokens from errors.
- Bind to `127.0.0.1` by default unless explicitly configured otherwise.
- Assume public exposure happens via a tunnel/reverse proxy with HTTPS.
- Webhook delivery must not imply any write grant.
- Webhook-triggered refresh is read-only.
- Ordinary model/tool calls cannot mint verified deliveries.

## Failure behavior

- Signature failure: record rejected delivery count in daemon logs, no routing, no watch mutation.
- Unknown PR or unwatched PR: record ignored delivery in daemon logs, no watch mutation.
- Root mismatch: quarantine delivery with root mismatch reason, no watch mutation.
- Lock contention: record one queued follow-up refresh, not unbounded retries.
- Refresh failure: record `last_delivery_status=refresh_failed`, keep watch non-terminal, schedule bounded retry/backoff if configured.
- Daemon down: GitHub hook delivery fails; doctor surfaces failing hook last response.
- Event lost: optional fallback heartbeat catches drift if configured.
- Duplicate events: dedupe store prevents repeated refresh storms.

## Implementation phases

### Phase 1: Event model and shared refresh boundary
- Extract current poll collection/classification into a function callable by polling and webhook paths.
- Add `webhook_refresh_watch` with lock, re-read, root validation, stale-write protection, handoff preservation, and monitor scheduling suppression for webhook mode.
- Add state fields for webhook mode and delivery metadata with defaults.
- Add tests proving existing polling behavior is unchanged.

### Phase 2: Daemon-only verifier/parser
- Add `VerifiedGithubDelivery` internal type.
- Add parser for GitHub headers and supported event payloads.
- Add HMAC SHA-256 verifier with constant-time comparison.
- Add content-type and body-size handling before parse.
- Unit-test event extraction for all supported event types.
- Unit-test that no `pr_watch` tool action can spoof verified delivery ingestion.

### Phase 3: Watch index, root routing, dedupe, and debounce
- Add global webhook watch index.
- Register/update/remove entries from watch start/stop.
- Add root mismatch quarantine behavior.
- Add delivery ID store and per-watch debounce/coalesce queue.
- Add backpressure/rate-limit behavior.
- Add tests for duplicate deliveries, daemon restart dedupe persistence, bursty review events, running-refresh follow-up, and status events resolving SHA to indexed PRs.

### Phase 4: Native webhook daemon
- Add `jcode pr-watch webhook serve` or equivalent service command.
- Wire verified deliveries to the router.
- Persist daemon health metadata.
- Add single-instance lock, pid/health files, stale pid cleanup, graceful shutdown, port-collision health behavior, and structured logs.

### Phase 5: Webhook watch mode and heartbeat
- Add start/status support for `event_mode=webhook|hybrid|polling`.
- Disable routine monitor scheduling in webhook mode.
- Add optional `webhook_heartbeat` read-only action/payload/schedule kind.
- Preserve action-required handoff behavior.

### Phase 6: Doctor and cakepage migration docs
- Add `webhook status` and `webhook doctor`.
- Document replacing cakepage's shell bridge with native Jcode webhook mode.
- Mark existing repo-local webhook scripts as deprecated historical bridge, not deleted in MVP.

### Deferred Phase 7: GitHub hook installer
- Add dry-run hook creation/update later.
- Use `gh api repos/:owner/:repo/hooks`.
- Never print the secret.
- Update existing Jcode hook rather than duplicating hooks.
- Show required GitHub permissions.

## Test plan

### Unit tests
- Signature verification accepts valid GitHub SHA-256 signatures and rejects invalid/missing signatures.
- Payload is not parsed/trusted before signature verification.
- Payload extraction maps each supported event to expected `repo#pr` targets.
- `status` events resolve SHA to associated indexed PRs through fake `gh` response.
- Duplicate delivery IDs are ignored after daemon restart.
- Burst events coalesce to one refresh per debounce window.
- Refresh-in-progress schedules one follow-up refresh, not unbounded recursion.
- Existing scheduled monitor validation still rejects mutating actions.
- Webhook mode does not schedule normal 5-minute monitor cycles.
- Heartbeat mode schedules only low-frequency read-only checks.
- Root mismatch rejects/quarantines before write.
- Missing or unreadable indexed `state_path` fails strict daemon startup or quarantines the entry before delivery routing.
- Ordinary model/tool input cannot spoof verified delivery registration.

### Integration/fake-gh tests
- Webhook delivery triggers the same state classification as `poll_now`.
- Review comment event produces action-required handoff when fake GitHub returns actionable unresolved thread.
- Check-run failure event records failed checks.
- Pull request synchronize event resets head-sensitive state.
- Closed/merged event terminalizes watch after authoritative refresh.
- Lock contention records one follow-up refresh request.
- Rate-limit/transient failure records refresh failure and bounded retry.
- Stale PID cleanup replaces dead PID files and records the cleanup in daemon health.
- Port collision fails startup and records a non-running health status without clobbering an active daemon.
- Missing/unreadable indexed `state_path` refuses daemon startup or quarantines that entry before accepting deliveries, depending on whether strict startup validation is enabled.

### Manual dogfood
- Start daemon on localhost with a generated secret.
- Use a local signed fixture delivery to validate signature and routing.
- Start a webhook-mode watch for a test PR.
- Confirm no normal 5-minute monitor schedule exists.
- Optionally expose via ngrok and GitHub test hook.
- Confirm `pr_watch status` shows last delivery.
- Confirm a real PR review comment wakes the watch within debounce window.
- Confirm doctor reports dead tunnel/hook failures if ngrok is stopped.

## Acceptance criteria
1. A webhook-mode watch can receive a GitHub review comment delivery and update native PR watch state without waiting for a polling interval.
2. No normal 5-minute monitor follow-up is scheduled for webhook-mode watches.
3. Optional heartbeat uses a distinct read-only `webhook_heartbeat` schedule action/payload and cannot schedule mutations.
4. `pr_watch status` and webhook doctor produce distinct signals for daemon down, tunnel down, and GitHub hook failing with non-2xx/404 responses.
5. GitHub webhook payloads never directly cause remote mutations or grants.
6. Existing polling mode behavior and tests remain valid.
7. Existing scheduled monitor payload validation remains intact except for explicitly whitelisting read-only `webhook_heartbeat`.
8. Cakepage's observed failure mode, active GitHub hook returning 404 while local webhook/ngrok pids are dead, would be surfaced by `webhook doctor`.
9. Ordinary model/tool calls cannot spoof verified webhook delivery ingestion.
10. Root mismatch cannot write to the wrong repo's watch state.

## Recommended MVP cut
Implement phases 1 through 6. Defer GitHub hook installer to Phase 7. Route only to existing watches. Default heartbeat disabled, with documented opt-in to 2 hours.
