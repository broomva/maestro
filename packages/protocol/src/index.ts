// @maestro/protocol — the single language: events, intents, work items.
//
// Imported by BOTH the runtime and the client so the wire contract is the same
// code on both sides, not a codegen seam that drifts (PATTERNS §10: no type
// describing the wire is declared outside this package).
//
// Canon: DATA-MODEL §A.2/§B.2/§B.3, API.md §1/§4, specs/VERIFIER.md §1,
// PATTERNS §7/§10, docs/data-contract.md, FLOWS F1–F10, and the amendments in
// docs/canon-amendments.md (D-ENUM/GATE/AUTODONE/DURABILITY/ORDER/EVENTNAMES/NAME).

export * from "./chat";
export * from "./events";
export * from "./frontmatter";
export * from "./gate";
export * from "./index-schema";
export * from "./intents";
export * from "./plain-voice";
export * from "./state";
export * from "./version";
export * from "./work";
export * from "./work-item";
