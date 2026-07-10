/// <reference types="bun" />
// The model proxy (HARNESS §3) — a supervisor-owned local HTTP listener the child's Agent SDK targets
// via `baseURL = BROOMVA_MODEL_PROXY` + its per-session bearer. Every model call flows through here:
// authenticate the bearer, guard the budget BEFORE forwarding, forward with the RUNTIME's key
// (attached only here, never in the child env), then meter the response. The guard is in the request
// path, not the agent's goodwill.
//
// The upstream forwarder is injected so the whole path is testable without a socket or a real key —
// and so the mock-model server (BRO-1806, the D8 canary layer 1) can sit behind the same seam.

import { Hono } from "hono";
import type { ChildRole } from "../harness/spawn-contract";
import type { BudgetGuard } from "./budget";
import { resolvePinnedModel } from "./models";
import type { SessionContext, SessionTokenRegistry } from "./tokens";

/** The result of a forwarded model call — the body passed back plus the usage the proxy meters. */
export interface UpstreamResult {
  status: number;
  body: unknown;
  /** Metered usage; absent → nothing metered (e.g. an upstream error the guard shouldn't bill). */
  usage?: { usd: number; tokens?: number };
}

/** What the proxy forwards to. The runtime KEY is passed per-call so it is never stored on a
 *  long-lived object graph — it attaches at the moment of forwarding and nowhere else. */
export interface ModelUpstream {
  forward(req: {
    model: string;
    role: ChildRole;
    payload: unknown;
    apiKey: string;
  }): Promise<UpstreamResult>;
}

export interface ModelProxyDeps {
  guard: BudgetGuard;
  tokens: SessionTokenRegistry;
  upstream: ModelUpstream;
  /** The runtime's Anthropic key, read lazily at forward time (a getter, not a stored string). */
  apiKey: () => string;
  /** Env for model-pin overrides (MAESTRO_MODEL_<ROLE>); defaults to process.env. */
  env?: Record<string, string | undefined>;
}

/** The refusal cap → child park state (F3.1: an over-budget run parks `blocked`). */
export function parkForRefusal(): "blocked" {
  return "blocked";
}

/** Extract the bearer from an `Authorization: Bearer <token>` header, or null. */
function bearerOf(header: string | undefined): string | null {
  if (header === undefined) return null;
  const m = /^Bearer[ ]+(.+)$/.exec(header.trim());
  return m ? (m[1] as string).trim() : null;
}

/**
 * Build the proxy Hono app. `POST /v1/messages` is the one scoped endpoint (HARNESS §3: the token is
 * scoped to messages/completions only). Errors use the protocol `ErrorResponse` shape (API §4).
 */
export function createModelProxy(deps: ModelProxyDeps): Hono {
  const app = new Hono();

  app.post("/v1/messages", async (c) => {
    // 1. Authenticate the per-session bearer → the run context.
    const token = bearerOf(c.req.header("authorization"));
    const ctx: SessionContext | null = token ? deps.tokens.resolve(token) : null;
    if (ctx === null) {
      return c.json(
        {
          error: {
            code: "unauthorized",
            message: "invalid or revoked session token",
            retryable: false,
          },
        },
        401,
      );
    }

    // 2. Budget guard — BEFORE forwarding. Over any cap → 402, request never reaches Anthropic.
    const verdict = await deps.guard.preflight(ctx);
    if (!verdict.ok) {
      return c.json(
        {
          error: {
            code: "budget_exhausted",
            message: `budget exhausted (${verdict.reason}); run parks ${parkForRefusal()}`,
            retryable: false,
          },
        },
        402,
      );
    }

    // 3. Forward with the runtime key (attached HERE, never earlier). The child's role resolves the
    // pinned model — the child never names a model id.
    const payload = await c.req.json().catch(() => ({}));
    const model = resolvePinnedModel(ctx.role, deps.env);
    const result = await deps.upstream.forward({
      model,
      role: ctx.role,
      payload,
      apiKey: deps.apiKey(),
    });

    // 4. Meter actual usage (HARNESS §3 step 3). No usage → nothing billed.
    if (result.usage) await deps.guard.meter(ctx, result.usage);

    return c.json(result.body as never, result.status as never);
  });

  return app;
}

/** A bound proxy listener + its client URL + a stop handle. */
export interface ProxyServer {
  /** The value handed to a child as BROOMVA_MODEL_PROXY. */
  url: string;
  stop(): void;
}

export interface ProxyListenOptions {
  /** Preferred: a unix socket path (HARNESS §3 "unix socket pinned"). */
  unixSocket?: string;
  /** Loopback fallback. Port 0 = OS-assigned. */
  port?: number;
  hostname?: string;
}

/**
 * Bind the proxy to a local listener — a unix socket when given (pinned, no TCP surface), else
 * loopback. Never binds a non-loopback interface: the proxy holds the key, so it must not be
 * reachable off-host.
 */
export function serveProxy(app: Hono, opts: ProxyListenOptions = {}): ProxyServer {
  if (opts.unixSocket !== undefined) {
    const server = Bun.serve({ unix: opts.unixSocket, fetch: app.fetch });
    return { url: `unix://${opts.unixSocket}`, stop: () => server.stop(true) };
  }
  const server = Bun.serve({
    port: opts.port ?? 0,
    hostname: opts.hostname ?? "127.0.0.1",
    fetch: app.fetch,
  });
  return { url: `http://${server.hostname}:${server.port}`, stop: () => server.stop(true) };
}
