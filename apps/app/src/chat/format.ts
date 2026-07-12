// format (BRO-1826 M4, slice B) — pure text helpers for the feed. Kept out of the renderer so the
// tokenizing + copy rules are unit-testable without a DOM.

/** A run of assistant text: plain prose, or a `pill` (an inline link pill — 1 of the 5 sanctioned color
 *  uses, CLAUDE.md §Color). The pill's `value` is the noun without its delimiters. */
export interface TextToken {
  kind: "text" | "pill";
  value: string;
}

/**
 * Split assistant text into plain runs and inline link pills. Pills are the backtick-delimited spans an
 * agent already emits for real-world nouns — run refs (`run/7c2f1a`), branches, file paths, ids. This is
 * a deterministic trigger, NOT a natural-language "detect a proper noun" heuristic: an agent backticks
 * identifiers, never chrome words, so the canon rule ("bold real-world nouns, never UI chrome words")
 * falls out of the convention. An unpaired backtick is treated as literal text (no pill), so malformed
 * input never eats the rest of the message.
 */
export function tokenizeAssistantText(text: string): TextToken[] {
  const tokens: TextToken[] = [];
  let i = 0;
  while (i < text.length) {
    const open = text.indexOf("`", i);
    if (open < 0) {
      tokens.push({ kind: "text", value: text.slice(i) });
      break;
    }
    const close = text.indexOf("`", open + 1);
    if (close < 0) {
      // Unpaired backtick — the rest is literal (a half-typed span mid-stream stays prose until closed).
      tokens.push({ kind: "text", value: text.slice(i) });
      break;
    }
    if (open > i) tokens.push({ kind: "text", value: text.slice(i, open) });
    const inner = text.slice(open + 1, close);
    // An empty span (``) is not a pill — emit the literal backticks so nothing silently vanishes.
    if (inner.length > 0) tokens.push({ kind: "pill", value: inner });
    else tokens.push({ kind: "text", value: "``" });
    i = close + 1;
  }
  // Collapse to a single text token when there were no pills, so the common case stays a plain string.
  if (tokens.length === 0) return [{ kind: "text", value: "" }];
  return tokens;
}

/** The empty-session greeting — plain voice, sentence case, no emoji, no "Welcome!" (CLAUDE.md §Voice).
 *  `layer` is the work folder this fresh session runs in (e.g. "inbox", a project path); without one it
 *  degrades to a bare "A fresh session" rather than an awkward self-referential phrase. */
export function emptySessionGreeting(layer?: string): string {
  const where = layer?.trim();
  return where ? `A fresh session on ${where}` : "A fresh session";
}
