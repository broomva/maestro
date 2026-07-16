// Spawn contract (HARNESS §1) — the supervisor→child seam. The child is untrusted-by-default: it
// gets an allowlisted env with NO host secrets (never the Anthropic key, runtime credential, or
// relay key), the run worktree as cwd, and a metered model proxy. This module is the supervisor SIDE
// of that seam: it parses/serializes the child argv contract and constructs the allowlisted env.
//
//   argv:  broomva-child --role agent|verifier|orchestrator --session <id>
//   env:   allowlist ONLY — PATH/HOME/LANG + toolchain caches + the BROOMVA_* contract vars

export type ChildRole = "agent" | "verifier" | "orchestrator";
export const CHILD_ROLES = [
  "agent",
  "verifier",
  "orchestrator",
] as const satisfies readonly ChildRole[];

export class SpawnContractError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SpawnContractError";
  }
}

export interface ChildInvocation {
  role: ChildRole;
  session: string;
}

/**
 * Parse the child argv: `--role agent|verifier|orchestrator --session <id>`. Throws on anything
 * malformed — the SUPERVISOR builds this argv, so a parse failure is a supervisor bug and must be
 * loud, not silently defaulted.
 */
export function parseChildArgv(argv: readonly string[]): ChildInvocation {
  let role: string | undefined;
  let session: string | undefined;
  // A flag's value must exist, must not be another flag (a `--`-prefixed token means the value was
  // omitted), and must not have been set already — all three are supervisor-argv bugs, so throw loudly.
  const take = (flag: string, prev: string | undefined, value: string | undefined): string => {
    if (prev !== undefined) throw new SpawnContractError(`${flag} given more than once`);
    if (value === undefined || value.startsWith("--")) {
      throw new SpawnContractError(`${flag} requires a value (got ${value ?? "nothing"})`);
    }
    return value;
  };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--role") role = take("--role", role, argv[++i]);
    else if (argv[i] === "--session") session = take("--session", session, argv[++i]);
  }
  if (role === undefined || !CHILD_ROLES.includes(role as ChildRole)) {
    throw new SpawnContractError(
      `--role must be one of ${CHILD_ROLES.join("|")} (got ${role ?? "nothing"})`,
    );
  }
  if (session === undefined || session.trim() === "") {
    throw new SpawnContractError("--session <id> is required and must be non-empty");
  }
  return { role: role as ChildRole, session };
}

/** The argv a supervisor passes when spawning the child — the inverse of parseChildArgv. */
export function serializeChildArgv(inv: ChildInvocation): string[] {
  return ["--role", inv.role, "--session", inv.session];
}

// The child env is DENY-BY-DEFAULT: only these exact host vars pass through. Everything else — every
// secret — is dropped. Deliberately NOT prefix-matched (`NODE_*` would let NODE_OPTIONS inject a
// `--require`, a host→child contamination vector) — an explicit name allowlist has no such surface.
const PASSTHROUGH_ENV = new Set([
  "PATH",
  "HOME",
  "LANG",
  "LC_ALL",
  "TZ",
  "TMPDIR",
  "SHELL", // the agent's sh tool needs a shell
  // toolchain caches/homes (HARNESS §1 "node/pnpm/bun caches") — paths, not option-injection vars
  "BUN_INSTALL",
  "PNPM_HOME",
  "npm_config_cache",
  "XDG_CACHE_HOME",
  "XDG_DATA_HOME",
  "XDG_CONFIG_HOME",
]);

// Defense-in-depth: even if the allowlist ever grew to admit one of these by mistake, a var whose
// NAME looks like a secret never reaches a child. The allowlist above already excludes them; this is
// the belt to that suspenders, and the predicate the test asserts against.
const SECRET_NAME =
  /(?:_KEY|SECRET|TOKEN|PASSWORD|PASSWD|CREDENTIAL|ANTHROPIC|OPENAI|_PAT|BEARER)/i;

/** True if an env var name looks like a secret and must never enter a child env. */
export function isSecretEnvName(name: string): boolean {
  return SECRET_NAME.test(name);
}

