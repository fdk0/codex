# Custom Codex Fork Handoff

This document is the durable handoff for the custom Codex fork in this repository. Read it after
the repository `AGENTS.md` at the start of any new maintenance, debugging, build, or upstream-sync
thread.

It records a point-in-time snapshot, not a substitute for inspecting the current tree. Before
acting, verify the branch, commit, version, working tree, and upstream merge base.

## New-thread bootstrap

Use this opening instruction in a replacement thread:

```text
Read AGENTS.md and CUSTOM_FORK_HANDOFF.md first. Verify the current Git state and treat the
handoff snapshot as evidence rather than blindly assuming it is still current. Preserve the
custom-fork invariants when syncing or fixing the repository. If upstream now implements an
equivalent or better feature, prefer the upstream implementation and retire the duplicate local
implementation without dropping the invariant or its regression coverage.
```

Then run:

```bash
cd /Users/fdk0/git/codex
git status --short --branch
git branch --show-current
git log -5 --oneline --decorate
git remote -v
git merge-base upstream/main HEAD
rg -n '^version = ' codex-rs/Cargo.toml | head -n 5
```

## Snapshot at 2026-07-12

- Custom branch: `feature/upstream-native-wake-dashboard`
- Custom branch commit: `2913293190`
- Local `main`: `21c780399f`
- Upstream merge base: `d72d669ca7`
- Rust workspace version: `0.144.0`
- At this snapshot, the feature branch is 120 commits ahead of that upstream merge base.
- Remote layout:
  - `origin`: `https://github.com/fdk0/codex.git`
  - `upstream`: `https://github.com/openai/codex.git`

Never copy these identifiers into a future merge command without rechecking them.

## MultiAgentV2 tool namespace

The configuration key is `tool_namespace`, not `tool_namespae`:

```toml
[features.multi_agent_v2]
enabled = true
tool_namespace = "agents"
```

When the active provider supports Responses API namespaces, this groups the seven MultiAgentV2
tools under the logical `agents` namespace:

- `agents.spawn_agent`
- `agents.send_message`
- `agents.followup_task`
- `agents.wait_agent`
- `agents.interrupt_agent`
- `agents.list_agents`
- `agents.close_agent`

Some clients render these as `functions.agents.<tool>`. The namespace changes tool naming and
grouping only. It does not change thread topology, roles, wake policy, concurrency, or agent
configuration. Providers without namespace-tool support fall back to ordinary function tools.

Upstream's default namespace is `collaboration`. The Responses API reserves that namespace for
the exact upstream schemas. This fork extends the schemas with wake controls, environment
overrides, targeted waits, plaintext-message behavior, and `close_agent`. Advertising those
extended schemas as `collaboration.*` caused HTTP 400 request-validation errors.

Commit `2913293190` therefore applies this compatibility rule:

- configured `collaboration` -> expose the custom family as plain function tools;
- configured non-reserved namespace such as `agents` -> preserve namespaced tools;
- never place a locally extended schema back into reserved `collaboration` unless the backend's
  canonical schema has become exactly compatible.

Relevant source:

- `codex-rs/core/src/tools/spec_plan.rs`
- `codex-rs/core/src/tools/spec_plan_tests.rs`
- `codex-rs/core/src/config/mod.rs`

## Subagent model and reasoning selection

### Current behavior

MultiAgentV2 already supports parent-selected `model`, `reasoning_effort`, and `service_tier`.
This is upstream behavior, not a missing custom feature. The v2 schema and handler are in:

- `codex-rs/core/src/tools/handlers/multi_agents_spec.rs`
- `codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs`
- `codex-rs/core/src/tools/handlers/multi_agents_common.rs`

It can appear unavailable because upstream intentionally defaults this setting to `true`:

```toml
[features.multi_agent_v2]
hide_spawn_agent_metadata = true
```

That removes `agent_type`, `model`, `reasoning_effort`, and `service_tier` from the model-visible
v2 tool schema. V1 explicitly keeps those selectors visible for compatibility.

To let the parent choose them in V2, use:

```toml
[features.multi_agent_v2]
enabled = true
hide_spawn_agent_metadata = false
```

The current macOS user config already had `hide_spawn_agent_metadata = false` at this snapshot.
Start a fresh thread after changing it so the new thread receives the updated tool schema. The
multi-agent generation is pinned per thread; an older thread can continue exposing V1 even after
`features.multi_agent_v2.enabled = true` is present in the current config.

Example:

