// Per-session bearer tokens (HARNESS §3). The child's Agent SDK is handed a bearer that authorizes
// the model proxy — NOT the Anthropic key. The registry mints one token per run at spawn and revokes
// it on kill, so a leaked token is exactly one revocable run's worth of blast radius. The token
// resolves to the run's full context (session, run dir, role, budget) — everything the proxy needs
// to meter and pin, without the child ever naming any of it.

import { randomBytes } from "node:crypto";
import type { Budget } from "@maestro/protocol";
import type { ChildRole } from "../harness/spawn-contract";

/** Everything the proxy needs about the run behind a bearer token. */
export interface SessionContext {
  /** The session (= run) this token authorizes. */
  session: string;
  /** Abs path to runs/run-<id>/ — where budget events journal (D-DURABILITY). */
  runDir: string;
  /** The child role — drives model pinning (HARNESS §3, §7). */
  role: ChildRole;
  /** The run's resolved budget contract (per_run_usd / per_day_usd / max_iterations). */
  budget: Budget;
}

/** A 256-bit URL-safe bearer — unguessable, and never derived from host state. */
function newToken(): string {
  return randomBytes(32).toString("base64url");
}

/**
 * The supervisor-owned registry of live bearer tokens. In-memory on purpose: a token is valid only
 * while its run's process is alive, so it must NOT survive a runtime restart (a restart respawns the
 * child with a fresh token). Single-writer, like the rest of the runtime.
 */
export class SessionTokenRegistry {
  #byToken = new Map<string, SessionContext>();
  #tokenBySession = new Map<string, string>();
  #mint: () => string;

  /** `mintToken` is injectable so tests can pin deterministic tokens. */
  constructor(mintToken: () => string = newToken) {
    this.#mint = mintToken;
  }

  /**
   * Mint a bearer for a run at spawn. Idempotent per session: re-minting (e.g. a fresh-context
   * restart, HARNESS §5, reuses the session id) revokes the prior token first, so an old token can
   * never outlive its process.
   */
  mint(ctx: SessionContext): string {
    const existing = this.#tokenBySession.get(ctx.session);
    if (existing !== undefined) this.#byToken.delete(existing);
    const token = this.#mint();
    this.#byToken.set(token, ctx);
    this.#tokenBySession.set(ctx.session, token);
    return token;
  }

  /** Resolve a bearer to its run context, or null if unknown/revoked (→ the proxy answers 401). */
  resolve(token: string): SessionContext | null {
    return this.#byToken.get(token) ?? null;
  }

  /** Revoke a run's token on kill (HARNESS §3): every in-flight and future call with it fails auth. */
  revoke(session: string): void {
    const token = this.#tokenBySession.get(session);
    if (token !== undefined) this.#byToken.delete(token);
    this.#tokenBySession.delete(session);
  }

  /** Live token count — for the observability surface (AUTONOMY §4). */
  get size(): number {
    return this.#byToken.size;
  }
}
