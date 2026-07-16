// useLedger (BRO-1818) — the ambient autonomy scoreboard's data hook. Fetches the DERIVED ledger from
// the runtime read endpoint (`GET /api/ledger`) on a calm poll. It degrades quietly — no index (404) or a
// transient network error keeps the last good/empty state rather than flashing an error into the chrome
// (rung-1 ambient is never alarming).
//
// Deliberately POLL-ONLY, not event-driven. The scoreboard is a rung-1 ambient surface (peripheral
// awareness, not the gate) — a look / a run start-stop shows up on the next tick, which is calm-correct.
// An earlier version refetched on the store's `server` slice changing, but that reference is minted anew
// on EVERY streamed event (reducer.ts returns a fresh object per applied event, including the
// high-frequency run.beat / tool.call / check.* log lines), so an active run fired a fetch per event — a
// storm against this endpoint's (currently unindexed) event scan. Time also accrues without events (a
// still-running run's unsupervised minutes grow), so a poll is the honest cadence regardless.
//
// The DERIVATION lives server-side (apps/runtime `deriveLedger`, over the event log); this hook only
// transports the already-derived aggregate. Effects are not unit-tested here (the app has no DOM harness,
// SSR skips effects) — the mapping is tested pure in autonomy-scoreboard.tsx, the live fetch is P11.
// Mirrors the store `connectStream` conventions: same-origin default baseUrl, injectable fetch.

import type { LedgerResponse } from "@maestro/protocol";
import { useCallback, useEffect, useRef, useState } from "react";

/** The ambient refresh cadence. The display is minute-granular for hours (`formatUnsupervised` floors to
 *  minutes); 30s keeps the discrete look/run counts reasonably fresh without hammering the scan. */
const LEDGER_REFRESH_MS = 30_000;

export interface UseLedgerOptions {
  /** Runtime origin; default `""` (same origin → the vite `/api` proxy forwards to the runtime). */
  baseUrl?: string;
  /** Injected `fetch` (default the global) — for tests / a non-default transport. */
  fetchImpl?: typeof fetch;
  /** Poll cadence in ms (default 30s). */
  intervalMs?: number;
}

/** Subscribe the caller to the live autonomy ledger. Returns the latest `LedgerResponse`, or `null` until
 *  the first successful load (the scoreboard renders its calm empty state while null). */
export function useLedger(opts: UseLedgerOptions = {}): LedgerResponse | null {
  const { baseUrl = "", fetchImpl = fetch, intervalMs = LEDGER_REFRESH_MS } = opts;
  const [ledger, setLedger] = useState<LedgerResponse | null>(null);

  // Guard setState against (a) a response that resolves after unmount, and (b) a slow response that
  // resolves AFTER a newer request already started — a monotonically increasing generation drops any
  // response that is no longer the latest, so state can never regress to an older snapshot.
  const alive = useRef(true);
  const generation = useRef(0);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    const mine = ++generation.current;
    try {
      const r = await fetchImpl(`${baseUrl}/api/ledger`);
      // Drop if superseded (a newer load started), unmounted, or non-2xx (no index → 404) — keep prior.
      if (!r.ok || !alive.current || mine !== generation.current) return;
      const body = (await r.json()) as LedgerResponse;
      if (alive.current && mine === generation.current) setLedger(body);
    } catch {
      // a transient network hiccup — stay calm, keep the last good/empty state
    }
  }, [baseUrl, fetchImpl]);

  // Mount load + the calm ambient poll (its sole trigger — see the file header on why it is not
  // event-driven). The generation guard above tolerates a tick that overlaps a slow prior fetch.
  useEffect(() => {
    load();
    const id = setInterval(load, intervalMs);
    return () => clearInterval(id);
  }, [load, intervalMs]);

  return ledger;
}
