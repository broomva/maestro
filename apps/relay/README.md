# @maestro/relay — placeholder

The thin broker (small Hono service). Not built in the functional core — the dev
loop and 24/7 runtime use the direct/LAN path and bypass the relay (STACK.md
"The dev loop is the self-host tier"). Lands in **P6 (Distribution)**. Auth direction: **Better Auth** (the Broomva
convention — never NextAuth). The vendored handoff still specifies Clerk/WorkOS;
the canon-repairs ticket **BRO-1769** reconciles that into the handoff specs, so
treat the handoff Clerk/WorkOS references as superseded here. Do not build relay
auth until BRO-1769 records the reconciled decision.