```text
spawn_agent(
  task_name="repo_scan",
  agent_type="explorer",
  fork_turns="none",
  model="gpt-5.6-luna",
  reasoning_effort="low",
  wake_parent_on_completion=true,
  message="Inspect the requested files and return concise findings."
)
```

### Constraints

1. `fork_turns="all"` is a full-history fork and intentionally inherits the parent agent type,
   model, and reasoning effort. V2 also defaults omitted `fork_turns` to `all`. To override model
   or reasoning, use `fork_turns="none"` or a positive turn-count string.
2. The selected model must be in the available model catalog.
3. The requested reasoning effort must be supported by the selected model.
4. A role file that explicitly sets `model` or `model_reasoning_effort` owns those settings and
   takes precedence. Remove the locked keys from that role, or define a separate role, if the
   parent should choose dynamically.
5. If metadata remains hidden, adding fields only to the handler is insufficient: the model cannot
   legally send fields omitted from the tool schema.

Upstream commit anchors:

- `91ca20c7c3`: added spawn model/reasoning overrides.
- `668703c23f`: made hidden V2 spawn metadata the default.
- `66232220e2`: explicitly kept V1 metadata visible.

No Rust change is currently required to enable parent-selected model/reasoning in V2. Prefer the
configuration switch over changing the upstream default globally.

## Custom-fork behavior that must be preserved

The following are the behavior-level invariants. Some implementation details may be replaced by
upstream equivalents during future syncs, but the behavior and regression coverage must remain.

### 1. Wake-on-completion and leaf topology

Default behavior without explicit `config.toml` keys:

```toml
[agents]
wake_parent_on_completion_default = true
wait_on_wake_enabled = "reject"
wake_descendant_policy = "leaf_only"
```

The keys may be omitted because these are compiled defaults in this fork.

Required invariants:

- A wake-enabled child wakes its immediate parent exactly once for each completed generation.
- Reusing a completed child rearms the watcher for the new follow-up generation.
- Parent unload/reload, child resume, compaction, deferred mailbox delivery, and temporary missing
  subscriptions must not lose the wake.
- `leaf_only` prevents a child from waking its parent while that child still has active
  descendants. A grandchild must not skip the immediate parent and wake the root directly.
- Duplicate live/history notifications are suppressed without suppressing the actual wake input.
- A parent should end its turn and rely on wake delivery instead of polling a wake-enabled child;
  `wait_agent` rejects that misuse by default.

Primary implementation and tests:

- `codex-rs/core/src/agent/control/wake.rs`
- `codex-rs/core/src/agent/control/spawn.rs`
- `codex-rs/core/src/agent/control.rs`
- `codex-rs/core/src/agent/control_tests.rs`
- `codex-rs/core/src/tools/handlers/multi_agents_v2/wait.rs`
- `codex-rs/core/tests/suite/subagent_notifications.rs`

Important commit anchors include `a0ea571949`, `8453fb6ace`, `362983783c`, `36d2e55504`,
`ba3ae00171`, `94a021fb51`, `1de060037c`, `ff0d80b1f9`, `c10bce03ae`, `1a6e933471`, and
`c810a8588d`.

### 2. Spawn environment propagation

Both multi-agent generations preserve explicit spawn environment overrides. V2 accepts:

```text
spawn_agent(
  task_name="dispatcher",
  agent_type="dispatcher",
  fork_turns="none",
  wake_parent_on_completion=true,
  env={"CODEX_LANE":"dispatcher","BD_ACTOR":"dispatcher:1"},
  message="..."
)
```

The values are applied through the child's shell environment policy before startup. Preserve the
input schema, deserialization, config mutation, app-server/tool transport, and tests together.

Primary source:

- `codex-rs/core/src/tools/handlers/multi_agents_spec.rs`
- `codex-rs/core/src/tools/handlers/multi_agents_common.rs`
- `codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs`
- commit `868bbcef51`

### 3. MultiAgentV2 operational extensions

This fork keeps the upstream v2 task-name topology and adds or hardens:

- `close_agent` in V2, accepting agent id, relative task name, or canonical task name;
- capacity release when a child is closed;
- live `list_agents` data scoped to the current root tree;
- targeted wait behavior and wake-aware wait rejection;
- plaintext message schemas by default (`encrypted_messages = false`) for compatibility with the
  custom application/model path;
- custom-schema namespace compatibility described above;
- explicit interruption/follow-up handling without losing watcher generations.

Primary source:

- `codex-rs/core/src/tools/handlers/multi_agents_v2/`
- `codex-rs/core/src/tools/handlers/multi_agents_spec.rs`
- `codex-rs/core/src/tools/spec_plan.rs`
- `codex-rs/core/src/agent/control/residency.rs`
- commits `3643c3d6e5`, `ce64f410f5`, `7df651134c`, and `2913293190`

