// The Knowledge graph's SAMPLE data (BRO-1893 FID-6 slice 2). This is DEMO / fixture data — the client
// store has NO knowledge-graph read path yet (like History had no session read path: `/api/tree`
// hydrates work nodes only, no SSE event populates a KG). So the Knowledge page renders this sample
// scope graph, LABELLED as sample in the UI (never presented as live). When a real KG read path lands
// (walk `research/entities/` frontmatter → nodes + `related:` edges), this fixture is replaced by the
// projected data behind the same components. Verbatim port of the prototype's `KG_SCOPES` / `KG_TYPE` /
// `KG_FRESH` (ConceptKnowledge.jsx + KgGraph.jsx + KnowledgeApp.jsx).

/** A knowledge-entity kind — sets the node's colour + legend label (KgGraph.jsx `KG_TYPE`). */
export type KgNodeType =
  | "concept"
  | "pattern"
  | "primitive"
  | "tool"
  | "person"
  | "paper"
  | "decision"
  | "doc"
  | "session"
  | "vault"
  | "workspace"
  | "initiative"
  | "project"
  | "task"
  | "routine";

/** A Nous score triple: novelty · specificity · relevance, each 0–3 (P6 promotion gate). */
export type KgScore = readonly [number, number, number];

/** One knowledge entity — a file with frontmatter, in graph terms. */
export interface KgNode {
  id: string;
  label: string;
  type: KgNodeType;
  /** the entity's one-line core claim (frontmatter `core_claim`). */
  claim: string;
  /** ids of related entities — each draws a (bidirectional) edge. */
  related?: string[];
  /** Nous score (novelty, specificity, relevance); absent when unscored. */
  score?: KgScore;
  /** provenance receipts (frontmatter `sources`). */
  sources?: string[];
  /** when set, this node is a FOLDER (sub-scope) you can enter — its value is the child scope id. */
  scopeRef?: string;
  /** a live session node (renders the comet + pulse). */
  live?: boolean;
}

/** A scope — one folder's knowledge (a level of the graph you can descend into). */
export interface KgScope {
  id: string;
  /** the breadcrumb label. */
  crumb: string;
  /** the folder kind (vault / initiative / project / task / routine). */
  kind: string;
  /** one-line description of the scope. */
  desc: string;
  /** the parent scope id, or null at the root. */
  parent: string | null;
  nodes: KgNode[];
}

export type KgScopes = Record<string, KgScope>;

/** A freshly-bookkept entity (the "what's new" rail feed). */
export interface KgFresh {
  scopeId: string;
  nodeId: string;
  /** relative age, e.g. "12m", "1h", "1d". */
  when: string;
}

