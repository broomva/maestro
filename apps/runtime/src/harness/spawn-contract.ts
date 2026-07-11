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
 * Construct the child's env from the host env + the contract spec. Allowlist-only: host secrets
 * (Anthropic key, runtime credential, relay key) never pass. The BROOMVA_* contract vars are set
 * explicitly AFTER the passthrough filter — including BROOMVA_MODEL_TOKEN, which the child legitimately
 * needs (its name matches the secret predicate, so it can only be set deliberately here, never leaked
 * from the host).
 */
export function buildChildEnv(
  hostEnv: Record<string, string | undefined>,
  spec: ChildEnvSpec,
): Record<string, string> {
  const env: Record<string, string> = {};
  for (const [name, value] of Object.entries(hostEnv)) {
    if (value === undefined) continue;
    if (isSecretEnvName(name)) continue; // defense-in-depth, before the allowlist
    if (PASSTHROUGH_ENV.has(name)) env[name] = value;
  }
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
