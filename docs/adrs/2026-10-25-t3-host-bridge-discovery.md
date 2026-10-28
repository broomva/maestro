# ADR: T3 Host Identity and Effect Bridge Discovery (BRO-3268)

Date: 2026-10-25
Status: accepted

## Context

Rev 2 of `docs/specs/2026-10-18-maestro-rust.html` asked `host-identity` (BRO-3268) to determine empirically what API or registry T3 exposes to a process on the host for thread identity, working directory / worktree resolution, liveness, and pending runtime permission requests, and to bind caller verification to registry state rather than trusting caller-supplied metadata alone.

## Empirical Findings on Live T3 Host (`0.1.3`)

1. **Runtime metadata & transport endpoint**:
   - `$T3CODE_HOME/userdata/environment-id` holds the stable T3 environment UUID (e.g. `8c5e81ab-e50f-49b4-a7e9-ba471308fac9`).
   - `$T3CODE_HOME/userdata/server-runtime.json` and `$T3CODE_HOME/runtime/service-state.json` record `serverVersion` / `activeVersion` (`0.1.3`), `pid`, `host`, `port`, and `origin`.
   - Inside a T3-hosted session, T3 injects `T3_ACP_MCP_ENDPOINT` (`http://<host>:<port>/mcp`), `T3_ACP_MCP_AUTHORIZATION` (ephemeral bearer token), and `T3_ACP_MCP_NODE`.
2. **Thread, worktree, run status, and runtime permission registry (`statev2.sqlite`)**:
   - `$T3CODE_HOME/userdata/statev2.sqlite` stores the canonical local projection tables:
     - `orchestration_v2_projection_threads`: `thread_id` (e.g. `mcp:<uuid>`), `project_id`, `default_provider`, `archived_at`, `deleted_at`, and `payload_json` (containing `worktreePath`, `branch`, `settledAt`, `unsettledAt`, `lineage`).
     - `projection_projects`: `project_id`, `workspace_root` (used when `worktreePath` is `null` on root-checkout threads).
     - `orchestration_v2_projection_runs`: `thread_id`, `ordinal`, `status` (`running`, `completed`, `failed`, `cancelled`).
     - `orchestration_v2_projection_runtime_requests`: `runtime_request_id`, `thread_id`, `kind` (`approval`, `permission`, `tool_approval`, `user_input`), `status` (`pending`, `resolved`), `payload_json`.

## Decision

1. `maestro_hosts::t3::T3Registry` discovers the T3 transport (`T3McpBridge`, `T3SqliteReadonly`, `T3JsonSnapshot`), runtime version, environment ID, and connection state via `T3Registry::discover()`, and reads `statev2.sqlite` in read-only mode (`?mode=ro`) with per-entry fault isolation on `orchestration_v2_projection_runtime_requests`.
2. `SurfaceRegistry` verifies callers against the surface registry (`Paseo`, `T3`, `Direct`), enforces symlink-safe `within(cwd, registered_cwd)` containment (canonicalizing on-disk paths before any lexical fallback), refuses cross-surface spoofing, and binds short-lived tuple-bound `BridgeCredential` tokens (`(session, node, cwd, issued_at_ms, expires_at_ms, generation)`) so wrong-node, stale-credential, ID-reuse, peer-UID mismatch, and disconnected-registry states all fail closed.
3. `maestro-mcp` wires `SurfaceRegistry` into `agent_request` (`verified_identity`) and projects live pending permission prompts from `SurfaceRegistry::live_permission_needs` into `maestro_needs_list`, `maestro_snapshot`, and `maestro_context`.
