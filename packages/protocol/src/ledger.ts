// Shared autonomy-ledger formatters (BRO-1818). Pure, presentation-only, and used on BOTH sides: the
// runtime builds `LedgerResponse.label` (the single-line form) with `formatLedgerLabel`, and the app
// chrome (AutonomyScoreboard) calls `formatUnsupervised` to render the hours portion of its two-tier
// hours/looks layout — so the plain-voice duration reads identically wherever it appears. The DERIVATION
// (`deriveLedger`, which reads the event log) lives server-side in apps/runtime; only these formatters,
// which act on the already-derived aggregate, are shared here.
//
// Voice canon (CLAUDE.md §Voice / §Work states): receipts, never percentages — whole hours + minutes,
// no `%`, no progress bar. "the scarce resource is unsupervised hours".

/**
 * Plain-voice rendering of a duration (data-contract §"2h 14m unsupervised"): whole hours + minutes, no
 * seconds, no percentage. `0` → "0m"; sub-minute → "0m" (a look, not a stopwatch); negatives clamp to 0.
 */
export function formatUnsupervised(ms: number): string {
  const totalMinutes = Math.floor(Math.max(0, ms) / 60_000);
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (hours === 0) return `${minutes}m`;
  return `${hours}h ${minutes}m`;
}

/**
 * The single plain-voice line the chrome can render verbatim — "2h 14m unsupervised · 3 looks" (lead with
 * the number, sentence case, no `%`). `1` look is singular ("· 1 look").
 */
export function formatLedgerLabel(l: { unsupervisedMs: number; humanLooks: number }): string {
  const looks = l.humanLooks === 1 ? "1 look" : `${l.humanLooks} looks`;
  return `${formatUnsupervised(l.unsupervisedMs)} unsupervised · ${looks}`;
}
