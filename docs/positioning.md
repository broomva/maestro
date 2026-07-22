# Positioning — Maestro vs the field

> Status: living doc. One entry per adjacent tool, added when a checkit evaluation lands.
> Provenance for each entry lives in the workspace knowledge graph (`research/entities/tool/`).

## Plasma Fractal (plasma-ai/fractal, Jul 2026)

[Fractal](https://github.com/plasma-ai/fractal) is the closest shipped neighbor to Maestro's
problem — orchestrating loops of agentic work under budgets, with durable state and gates — and
the clearest evidence the problem is real: it independently converged on several of our core
calls (hard per-node budget caps, a reserve wind-down window, squash-merge-per-child worktree
isolation, and the distinction between a run that *met its goal* and a run that *signaled
completion* — their `exited` vs `completed` is our "gate completion on the durable landed-signal,
never the claim"). The planes differ, and that difference is the moat: Fractal is
**dev-tool-shaped** — a git+tmux+SQLite CLI whose operator reads worktrees, step files, and radio
mailboxes — while Maestro is **product-shaped**: work is the noun, sessions are the verb, chat is
a projection, and the human's one verb is the gate at "Needs you," with receipts instead of
engine-room internals (the disclosure ladder keeps worktrees and the index below the waterline).
Fractal optimizes for the operator *inside* the tree; Maestro optimizes for **unsupervised
hours** — how long the tree runs before a human must look. Where Fractal is simply ahead —
child-budget pricing arithmetic (a child's cap must cover solve + wind-down + reserve, and the
dispatcher must retain an integration iteration) — we borrow deliberately: BRO-1965 folds that
arithmetic into the decision policy rather than reinventing it.