export interface ChildEnvSpec {
  session: string;
  /** BROOMVA_RUN_DIR — abs path to runs/run-<id>/. */
  runDir: string;
  /** BROOMVA_CONTRACT — abs path to the contract.json snapshot. */
  contractPath: string;
  /** BROOMVA_MODEL_PROXY — the supervisor-owned metered proxy (HARNESS §3). */
  modelProxyUrl: string;
  /** BROOMVA_MODEL_TOKEN — the per-session bearer, minted at spawn, revoked on kill. */
  modelToken: string;
  /** BROOMVA_CONTEXT_CEILING — token ceiling past which the child restarts fresh (HARNESS §5). Optional;
   *  omitted → the child falls back to its own DEFAULT_CONTEXT_CEILING_TOKENS. */
  contextCeilingTokens?: number;
}

/**
 * The DENY-BY-DEFAULT env floor shared by every process the runtime spawns that must not see host
 * secrets: keep only the {@link PASSTHROUGH_ENV} allowlist, and drop anything whose name looks like a
 * secret ({@link isSecretEnvName}) even if the allowlist ever admitted it. No BROOMVA_* contract vars —
 * callers that need those (the agent child) add them AFTER this floor.
 */
export function filterPassthroughEnv(
  hostEnv: Record<string, string | undefined>,
): Record<string, string> {
  const env: Record<string, string> = {};
  for (const [name, value] of Object.entries(hostEnv)) {
    if (value === undefined) continue;
    if (isSecretEnvName(name)) continue; // defense-in-depth, before the allowlist
    if (PASSTHROUGH_ENV.has(name)) env[name] = value;
  }
  return env;
}

/**
 * Construct the child's env from the host env + the contract spec. Allowlist-only ({@link
 * filterPassthroughEnv}): host secrets (Anthropic key, runtime credential, relay key) never pass. The
 * BROOMVA_* contract vars are set explicitly AFTER the passthrough filter — including BROOMVA_MODEL_TOKEN,
 * which the child legitimately needs (its name matches the secret predicate, so it can only be set
 * deliberately here, never leaked from the host).
 */
export function buildChildEnv(
  hostEnv: Record<string, string | undefined>,
  spec: ChildEnvSpec,
): Record<string, string> {
  const env = filterPassthroughEnv(hostEnv);
  env.BROOMVA_SESSION = spec.session;
  env.BROOMVA_RUN_DIR = spec.runDir;
  env.BROOMVA_CONTRACT = spec.contractPath;
  env.BROOMVA_MODEL_PROXY = spec.modelProxyUrl;
  env.BROOMVA_MODEL_TOKEN = spec.modelToken;
  if (spec.contextCeilingTokens !== undefined) {
    env.BROOMVA_CONTEXT_CEILING = String(spec.contextCeilingTokens);
  }
  return env;
}

/**
 * The env a verifier Stage-1 CHECK runs under (VERIFIER §2 Stage 1). A check command runs
 * agent-influenced code (the diff's test/build scripts), so it gets the allowlist floor and NOTHING
 * else — no BROOMVA_* contract vars, no model token, no host secret. Passed as the FULL child env to
 * the spawn (never merged onto `process.env`), so the check sees only PATH/HOME/toolchain.
 */
export function buildCheckEnv(hostEnv: Record<string, string | undefined>): Record<string, string> {
  return filterPassthroughEnv(hostEnv);
}

// The subscription CLI runner (BRO-1912) authenticates on the OPERATOR's own channel — NOT the metered
// proxy and NOT the Anthropic API key — so it needs a NARROW set of auth-carrying vars ON TOP of the
// deny-by-default floor. This is the ONE deliberate widening of the child env, and it stays an explicit
// NAME allowlist (never `...process.env`):
//   - USER / LOGNAME — the macOS login-Keychain owner. The subscription OAuth token lives in the
//     Keychain (never in an env var; verified: item "Claude Code-credentials"), so forwarding the
//     *username* is what lets the CLI read it. Not secrets.
//   - CLAUDE_CODE_OAUTH_TOKEN — the explicit OAuth-token channel (Linux / CI, where there is no
//     Keychain). The ONE credential the subscription runner is designed to spend. Its NAME matches the
//     secret predicate, so filterPassthroughEnv strips it — it can only be re-added HERE, by name.
export const CLAUDE_AUTH_PASSTHROUGH = ["USER", "LOGNAME", "CLAUDE_CODE_OAUTH_TOKEN"] as const;

