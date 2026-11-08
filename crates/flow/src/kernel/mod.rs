//! The flow kernel over the storage layer (BRO-3633): definitions, their compiler, and the
//! read-only scheduler.
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
//! - **Events** ([`events`]) are typed bodies of the storage layer's generic event. Only
//!   `Started` has a writer here; the claim, complete, choose, heartbeat and verdict writers come
//!   next.

mod compile;
mod definition;
mod error;
pub mod events;
pub mod ir;
mod jq;
mod replay;

pub use compile::{CompiledFlow, compile};
pub use error::{CompileError, KernelError};
pub use ir::{Ir, Iteration};
pub use replay::{
    Begun, Enabled, Next, NodeStatus, Reason, Rejected, RepeatState, RepeatStatus, State, Status, next, start, status,
};
