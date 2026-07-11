// NDJSON line splitter (HARNESS §2) — the shared byte-stream→lines primitive. A `ReadableStream<Uint8Array>`
// (a child's stdout, or the child reading its own control stdin) hands over arbitrary byte chunks: a single
// read may carry half a line, or several lines plus a partial. This buffers across chunks and yields only
// COMPLETE lines, retaining the trailing partial for the next push. A pathological stream that emits
// megabytes with no newline must not grow the buffer without bound — past `maxLineBytes` the buffer is
// dropped and counted (an `overflow`), so the reader can never OOM.
//
// Extracted from stdio.ts (BRO-1862) so BOTH the supervisor's stdout pump AND the child's stdin control
// reader share ONE splitter — the child is a compiled standalone binary and must not import the supervisor's
// stdio plumbing (SessionTee/ChildControl/LivenessMonitor) just to split lines. stdio.ts re-exports this.

/** Guard against an unbounded partial line (a stream emitting bytes with no `\n`). 16 MiB is far above any
 *  real event — a coalesced `agent.said` turn is kilobytes — so a trip means abuse/bug. */
export const DEFAULT_MAX_LINE_BYTES = 16 * 1024 * 1024;

export interface NdjsonSplitter {
  /** Feed a decoded chunk; return every complete line it completes (newline-delimited, `\n` stripped). */
  push(chunk: string): string[];
  /** Any trailing partial with no final newline (a stream that ends mid-line), or null. */
  flush(): string | null;
  /** How many times the buffer overflowed `maxLineBytes` and was dropped. */
  overflows(): number;
}

export function createNdjsonSplitter(maxLineBytes = DEFAULT_MAX_LINE_BYTES): NdjsonSplitter {
  let buf = "";
  let overflowCount = 0;
  return {
    push(chunk: string): string[] {
      buf += chunk;
      const lines: string[] = [];
      let idx = buf.indexOf("\n");
      while (idx !== -1) {
        // strip an optional trailing `\r` so a CRLF stream never yields a corrupt fragment
        const line = buf.slice(0, idx);
        lines.push(line.endsWith("\r") ? line.slice(0, -1) : line);
        buf = buf.slice(idx + 1);
        idx = buf.indexOf("\n");
      }
      // No newline in a buffer this large means an abusive/broken line — drop it, keep the reader alive.
      // Crucially this runs AFTER the complete lines above are extracted, so a valid line co-resident with
      // an over-cap partial in one append is NOT lost — only the remaining partial is dropped.
      if (buf.length > maxLineBytes) {
        buf = "";
        overflowCount++;
      }
      return lines;
    },
    flush(): string | null {
      const rest = buf;
      buf = "";
      return rest.length > 0 ? rest : null;
    },
    overflows: () => overflowCount,
  };
}
