verdict: APPROVE

**Blockers**
None. All four Codex pass 1 blockers are concretely resolved:
- Trust boundary: `VerifiedGithubDelivery` is daemon-only, non-deserializable from tool input (lines 111-129); webhook deliveries cannot grant writes (lines 128, 386).
- Refresh boundary: `webhook_refresh_watch` defines lock acquisition, re-read, root/repo/PR/terminal validation, authoritative collection, stale-write protection via `updated_at`/`cycle_number`, handoff preservation, monitor-scheduling suppression, sanitized-metadata-after-validation, and bounded follow-up on lock contention (lines 164-188).
- Root routing: Global watch index with `root_dir`/`state_path`, state-load matches `state.root_dir` and `state.pr.repo/pr` before any write, missing/mismatched routes are quarantined (lines 131-162).
- Heartbeat: `PrWatchAction::WebhookHeartbeat`, dedicated payload with `readonly=true` invariant, distinct schedule kind `pr_watch.webhook_heartbeat`, distinct schedule key, mandatory tests proving it cannot schedule mutations or fall through to 5-minute monitor cycles (lines 317-347).

**Required revisions**
None.

**Optional improvements**
1. State explicitly that startup MUST fail closed if `GITHUB_WEBHOOK_SECRET` is unset/empty, and that secret-rotation behavior is "verification fails until restart with new secret" — closes an obvious foot-gun in operations.
2. Define `Hybrid` mode's monitor-scheduling behavior explicitly. The plan disables routine monitor "for webhook-mode watches" (line 80) but `Hybrid` retains polling per line 261; the `webhook_refresh_watch` rule "do not call `maybe_schedule_next_monitor` for webhook-mode" (line 183) is ambiguous for `Hybrid`. Resolve to one of: hybrid keeps monitor scheduling, or hybrid suppresses scheduling but relies on heartbeat — either works, but pick one and assert it in tests.
3. Per-repo concurrency cap of 2 (line 312) could starve other repos under pathological burst from one repo with many watched PRs. Consider a global daemon refresh concurrency cap in addition, even if just documented as "future".
4. Dedupe retention "10,000 IDs or 7 days, whichever is smaller" (line 309) optimizes for bounded memory but accepts replay risk after restart following a quiet week. Confirm "smaller" is intentional vs. "larger".
5. Acceptance criteria #4 (line 491) should cite a specific doctor signal that distinguishes "GitHub hook 404" from "tunnel down" from "daemon down" — the plan describes these distinctions at lines 244-247 but the acceptance criterion only says "visible enough to diagnose"; tighten to require those three distinct status outputs.
6. Add a test that the daemon refuses to start (and writes a non-running health status) when `state_path` referenced in the index does not exist or is unreadable, preventing index drift from silently swallowing deliveries.