### 4. Turn and task lifecycle hardening

Required invariants:

- A turn must not silently complete while a spawned terminal process remains unresolved.
- Panicked session tasks must enter a terminal error/cleanup path and clear active-turn state.
- App-server `turn/start` rejects an overlapping active turn and directs the caller to steer or
  interrupt instead of creating a duplicate executor.
- Post-shutdown background skills updates must not mutate a dead session.

Primary commits:

- `196f3bc79f`: keep turns alive for pending processes.
- `89d94f89b5` and `0e42c474ae`: complete panicked tasks and wait for cleanup in tests.
- `8fd3dcd7ec`: reject overlapping turn starts.
- `90422e7d15`: stop post-shutdown skills updates.

### 5. Subagent visibility and TUI/app-server parity

Required invariants:

- Nested V2 subagents remain discoverable from the parent/root task tree.
- Spawned thread membership and parent-thread identity survive app-server replay/resume.
- The `/agent` picker refreshes liveness and displays hydrated names/status instead of remaining
  `Agent` indefinitely.
- Replayed wake and subagent notifications are rendered as history cells with preserved Markdown
  and multiline output.
- Mirrored live prompts and completion notices are not duplicated.
- Remote TUI/app-server sessions expose the same thread ancestry needed by local navigation.

Primary areas:

- `codex-rs/app-server/src/`
- `codex-rs/app-server-protocol/src/protocol/thread_history.rs`
- `codex-rs/state/src/runtime/threads.rs`
- `codex-rs/thread-store/src/local/list_threads.rs`
- `codex-rs/tui/src/app/agent_navigation.rs`
- `codex-rs/tui/src/multi_agents.rs`
- `codex-rs/tui/src/history_cell/notices.rs`
- `codex-rs/agent-dashboard/`

Commit anchors include `cf6d818c25`, `1aaae771ff`, `685a3e6fb5`, `b582189f3e`, `7082ad1dd4`,
`798cd4809b`, `7debc0c5ed`, `275ff66b1b`, `6eda09fe30`, and `d2f754e241`.

### 6. Hooks, profiles, and compaction

Required behavior:

- Hook matcher conditions remain functional.
- Hook profile selection uses the active Profile V2 layer for the current root or subagent turn;
  a globally selected driver profile must not incorrectly poison another role.
- Hooks use the selected turn environment cwd rather than stale session cwd.
- `AfterCompaction` exists as a first-class hook event with bounded context injection.
- Hook and wake output preserves multiline/Markdown formatting in the TUI and replay history.
- Profile-backed agent roles resolve Profile V2 files correctly.

Primary source:

- `codex-rs/core/src/hook_runtime.rs`
- `codex-rs/hooks/src/`
- `codex-rs/config/src/hook_config.rs`
- `codex-rs/core/src/agent/role.rs`

Commit anchors include `150c7c004b`, `ac9421b1bf`, `1636d833bf`, `78d6b2716f`, `ae2f61b7e7`,
`ca88e76e88`, `76bd460bb4`, `a3896a58ea`, `07a9f9078e`, and `0e57a4ef2`.

### 7. Profile V2 and per-project profile defaults

This fork supports selecting a Profile V2 file from a user-owned project entry:

```toml
[projects."/absolute/path/to/project"]
trust_level = "trusted"
profile = "bd-driver"
```

The profile file is `${CODEX_HOME}/bd-driver.config.toml`. If the base `config.toml` is a symlink,
the sibling profile file next to the resolved symlink target is also supported.

Do not put top-level `profile = "..."` in project-local `.codex/config.toml`; upstream project
layer filtering intentionally rejects that key. An explicit CLI/app-server profile override wins
over the user-level project mapping.

Primary source and commits:

- `codex-rs/config/src/loader/mod.rs`
- `codex-rs/core/src/config/config_loader_tests.rs`
- `bdf91da621`: per-project profile defaults.
- `68b7321ede`: profile files beside symlinked config.
- `ca88e76e88`: Profile V2 in agent roles.

### 8. Bundled plugins and Computer Use discovery

The custom executable discovers app-bundled plugin marketplaces relative to its executable,
including `Contents/Resources/plugins/openai-bundled`, without requiring a manual marketplace path
in `config.toml`. This restores bundled Chrome/Computer Use visibility in rebuilt macOS apps.

Primary source and commits:

- `codex-rs/core-plugins/src/installed_marketplaces.rs`
- `codex-rs/core-plugins/src/manager.rs`
- `087dd2d324`: bundled marketplace auto-discovery.
- `0b623fce33`: bundled Computer Use visibility.

