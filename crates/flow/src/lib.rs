//! The flow ledger's storage layer (BRO-3633). `maestro-flow` is the only writer of the flow
//! ledger (`docs/adrs/2026-11-06-flow-ledger.md`); nothing else reads or writes these files.
//!
//! # Layout
//!
//! The caller passes the flows root; in production it is `<workspace>/.broomva/flows`.
//!
//! ```text
//! .broomva/
//!   flows.lock                 the lock file, beside the root; holds the root's epoch
//!   flows/                     the root
//!     HOST                     the host this ledger is bound to
//!     EPOCH                    32 hex, made once at genesis
//!     instances/<id>/events.jsonl
//!     retired/<id>/events.jsonl
//!     projection.sqlite        a cache of the logs, rebuilt from them at will
//!   .flows.genesis-<epoch>/    a root being built (only during a genesis, or after a crash in one)
//! ```
//!
//! # Rules
//!
//! - **One lock, one concurrency rule.** Writers take `flows.lock` exclusive ([`Ledger::lock`]),
//!   readers shared ([`Reader::lock`]). An append names the sequence number it expects the log
//!   to be at and is refused `stale_seq` otherwise; that check, under the lock, is the only
//!   concurrency rule. The guard releases the lock when dropped, so a caller drops it before
//!   slow external work (filing an ask can wait 30 s on the ask ledger's lock) and comes back
//!   with the sequence number it saw.
//! - **Host binding and identity.** The lock file lives beside the root so archiving the root
//!   (moving it away) leaves the lock behind: every writer, waiting or new, then refuses
//!   `root_moved` and creates nothing. A genesis runs only on an empty tree (no lock, no root, so
//!   no `HOST` or `EPOCH`): it builds the root at a stage path, links a lock already holding the
//!   epoch with a hard link (which fails if a lock exists, so one first writer is elected and
//!   the others see its `HOST` and refuse `foreign_host`), then renames the root into place. A
//!   writer remembers the epoch it opened and refuses a root with another (`epoch_mismatch`). A
//!   missing or empty lock beside a live root is `lock_lost`, never a new epoch.
//! - **Crash recovery.** A crash mid-genesis leaves a lock naming an epoch whose stage was never
//!   moved into place, which every writer refuses `genesis_incomplete`. [`operator::recover`]
//!   finishes it, and also restores a lost lock from the root's own epoch. Making a new ledger
//!   for a new host after an archive is [`operator::create_fresh_ledger`], never a writer's job.
//! - **Retire.** [`operator::retire`] moves `instances/<id>` to `retired/<id>` under the lock.
//!   Any later write or read naming that id is refused `retired`; the kernel only stats
//!   `retired/<id>` and never opens anything inside it.
//! - **Log and projection.** Each line of `events.jsonl` is one JSON event with `seq` and
//!   `prev_event`, the sha256 of the line before ([`log`]). A torn last line reads as unread and
//!   blocks appends (`torn_tail`) until [`operator::truncate_torn_tail`]. Reads take the shared
//!   lock and write nothing, the cache included: they open SQLite read-only and only compare.
//!   The log wins every disagreement.
//!
//! Every refusal carries a stable code, [`FlowError::code`].

mod error;
mod ids;
mod layout;
mod ledger;
mod lock;
pub mod log;
pub mod operator;
mod projection;

pub use error::{FlowError, Result};
pub use ids::{HostId, InstanceId};
pub use ledger::{Appended, CacheState, CacheWrite, Ledger, Options, ReadGuard, Reader, Summary, WriteGuard};
pub use log::{Event, InstanceSummary, Log, NewEvent};
pub use projection::ProjectionDump;
