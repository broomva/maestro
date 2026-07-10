/// <reference types="bun" />
// The model proxy (HARNESS §3) — a supervisor-owned local HTTP listener the child's Agent SDK targets
// via `baseURL = BROOMVA_MODEL_PROXY` + its per-session bearer. Every model call flows through here:
// authenticate the bearer, RESERVE the budget BEFORE forwarding, forward with the RUNTIME's key
// (attached only here, never in the child env), then reconcile the reservation to the response. The
// guard is in the request path, not the agent's goodwill.
//
// The upstream forwarder is injected so the whole path is testable without a socket or a real key —
// and so the mock-model server (BRO-1806, the D8 canary layer 1) can sit behind the same seam.

import type { ErrorResponse } from "@maestro/protocol";
import { Hono } from "hono";
import type { ChildRole } from "../harness/spawn-contract";
import type { BudgetGuard } from "./budget";
import { estimateCallCeilingUsd, resolvePinnedModel } from "./models";
import type { SessionContext, SessionTokenRegistry } from "./tokens";

/** The result of a forwarded model call — the body passed back plus the usage the proxy reconciles. */
export interface UpstreamResult {
  status: number;
  body: unknown;
  /** Metered usage; absent → the reservation is RELEASED (e.g. an upstream error, nothing to bill). */
  usage?: { usd: number; tokens?: number };
}

/** What the proxy forwards to. The runtime KEY is passed per-call so it is never stored on a
 *  long-lived object graph — it attaches at the moment of forwarding and nowhere else.
 *  NOTE (BRO-1756 live loop): the real /v1/messages path streams SSE and meters on `message_stop`;
 *  this buffered result shape is the v1/mock contract and gains a streaming variant when start() lands. */
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

/** An over-budget run parks `blocked` (F3.1). The one place that mapping lives. */
export const PARK_STATE = "blocked" as const;

/** Extract the bearer from an `Authorization: Bearer <token>` header, or null. */
function bearerOf(header: string | undefined): string | null {
  if (header === undefined) return null;
  const m = /^Bearer[ ]+(.+)$/.exec(header.trim());
  return m ? (m[1] as string).trim() : null;
}

/**
 * Build the proxy Hono app. `POST /v1/messages` is the one scoped endpoint (HARNESS §3: the token is
 * scoped to messages/completions only). Refusals use the protocol `ErrorResponse` shape (API §4); an
 * upstream failure passes back an Anthropic-shaped, retryable 502 (the child's SDK speaks that).
 */
export function createModelProxy(deps: ModelProxyDeps): Hono {
  const app = new Hono();

  app.post("/v1/messages", async (c) => {
    // 1. Authenticate the per-session bearer → the run context.
    const token = bearerOf(c.req.header("authorization"));
    const ctx: SessionContext | null = token ? deps.tokens.resolve(token) : null;
    if (ctx === null) {
      const body: ErrorResponse = {
        error: {
          code: "unauthorized",
          message: "invalid or revoked session token",
          retryable: false,
        },
      };
      return c.json(body, 401);
    }

    // 2. Budget guard — RESERVE the per-call cost CEILING (model price × request max_tokens) before
    // forwarding. Because the reserve is >= the real cost, a call that would breach a cap is refused
    // here → 402, and the request never reaches Anthropic. The same `reserve` is threaded to
    // meter/release so the call reconciles against exactly what it reserved.
    const payload = await c.req.json().catch(() => ({}));
    const model = resolvePinnedModel(ctx.role, deps.env);
    const reserve = estimateCallCeilingUsd(model, payload, deps.env);

    const verdict = await deps.guard.preflight(ctx, reserve);
    if (!verdict.ok) {
      const body: ErrorResponse = {
        error: {
          code: "budget_exhausted",
          message: `budget exhausted (${verdict.reason}); run parks ${PARK_STATE}`,
          retryable: false,
        },
      };
      return c.json(body, 402);
    }

    // 2b. Revocation is checked AFTER the reserve's await too — a kill mid-preflight must not let one
    // more call land. Release the reservation we just took. (`token` is non-null here — a null token
    // already 401'd at step 1.)
    if (deps.tokens.resolve(token as string) === null) {
      await deps.guard.release(ctx, reserve);
      const body: ErrorResponse = {
        error: { code: "unauthorized", message: "session token revoked", retryable: false },
      };
      return c.json(body, 401);
    }

    // 3. Forward with the runtime key (attached HERE, never earlier). The child's role resolved the
    // pinned model — the child never names a model id. An upstream throw releases the reservation and
    // becomes a retryable 502, never an unhandled 500.
    let result: UpstreamResult;
    try {
      result = await deps.upstream.forward({
        model,
        role: ctx.role,
        payload,
        apiKey: deps.apiKey(),
      });
    } catch (err) {
      await deps.guard.release(ctx, reserve);
      return c.json(
        {
          error: { type: "upstream_unavailable", message: String((err as Error)?.message ?? err) },
        },
        502,
      );
    }

    // 4. Reconcile: meter actual usage, or release the reservation when nothing billed.
    if (result.usage) await deps.guard.meter(ctx, result.usage, reserve);
    else await deps.guard.release(ctx, reserve);

    return c.json(result.body as never, result.status as never);
  });

  return app;
}

/** A bound proxy listener + its client contract + a stop handle. */
export interface ProxyServer {
  /** The http base a child uses as BROOMVA_MODEL_PROXY (nominal `http://localhost` in unix mode). */
  url: string;
  /** Set in unix-socket mode — the socket path, carried out-of-band (a `unix://` URL is not
   *  fetch-dialable; the child dials the socket via this path). */
  socketPath?: string;
  stop(): void;
}

export interface ProxyListenOptions {
  /** Preferred: a unix socket path (HARNESS §3 "unix socket pinned", no TCP surface). */
  unixSocket?: string;
  /** Loopback fallback. Port 0 = OS-assigned. */
  port?: number;
  /** Loopback host only — 127.0.0.1 (default), ::1, or localhost. Anything else is rejected. */
  hostname?: string;
}

const LOOPBACK_HOSTS = new Set(["127.0.0.1", "::1", "localhost"]);

/**
 * Bind the proxy to a local listener — a unix socket when given (pinned), else loopback. NEVER binds a
 * non-loopback interface: the proxy holds the runtime key, so a non-loopback hostname is a hard error,
 * not a silent bind. This is the key-confinement invariant AS CODE (not a comment).
 */
export function serveProxy(app: Hono, opts: ProxyListenOptions = {}): ProxyServer {
  if (opts.unixSocket !== undefined) {
    const server = Bun.serve({ unix: opts.unixSocket, fetch: app.fetch });
    return {
      url: "http://localhost",
      socketPath: opts.unixSocket,
      stop: () => server.stop(true),
    };
  }
  const hostname = opts.hostname ?? "127.0.0.1";
  if (!LOOPBACK_HOSTS.has(hostname)) {
    throw new Error(
      `serveProxy refuses a non-loopback hostname (${hostname}) — the proxy holds the runtime key and must not be reachable off-host`,
    );
  }
  const server = Bun.serve({ port: opts.port ?? 0, hostname, fetch: app.fetch });
  return { url: `http://${server.hostname}:${server.port}`, stop: () => server.stop(true) };
}