// The scope graph. Folder nodes carry `scopeRef` (the child scope id) so a parent always holds the node
// its child morphs out of. SAMPLE DATA — see the file header.
export const KG_SCOPES: KgScopes = {
  broomva: {
    id: "broomva",
    crumb: "Broomva",
    kind: "vault",
    desc: "governance · the bstack contract",
    parent: null,
    nodes: [
      {
        id: "hawthorne",
        label: "hawthorne",
        type: "initiative",
        scopeRef: "hawthorne",
        claim: "The agent platform · multi-turn work with a lifecycle.",
        related: ["p6", "genesis"],
      },
      {
        id: "genesis",
        label: "genesis",
        type: "project",
        scopeRef: "genesis",
        claim: "The walking-skeleton repo: observe, decide, act, judge, commit.",
        related: ["hawthorne", "p1"],
      },
      {
        id: "ops",
        label: "ops",
        type: "initiative",
        scopeRef: "ops",
        claim: "Recurring ops · bookkeeping, nightly digests.",
        related: ["p2"],
      },
      {
        id: "agents",
        label: "AGENTS.md",
        type: "doc",
        claim: "Reflexive trigger rules every agent loads at session start.",
        related: ["p6", "p2", "policy", "p1"],
      },
      {
        id: "policy",
        label: ".control/policy.yaml",
        type: "doc",
        claim: "Governance · gates, budgets, scopes. The controller of last resort.",
        related: ["p2", "rcs", "agents"],
      },
      {
        id: "p6",
        label: "Bookkeeping",
        type: "primitive",
        claim: "Knowledge graphs without quality control degrade into noise.",
        score: [3, 3, 3],
        sources: ["primitives.md", "bookkeeping.py"],
        related: ["agents", "nous", "engine"],
      },
      {
        id: "p2",
        label: "Control Gate",
        type: "primitive",
        claim: "Blocks destructive ops the model did not authorize.",
        score: [3, 2, 3],
        sources: ["primitives.md"],
        related: ["policy", "agents"],
      },
      {
        id: "p1",
        label: "Conversation Bridge",
        type: "primitive",
        claim: "Closes session amnesia · each session writes back a doc.",
        score: [3, 3, 2],
        sources: ["primitives.md"],
        related: ["agents"],
      },
      {
        id: "nous",
        label: "Nous gate",
        type: "concept",
        claim: "Novelty + specificity + relevance, each 0–3. Items under 2 of 9 are discarded.",
        score: [2, 3, 3],
        sources: ["bookkeeping.py"],
        related: ["p6"],
      },
      {
        id: "rcs",
        label: "governance stability",
        type: "concept",
        claim: "A narrow stability margin · the contract evolves slowly on purpose.",
        sources: ["recursive-controlled-systems"],
        related: ["policy"],
      },
      {
        id: "engine",
        label: "bstack-engine",
        type: "pattern",
        claim: "The candidate ledger · where primitives are born and gated.",
        sources: ["bstack-engine.md"],
        related: ["p6"],
      },
    ],
  },
  hawthorne: {
    id: "hawthorne",
    crumb: "hawthorne",
    kind: "initiative",
    desc: "the agent platform · multi-turn object model",
    parent: "broomva",
    nodes: [
      {
        id: "hawthorne-core",
        label: "hawthorne-core",
        type: "project",
        scopeRef: "hawthorne-core",
        claim: "The object model · persist run transcripts, at your gate.",
        related: ["worknoun", "hawthorne-db"],
      },
      {
        id: "hawthorne-db",
        label: "hawthorne-db",
        type: "project",
        scopeRef: "hawthorne-db",
        claim: "Imports + the store · Linear cycles land as work items.",
        related: ["hawthorne-core"],
      },
      {
        id: "worknoun",
        label: "work-as-noun",
        type: "concept",
        claim: "Folders are work at any scale; sessions are the verb acting on them.",
        score: [3, 3, 3],
        sources: ["work-model.md"],
        related: ["mc", "autonomy"],
      },
      {
        id: "mc",
        label: "Maestro",
        type: "concept",
        claim: "The plane that sorts your screen by the decisions only a human can make.",
        related: ["worknoun", "gate"],
      },
      {
        id: "autonomy",
        label: "unsupervised hours",
        type: "concept",
        claim: "The scarce resource: how long an agent runs before a human must look.",
        score: [3, 3, 3],
        sources: ["decision-log"],
        related: ["look", "worknoun"],
      },
      {
        id: "look",
        label: "the look",
        type: "concept",
        claim: "Hours compressed to what changed · decided · asks · a 90-second look.",
        related: ["autonomy", "gate"],
      },
      {
        id: "gate",
        label: "the gate",
        type: "concept",
        claim: "A clean run still lands at your gate · needing you is a gate, not a failure.",
        related: ["mc", "look"],
      },
      {
        id: "maestro",
        label: "maestro",
        type: "person",
        claim: "The orchestrator is just a session that schedules sessions.",
        sources: ["symphony skill"],
        related: ["hawthorne-core", "gate"],
      },
    ],
  },
  genesis: {
    id: "genesis",
    crumb: "genesis",
    kind: "project",
    desc: "one repo · its own .git + contract",
    parent: "broomva",
    nodes: [
      {
        id: "projection",
        label: "@genesis/projection",
        type: "session",
        live: true,
        claim: "Reduce the event stream to a phase machine · 1h 18m.",
        related: ["phase", "reducer"],
      },
      {
        id: "phase",
        label: "phase machine",
        type: "concept",
        claim: "Reduce the event stream to running · awaiting · blocked · done.",
        related: ["ndjson", "reducer", "uimsg"],
      },
      {
        id: "ndjson",
        label: "event stream",
        type: "concept",
        claim: "Append-only event timeline · the session's source of truth.",
        related: ["phase"],
      },
      {
        id: "reducer",
        label: "projection/reducer.ts",
        type: "tool",
        claim: "Folds tool events into the live phase the chat renders.",
        related: ["phase", "uimsg"],
      },
      {
        id: "uimsg",
        label: "message parts",
        type: "concept",
        claim: "text · reasoning · tool · data · the streaming contract.",
        score: [3, 2, 3],
        sources: ["ai-sdk docs"],
        related: ["aisdk", "reducer"],
      },
      {
        id: "aisdk",
        label: "UI Message Stream",
        type: "paper",
        claim: "Generated UI stops being bespoke when every part speaks the same chunks.",
        sources: ["sdk.vercel.ai"],
        related: ["uimsg"],
      },
      {
        id: "metr",
        label: "METR Time Horizon 1.1",
        type: "paper",
        claim: "The 80%-reliability deployable horizon is about an hour · above it, persist.",
        sources: ["metr.org · Jan 2026"],
        related: ["phase"],
      },
    ],
  },
  "hawthorne-core": {
    id: "hawthorne-core",
    crumb: "hawthorne-core",
    kind: "project",
    desc: "persist run transcripts · worktree-per-run",
    parent: "hawthorne",
    nodes: [
      {
        id: "objmodel",
        label: "object model",
        type: "concept",
        claim: "Work item to lifecycle: proposed, running, review, done.",
        score: [3, 2, 3],
        sources: ["hawthorne-core"],
        related: ["multiturn", "relay"],
      },
      {
        id: "multiturn",
        label: "multi-turn",
        type: "concept",
        claim: "A work item outlives any single session that touches it.",
        related: ["objmodel"],
      },
      {
        id: "relay",
        label: "Maestro relay",
        type: "concept",
        claim: "Handoff protocol · any session can pick up the work from here.",
        related: ["objmodel"],
      },
      {
        id: "spec3",
        label: "spec.md",
        type: "doc",
        claim: "kind: project · owner: maestro · budget: 8h · gate: human-approve.",
        related: ["drun", "notes", "run7c"],
      },
      {
        id: "drun",
        label: "persist transcript on Run",
        type: "decision",
        claim: "Not the session · survives restarts; 14 tests cover replay.",
        score: [3, 3, 3],
        sources: ["run/7c2f1a", "PR #214"],
        related: ["spec3", "run7c", "notes"],
      },
      {
        id: "ddefer",
        label: "defer compression",
        type: "decision",
        claim: "Transcripts stay small until multi-day runs land · revisit then.",
        score: [2, 3, 2],
        sources: ["decision-log 2026-06-06"],
        related: ["spec3"],
      },
      {
        id: "notes",
        label: "notes/prior-art.md",
        type: "doc",
        claim: "Survey of resumability approaches, written from scout's 47m run.",
        related: ["scout", "drun"],
      },
      {
        id: "run7c",
        label: "run/7c2f1a",
        type: "session",
        claim: "claude · 2h 14m unsupervised · 41 events · ran to the gate.",
        live: true,
        related: ["drun", "spec3", "judge"],
      },
      {
        id: "scout",
        label: "scout · survey",
        type: "session",
        claim: "claude to scout · 47m unsupervised · done.",
        related: ["notes"],
      },
      {
        id: "judge",
        label: "judge verdict",
        type: "concept",
        claim: "Checks passed · 14 tests added · a clean run, still your gate.",
        related: ["run7c", "drun"],
      },
      {
        id: "review",
        label: "you to review API",
        type: "session",
        claim: "Halted · needed you (2 looks).",
        related: ["spec3"],
      },
    ],
  },
  "hawthorne-db": {
    id: "hawthorne-db",
    crumb: "hawthorne-db",
    kind: "project",
    desc: "imports + the store",
    parent: "hawthorne",
    nodes: [
      {
        id: "linear",
        label: "Linear import",
        type: "decision",
        claim: "Blocked · needs a read scope granted before it can run.",
        sources: ["run/b91e44"],
        related: ["runb91", "cycles"],
      },
      {
        id: "runb91",
        label: "run/b91e44",
        type: "session",
        claim: "claude · 41m · paused · waiting on the credential grant.",
        related: ["linear"],
      },
      {
        id: "cycles",
        label: "linear-cycles.md",
        type: "doc",
        claim: "Map Linear cycles to work items · the import contract.",
        related: ["linear"],
      },
    ],
  },
  ops: {
    id: "ops",
    crumb: "ops",
    kind: "initiative",
    desc: "recurring ops",
    parent: "broomva",
    nodes: [
      {
        id: "nightly",
        label: "nightly-digest",
        type: "routine",
        scopeRef: "nightly",
        claim: "A standing loop: the routine is the deliverable, gate: none.",
        related: ["finance"],
      },
      {
        id: "bookkeeping",
        label: "bookkeeping",
        type: "task",
        scopeRef: "bookkeeping",
        claim: "Reconcile May invoices in a cloud sandbox.",
        related: ["finance", "reconcile"],
      },
      {
        id: "finance",
        label: "finance-substrate",
        type: "tool",
        claim: "Bookkeeper reconciles invoices and pushes digests each month.",
        sources: ["finance-substrate skill"],
        related: ["nightly", "bookkeeping"],
      },
      {
        id: "reconcile",
        label: "reconciliation",
        type: "concept",
        claim: "Match receipts to invoices; flag the gaps for a look.",
        related: ["bookkeeping"],
      },
    ],
  },
  nightly: {
    id: "nightly",
    crumb: "nightly-digest",
    kind: "routine",
    desc: "a loop that never closes",
    parent: "ops",
    nodes: [
      {
        id: "cadence",
        label: "cadence: 02:00",
        type: "doc",
        claim: "kind: routine · cadence: nightly 02:00 · gate: none.",
        related: ["digest", "run0610"],
      },
      {
        id: "digest",
        label: "digest template",
        type: "doc",
        claim: "What landed, what's stuck, what needs a look by morning.",
        related: ["cadence"],
      },
      {
        id: "run0610",
        label: "Thu 02:00 run",
        type: "session",
        claim: "31m · digest pushed + flagged a stuck import.",
        related: ["cadence", "digest"],
      },
      {
        id: "run0609",
        label: "Wed 02:00 run",
        type: "session",
        claim: "19m · digest pushed · /h/digest-0610.",
        related: ["cadence"],
      },
    ],
  },
  bookkeeping: {
    id: "bookkeeping",
    crumb: "bookkeeping",
    kind: "task",
    desc: "May reconciliation · cloud sandbox",
    parent: "ops",
    nodes: [
      {
        id: "may",
        label: "may-invoices.md",
        type: "doc",
        claim: "36 invoices, 38 receipts · the month's ledger.",
        related: ["bk", "reconciled"],
      },
      {
        id: "bk",
        label: "bookkeeper run",
        type: "session",
        claim: "Bookkeeper · cloud sandbox · 31 of 36 reconciled.",
        live: true,
        related: ["may", "reconciled"],
      },
      {
        id: "receipts",
        label: "drive: /receipts/2026-05",
        type: "doc",
        claim: "38 source documents pulled from the drive.",
        related: ["bk"],
      },
      {
        id: "reconciled",
        label: "5 unmatched",
        type: "decision",
        claim: "Five invoices have no receipt · flagged for your look.",
        score: [2, 3, 3],
        sources: ["run/c30a9d"],
        related: ["may", "bk"],
      },
    ],
  },
};

/** Freshly-bookkept entities — the "what's new" rail (sample). */
export const KG_FRESH: KgFresh[] = [
  { scopeId: "hawthorne-core", nodeId: "drun", when: "12m" },
  { scopeId: "broomva", nodeId: "nous", when: "1h" },
  { scopeId: "genesis", nodeId: "uimsg", when: "3h" },
  { scopeId: "bookkeeping", nodeId: "reconciled", when: "5h" },
  { scopeId: "broomva", nodeId: "rcs", when: "1d" },
];