The helper application's macOS signing and authorization patching is maintained outside this
repository by the script described under **Build and application packaging**.

### 9. Remote-control and daemon compatibility

This branch contains app-server daemon, remote-control client-name, transport, and remote TUI
compatibility work. Upstream has since added substantial remote-control support, so future merges
must compare behavior rather than preserving old code mechanically.

Preserve these outcomes:

- the intended custom binary is the one started for a remote environment;
- daemon ownership and stale control sockets are detected rather than creating a second server;
- remote-control client names survive daemon/bootstrap startup;
- app-server and UI consumers receive correct server version, cwd, parent-thread, and task state;
- only one executor owns an active task turn.

Relevant areas include `codex-rs/app-server-daemon/`, `codex-rs/cli/src/remote_control_cmd.rs`, and
`codex-rs/app-server/src/`. Commit `6c7ed7c9f6` is the local client-name anchor.

### 10. Supporting compatibility fixes

Other intentionally retained fixes include:

- stale `apply_patch` self-executable fallback (`e31c4af486`);
- system `bwrap` discovery (`3058b0e96f`);
- sandbox cwd serialized as a file URI for MCP (`7cbe60849a`);
- low-level persistent log filtering (`716aae7fc5`);
- remote cwd not inferred incorrectly by the TUI (`50f4a319cf`);
- custom package version kept aligned with the upstream release (`83d4a78888`).

## Upstream synchronization procedure

The requested topology is always:

1. synchronize local `main` with `upstream/main` or the explicitly requested stable release;
2. merge local `main` into `feature/upstream-native-wake-dashboard`;
3. preserve local behavior unless upstream now provides the same or better behavior;
4. use upstream's implementation when equivalent, removing duplicate local code cleanly;
5. update the workspace/package version to the upstream release version.

### 1. Preflight and backups

Do not begin with a dirty tree. Commit the intended work first; do not stash or discard unknown
user changes.

```bash
cd /Users/fdk0/git/codex
git status --short --branch
git branch --show-current
git fetch upstream --tags
git fetch origin

stamp="$(date +%Y%m%dT%H%M%S)"
git branch "backup/pre-upstream-sync-${stamp}-main" main
git branch "backup/pre-upstream-sync-${stamp}-feature" feature/upstream-native-wake-dashboard
```

### 2. Synchronize `main`

```bash
git switch main
git merge --no-edit upstream/main
```

If the request targets a specific stable release tag, verify the tag, release version, and commit
before merging it. Do not silently select an alpha release.

### 3. Merge `main` into the custom branch

```bash
git switch feature/upstream-native-wake-dashboard
git merge --no-edit main
```

Do not rebase the shared custom branch unless the user explicitly requests a history rewrite.

### 4. Resolve conflicts semantically

Never resolve a broad conflict with blanket `ours` or `theirs` selection. For every conflict:

1. identify the upstream architectural change;
2. identify the local invariant from this document and its tests;
3. use upstream code directly if it now satisfies the invariant;
4. otherwise port the smallest local behavior into the new upstream architecture;
5. remove obsolete duplicate implementation rather than leaving two paths;
6. update tests to exercise the resulting single path.

Frequent conflict areas:

- `codex-rs/Cargo.toml` and `codex-rs/Cargo.lock`;
- multi-agent tool specs and `spec_plan.rs`;
- `agent/control`, spawn, wake, and residency;
- app-server thread item construction and parent-thread fields;
- TUI agent navigation/history replay;
- hook runtime and profile config loading.

### 5. Version alignment

Verify, do not assume, the final version:

```bash
rg -n '^version = ' codex-rs/Cargo.toml | head -n 5
rg -n 'name = "codex-cli"|version = "[0-9]' codex-rs/Cargo.lock | head -n 20
```

The custom binary must report the synchronized upstream release version after installation.

### 6. Regression review before building

At minimum, inspect:

```bash
git status --short --branch
git diff --check
git log --oneline --decorate main..HEAD
git diff --stat "$(git merge-base upstream/main HEAD)"..HEAD
```

Search the final tree for the invariants rather than relying only on commit presence. Merge commits
can preserve a subject while losing behavior during conflict resolution.

### 7. Validation strategy

Run the narrowest affected tests first through the output reducer. Examples:

```bash
cd /Users/fdk0/git/codex/codex-rs
just fmt

"${CODEX_HOME:-$HOME/.codex}/skills/tool-output-reducer/scripts/toolwrap" run --kind test -- \
  just test -p codex-core multi_agent

"${CODEX_HOME:-$HOME/.codex}/skills/tool-output-reducer/scripts/toolwrap" run --kind test -- \
  just test -p codex-app-server
```

