// Protocol identity + versioning.
//
// Every request and stream carries `x-maestro-protocol: 1` (API.md §Versioning,
// D-NAME). Bump MAESTRO_PROTOCOL_VERSION on a breaking change; the relay passes
// the header through untouched (API.md §3).

/** The workspace/npm name of this package. */
export const PROTOCOL_PACKAGE = "@maestro/protocol" as const;

/** The protocol version header name (D-NAME: renamed from `x-broomva-protocol`). */
export const X_MAESTRO_PROTOCOL = "x-maestro-protocol" as const;

/** The current wire protocol version — the value of the `x-maestro-protocol` header. */
export const MAESTRO_PROTOCOL_VERSION = 1 as const;

export type MaestroProtocolVersion = typeof MAESTRO_PROTOCOL_VERSION;
