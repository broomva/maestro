// The "/file/$path" route contract (BRO-1890 FID-4) — the single source of truth for reading the
// active file's path out of the pathname. Both the shell layout (the file-tree active row) and the
// tab strip (the active tab) decode the route the same way here, so the two can never drift.
//
// The decode is GUARDED: `decodeURIComponent` throws a `URIError` on a malformed percent-escape
// (`/file/foo%`, `%zz`, a truncated `%E0`). Because the shell layout calls this UNCONDITIONALLY on
// every render, an unguarded throw would be caught by the router's shell-scoped error boundary and
// blank the entire chrome — a hand-typed bad URL should never take down the whole app. On a decode
// failure we fall back to the raw slice: the file-tree/tab simply won't match a real node (a calm
// "no such file"), never a crash. (P20 correctness + CodeRabbit both flagged the unguarded copies.)

const FILE_PREFIX = "/file/";

/** The open file's path when the route is "/file/$path", else null. Never throws. */
export function activeFilePath(pathname: string): string | null {
  if (!pathname.startsWith(FILE_PREFIX)) return null;
  const raw = pathname.slice(FILE_PREFIX.length);
  try {
    return decodeURIComponent(raw);
  } catch {
    return raw;
  }
}
