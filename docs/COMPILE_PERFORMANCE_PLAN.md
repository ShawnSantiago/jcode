# Compile Performance Plan

This document tracks the plan to make jcode's self-dev / refactor loop much faster
without sacrificing full-feature builds.

## Goals

- Keep full-featured builds available for normal usage and self-dev reloads.
- Make common self-dev edits significantly cheaper to compile.
- Reduce how often customizations require recompilation at all.
- Measure improvements after each phase and stop churn that does not pay off.

## Current Baseline (2026-03-24)

Measured locally on the current tree:

- Warm `cargo check --quiet`: **~8.5s**
- Warm `scripts/dev_cargo.sh build --release -p jcode --bin jcode --quiet`: **~47.3s**

Additional observations from this audit:

- A previous warm-ish `cargo check` run landed around **~12.3s**.
- A less-warm `cargo check --timings` run landed around **~23.8s**.
- The previous local default `clang + mold` setup failed during release linking on this machine.
- `clang + lld` links the release `jcode` binary successfully here.

## Near-Term Targets

For common self-dev edits that do **not** touch broad shared interfaces:

- Warm `cargo check`: **< 5s**
- Warm `cargo build` / reload-oriented build: **< 20–30s**

For shared/core edits we should still aim to stay materially below today's baseline,
even if they cannot reach the same fast path.

## What Matters Most (ranked)

1. **Workspace / crate boundaries**
   - Rust caches best at the crate boundary.
   - Heavy untouched subsystems should remain compiled and reusable in full builds.
2. **Good boundary design**
   - High-churn logic should not live in broad fanout crates or unstable shared types.
3. **`sccache`**
   - Practical win for repeated local builds and CI.
4. **Fast, reliable linker configuration**
   - Especially important for `cargo build` and release/self-dev reload builds.
5. **Heavy subsystem isolation**
   - Embeddings, provider implementations, and large TUI/rendering code should stop
     churning unrelated builds.
6. **Narrower build targets for inner loops**
   - Avoid rebuilding extra bins/targets when not needed.
7. **Reduce the need to recompile at all**
   - Issue #32's customization records and extension points should make many changes
     config/hook/skill/data driven rather than source driven.

## Execution Plan

### Phase 1 — Tactical build speed wins

- Keep `.cargo/config.toml` conservative for local contributors.
- Use `scripts/dev_cargo.sh` for local self-dev builds:
  - enables `sccache` automatically if installed
  - prefers `clang + lld` on Linux x86_64
  - can still opt into `mold` via `JCODE_FAST_LINKER=mold`
- Route refactor-shadow builds through that wrapper.

### Phase 2 — Measurement and repeatability

- Add documented commands for cold/warm `check` and `build` timing.
- Prefer touched-file timings (for example `scripts/bench_compile.sh check --touch src/server.rs`) over no-op hot-cache reruns when judging ROI.
- Track timing deltas after each structural phase.
- Fix build/link blockers before treating any timing data as authoritative.

### Phase 3 — Workspace boundary design

Proposed destination layout:

- `jcode-core`
  - protocol, ids, message types, config primitives, shared utility types
- `jcode-server`
  - server lifecycle, reload, socket, swarm, daemon behaviors
- `jcode-agent`
  - agent turn loop, tool orchestration, stream handling
- `jcode-provider`
  - provider traits, shared provider types, routing/catalog support
- `jcode-embedding`
  - embedding model integration and related heavy inference dependencies
- `jcode-tui`
  - TUI rendering, widgets, state reduction, terminal UI support
- `jcode-selfdev`
  - customization records, migration logic, self-dev productization

### Phase 4 — First crate splits

Start with the highest-leverage cache boundaries:

1. `jcode-embedding`
2. provider support / provider implementation splits
3. self-dev/customization system once the new extension-point work lands
4. server / agent split along the seams already being extracted

### Phase 4a — First workspace boundary landed

- 2026-03-24: moved the heavy ONNX/tokenizer implementation into the new
  `crates/jcode-embedding` workspace crate.
- The main `src/embedding.rs` module now acts as a facade for process-local
  cache/stats/path/logging integration.
- This preserves the public `crate::embedding` API while creating a real Cargo
  cache boundary for the heaviest embedding dependencies.
- Follow-up: gather more realistic before/after timing data using controlled
  touched-file benchmarks rather than fully hot no-op rebuilds.

### Phase 5 — Reduce invalidation pressure

- Continue shrinking giant hotspot files.
- Keep high-churn code out of stable low-level crates.
- Avoid changing shared broad fanout types casually.

### Phase 6 — Reduce recompilation demand via issue #32

- Store customization intent, provenance, validation, and migration hints.
- Add extension points so more user changes live in:
  - config
  - hooks
  - skills
  - prompt overlays
  - routing/theme/layout data
- Prefer those over direct Rust source edits whenever possible.

## Developer Workflow Guidance

### Fast local cargo wrapper

Use:

```bash
scripts/dev_cargo.sh check --quiet
scripts/dev_cargo.sh build --release -p jcode --bin jcode --quiet
```

The wrapper:

- uses `sccache` automatically when available
- prefers `lld` locally on Linux x86_64
- avoids hard-forcing a linker mode that may be broken on a given machine

Override linker mode explicitly when needed:

```bash
JCODE_FAST_LINKER=lld scripts/dev_cargo.sh build --release -p jcode --bin jcode
JCODE_FAST_LINKER=mold scripts/dev_cargo.sh build --release -p jcode --bin jcode
JCODE_FAST_LINKER=system scripts/dev_cargo.sh build --release -p jcode --bin jcode
```

## Stop Conditions

After each structural phase we should re-measure and ask:

- Did warm `check` time improve materially?
- Did warm `build` / reload-oriented build time improve materially?
- Did we reduce rebuild scope for common self-dev edits?

If not, we should avoid continuing high-churn refactors on compile-time grounds alone.
