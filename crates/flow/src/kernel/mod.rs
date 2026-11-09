//! The flow kernel over the storage layer (BRO-3633): definitions, their compiler, the
//! read-only scheduler, and the writers that claim and complete nodes.
//!
//! - **Definitions** are YAML ([`compile`]). The phase 1 constructs: sequence (`needs`), `fork:
//!   all` with a join (needing the fork's id is needing every branch), `switch` on a choice
//!   node's edge or an observe node's `exit_map` value, `repeat` with `max`, `until` and
//!   `on_exhausted`, declared waits (`wait` on work and effect nodes, the `human` class), and
//!   `retry` and `cancel` per node. Subflows, traits and templates are refused as unsupported.
//! - **Guards** are jq, checked at compile time; writers evaluate them (3b2) and record the
//!   verdicts in the log, and replay only reads them.
//! - **The IR** ([`Ir`]) is what a definition compiles to; it carries a content hash, and
//!   [`ir::validate`] holds its rules: no unknown reference, no loop without a bound, no switch
//!   case its subject does not declare, no effect without `reconcile`, and the class rules.
//! - **The scheduler** ([`start`], [`next`], [`status`]). `start` appends `Started`, pinning the
//!   IR in the instance's log, so a running instance keeps the definition it started with.
//!   `next` and `status` replay the log under the shared lock; nothing is stored but events,
//!   and replay runs no command and no guard (see `replay`'s module docs for the semantics).
//! - **The writers** ([`claim`], [`heartbeat`], [`complete`]). Every write verb, `start` included,
//!   takes the caller's verified session; only the instance's owner (its session and owner marker,
//!   recorded in `Started`) writes it. `complete` runs the node's own command ([`exec`], bounded
//!   in time, output and environment, with the flows lock released) and appends the receipt or a
//!   refusal. See `write`'s module docs for ownership, tokens and leases.
//! - **Events** ([`events`]) are typed bodies of the storage layer's generic event. `Chosen` and
//!   `Guarded` have no writer yet: `choose` and the `until` verdicts come next.

#[cfg(test)]
mod clocked;
mod compile;
mod definition;
mod error;
pub mod events;
pub mod exec;
pub mod ir;
mod jq;
mod replay;
mod write;

pub use compile::{CompiledFlow, MAX_SOURCE, compile};
pub use error::{CompileError, KernelError, Refusal};
pub use exec::Limits;
pub use ir::{Ir, Iteration};
pub use replay::{
    Begun, Enabled, Next, NodeStatus, Reason, Rejected, RepeatState, RepeatStatus, State, Status, next, start, status,
};
pub use write::{
    Beat, Claim, Completion, DEFAULT_LEASE_SECS, MAX_ARGV, MAX_LEASE_SECS, MAX_OUTPUTS, MAX_TOKEN, claim, complete,
    heartbeat,
};