Select test filters that match the actual conflict set. Do not start the complete workspace suite
without user approval. Do not use `cargo test` directly.

Required regression themes after a multi-agent sync:

- v1 and v2 tool-family selection;
- custom schema never advertised in reserved `collaboration`;
- `agents` namespace still works;
- model/reasoning fields visible when metadata hiding is disabled;
- full-history override rejection;
- environment propagation;
- one wake per generation;
- reused-child watcher rearm;
- leaf-only descendant gating;
- wake survival across unload/resume;
- close releases capacity;
- nested thread membership visible through app-server/TUI;
- no duplicate notification or mirrored prompt;
- pending process and panic cleanup keep task state coherent;
- active Profile V2 controls hook matching.

## Build and application packaging

### Preferred macOS path

The maintained packaging script is:

```text
/Users/fdk0/bin/codexcustom-app
```

It is tracked in the dotfiles repository as `bin/codexcustom-app`; packaging changes were committed
there as `de0d0d50`.

Important paths:

- source app: `/Applications/ChatGPT.app`
- rebuilt app: `/Users/fdk0/Applications/CustomChatGPT.app`
- custom CLI: `/Users/fdk0/.local/share/codex-binaries/codex-custom`
- code-mode host aliases:
  - `/Users/fdk0/.local/bin/codex-code-mode-host`
  - `/Users/fdk0/.local/share/codex-binaries/codex-code-mode-host`
- app-bundled host:
  - `/Users/fdk0/Applications/CustomChatGPT.app/Contents/Resources/codex-code-mode-host`

To build both required Rust executables and rebuild the app:

```bash
/Users/fdk0/bin/codexcustom-app full-rebuild-run
```

If the custom binaries are already current and only the app must be repackaged:

```bash
/Users/fdk0/bin/codexcustom-app rebuild-run
```

The script must continue to build, install, embed, and sign both `codex` and
`codex-code-mode-host`. Upstream made the host stable and enabled by default; installing only
`codex` leaves CLI aliases unable to execute code-mode tools.

### Disk discipline

Rust artifacts have previously exceeded available disk space. Before a broad build, check:

```bash
df -h /Users/fdk0
du -sh /Users/fdk0/git/codex/codex-rs/target 2>/dev/null || true
```

Prefer targeted tests and one release build. After the installed binary and app are verified, use
`cargo clean` only when space recovery is needed and no other active build depends on `target`.
Remember that a clean removes the next build's incremental cache.

## Known failure signatures

### Reserved namespace HTTP 400

```text
Function 'collaboration.<tool>' is reserved for use by this model and must match the configured schema
```

Cause: an extended local schema entered the reserved `collaboration` namespace. Check
`add_collaboration_tools` and the tests around commit `2913293190`.

### V2 parent cannot see model/reasoning fields

Check:

```toml
[features.multi_agent_v2]
hide_spawn_agent_metadata = false
```

Then use a fresh thread and avoid `fork_turns="all"` when overriding.

### Missing code-mode host

```text
failed to spawn code-mode host .../codex-code-mode-host: No such file or directory
```

Cause: `codex` was installed without its required sibling host. Use the updated packaging script or
build/install both binaries.

### Child completes but parent remains stale

Inspect, in order:

1. child terminal status and generation;
2. parent wake subscription and preference;
3. descendant activity under `leaf_only`;
4. watcher rearm after follow-up/resume;
5. mailbox insertion and deferred scheduling;
6. parent task residency/load state;
7. app-server notification replay and duplicate suppression.

Do not patch only the visible TUI notification; delivery into parent model context is a separate
cross-boundary invariant.

### Subagent exists but is absent from the UI

Check root-tree membership, parent-thread identity, state runtime thread listing, app-server thread
history mapping, and TUI navigation hydration. A successful spawn event alone does not prove the
subagent is discoverable after replay.

## Source-of-truth audit commands

Use these to refresh this handoff after future changes:

```bash
cd /Users/fdk0/git/codex
base="$(git merge-base upstream/main HEAD)"
git log --reverse --format='%h%x09%an%x09%s' "$base"..HEAD
git diff --stat "$base"..HEAD
git diff --name-status "$base"..HEAD
git status --short --branch
```

Update this document whenever a custom invariant is removed, replaced by upstream, materially
changed, or moved to a new subsystem. Do not append stale amendments: rewrite the affected section
so it describes only the current behavior.