// Non-secret operator steering for the runner (model / bin / permission / extra-passthrough). Dropped by
// the floor (not in the allowlist), re-added by explicit name so `MAESTRO_CLAUDE_MODEL=…` reaches the runner.
const CLAUDE_CONFIG_PASSTHROUGH = [
  "MAESTRO_CLAUDE_BIN",
  "MAESTRO_CLAUDE_MODEL",
  "MAESTRO_CLAUDE_PERMISSION",
  "MAESTRO_CLAUDE_ENV_PASSTHROUGH",
] as const;

/**
 * The env for the subscription `claude` CLI runner (BRO-1912) — the deny-by-default {@link
 * filterPassthroughEnv} floor PLUS the narrow, named {@link CLAUDE_AUTH_PASSTHROUGH} auth channel and
 * non-secret runner config. Deliberately NOT `{...process.env}`: the CLI runs under `bypassPermissions`,
 * so leaking every host secret (`ANTHROPIC_API_KEY`, `CLOUDFLARE_API_TOKEN`, `HOSTINGER_API_TOKEN`, …)
 * into it is the R5 KEY-EXFIL class the harness exists to prevent.
 *
 * `ANTHROPIC_API_KEY` is force-DELETED at the end: it must never reach the CLI, both to keep the key
 * away from an autonomous agent AND so the CLI bills the SUBSCRIPTION, not the API (the CLI prefers the
 * key when present). An operator may name host-specific auth vars via `MAESTRO_CLAUDE_ENV_PASSTHROUGH`
 * (comma-separated) — but NO secret-named var can be re-opened that way (the extras themselves pass
 * through {@link isSecretEnvName}), so neither `ANTHROPIC_API_KEY` nor an alternate billing / endpoint
 * channel like `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_BASE_URL` can be smuggled back in. A genuinely-needed
 * secret auth channel is a code change to {@link CLAUDE_AUTH_PASSTHROUGH}, never a runtime env toggle.
 *
 * Idempotent: re-applying it to its own output is a no-op except it drops any BROOMVA_* overlay (the CLI
 * must not hold the per-session bearer), which is why the runner passes NO `childEnv` when confining the
 * CLI's env while the spawner passes the supervisor's `args.env` so the RUNNER keeps its BROOMVA_RUN_DIR.
 */
export function buildClaudeProviderEnv(
  hostEnv: Record<string, string | undefined>,
  childEnv?: Record<string, string>,
): Record<string, string> {
  const env = filterPassthroughEnv(hostEnv); // deny-by-default floor — every host secret dropped
  const extras = (hostEnv.MAESTRO_CLAUDE_ENV_PASSTHROUGH ?? "")
    .split(",")
    .map((s) => s.trim())
    // No secret-named var is re-openable via the operator escape hatch — not ANTHROPIC_API_KEY, and not an
    // alternate billing/endpoint channel (ANTHROPIC_AUTH_TOKEN, ANTHROPIC_BASE_URL). The hatch is for
    // host-specific NON-secret channels (e.g. a socket path); a secret auth var is a code change instead.
    .filter((s) => s !== "" && !isSecretEnvName(s));
  for (const name of [...CLAUDE_AUTH_PASSTHROUGH, ...CLAUDE_CONFIG_PASSTHROUGH, ...extras]) {
    const v = hostEnv[name];
    if (v !== undefined) env[name] = v;
  }
  if (childEnv) Object.assign(env, childEnv); // BROOMVA_* contract (run dir) over the floor — RUNNER only
  delete env.ANTHROPIC_API_KEY; // belt: force subscription billing + keep the R5 key from the CLI
  return env;
}
