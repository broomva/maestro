// useLedger (BRO-1818) — the ambient autonomy scoreboard's data hook. Fetches the DERIVED ledger from
// the runtime read endpoint (`GET /api/ledger`) and keeps it calm-fresh: an immediate load on mount, a
// prompt refetch when server truth changes (a look / a run start-stop bumps the store's `server` slice),
// and a minute-granular poll (unsupervised time accrues even with no new events, so we can't lean on SSE
// alone). It degrades quietly — no index (404) or a transient network error keeps the last good/empty
// state rather than flashing an error into the chrome (rung-1 ambient is never alarming).
//
// The DERIVATION lives server-side (apps/runtime `deriveLedger`, over the event log); this hook only
// transports the already-derived aggregate. Effects are not unit-tested here (the app has no DOM harness,
// SSR skips effects) — the mapping is tested pure in autonomy-scoreboard.tsx, and the live fetch is a P11
// concern. Mirrors the store `connectStream` conventions: same-origin default baseUrl, injectable fetch.

import type { LedgerResponse } from "@maestro/protocol";
import { useCallback, useEffect, useRef, useState } from "react";
import { useStore } from "zustand";
import { maestroStore } from "@/store";

/** The ambient refresh cadence. The display is minute-granular (`formatUnsupervised` floors to minutes),
 *  so a 60s poll is as fresh as the surface can show — no sub-minute churn. */
const LEDGER_REFRESH_MS = 60_000;

export interface UseLedgerOptions {
  /** Runtime origin; default `""` (same origin → the vite `/api` proxy forwards to the runtime). */
  baseUrl?: string;
  /** Injected `fetch` (default the global) — for tests / a non-default transport. */
  fetchImpl?: typeof fetch;
  /** Poll cadence in ms (default 60s). */
  intervalMs?: number;
}

/** Subscribe the caller to the live autonomy ledger. Returns the latest `LedgerResponse`, or `null` until
 *  the first successful load (the scoreboard renders its calm empty state while null). */
export function useLedger(opts: UseLedgerOptions = {}): LedgerResponse | null {
  const { baseUrl = "", fetchImpl = fetch, intervalMs = LEDGER_REFRESH_MS } = opts;
  const [ledger, setLedger] = useState<LedgerResponse | null>(null);
  // The stable server-truth slice reference — changes only when the reducer applies an event, so a new
  // look / run bumps it and we refetch promptly rather than waiting for the next poll tick.
  const server = useStore(maestroStore, (s) => s.server);

  // Guard setState against a response that resolves after unmount (a rung-1 surface must never warn).
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    try {
      const r = await fetchImpl(`${baseUrl}/api/ledger`);
      if (!r.ok || !alive.current) return; // 404 (no index) / non-2xx → keep prior state, quietly
      const body = (await r.json()) as LedgerResponse;
      if (alive.current) setLedger(body);
    } catch {
      // a transient network hiccup — stay calm, keep the last good/empty state
    }
  }, [baseUrl, fetchImpl]);

  // The steady ambient poll.
  useEffect(() => {
    const id = setInterval(load, intervalMs);
    return () => clearInterval(id);
  }, [load, intervalMs]);

  // Mount + refetch on new server truth (a look / run landed). `server` is a deliberate trigger dep —
  // its reference changes when the reducer applies an event, and that is precisely when we want to
  // refetch; it is not "extra".
  // biome-ignore lint/correctness/useExhaustiveDependencies: `server` is an intentional refetch trigger
  useEffect(() => {
    load();
  }, [load, server]);

  return ledger;
}
