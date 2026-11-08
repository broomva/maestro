//! The flow ledger's storage layer (BRO-3633). `maestro-flow` is the only writer of the flow
//! ledger (`docs/adrs/2026-11-06-flow-ledger.md`); nothing else reads or writes these files.
//!
//! # Threat model
//!
//! - **In scope:** writers that use this API concurrently (threads, processes, sessions), and
//!   operator mistakes that can be detected: a lock file deleted, emptied or replaced, a root
//!   moved, archived or recreated, a crash or power loss at any point, a symbolic link placed
//!   under the root, a torn or corrupted log. Each is refused with a code, never written through.
//! - **Out of scope:** a hostile local process with write access to the tree. Such a process can
//!   edit any file directly; the checks here narrow the windows for mistakes, they do not defend
//!   against it.
//! - **Accepted:** `operator::recover` restoring a deleted lock does not wait for a reader still
//!   holding the old one. Reads stay safe: they need only the log rules, and the worst a reader
//!   sees is a torn tail.
//!
//! # Layout
//!
//! The caller passes the flows root; in production it is `<workspace>/.broomva/flows`.
//!
//! ```text
//! .broomva/
//!   flows.lock                    the lock file, beside the root: the root's epoch
//!   flows/                        the root
//!     HOST                        the host this ledger is bound to
//!     EPOCH                       32 hex, made once at genesis
//!     instances/<id>/events.jsonl
//!     retired/<id>/events.jsonl
//!     projection.sqlite           a cache of the logs, rebuilt from them at will
//!   flows.archived                left by `operator::archive`: a ledger was here
//!   .flows.genesis-<epoch>/       a root being built
//!   .flows.lock.genesis-<epoch>   a genesis's lock, written whole before it is linked in
//!   .flows.lock.new-<epoch>       a new lock, written whole before it is renamed in
//! ```
//!
//! # Rules
//!
//! - **One lock, one concurrency rule.** Writers take two OS file locks in order, `flows.lock`
//!   then the root directory itself ([`Ledger::lock`]), so writers exclude each other even if
//!   the lock file is replaced under them. Readers take `flows.lock` shared ([`Reader::lock`]).
//!   An append names the sequence number it expects the log to be at and is refused `stale_seq`
//!   otherwise; that check, under the lock, is the only concurrency rule. The guard releases the
//!   lock when dropped, so a caller drops it before slow external work (filing an ask can wait
//!   30 s on the ask ledger's lock) and comes back with the sequence number it saw.
//! - **Writes go through the root's descriptor, never a path.** Taking the lock opens the root
//!   directory, waits for its lock, then checks the path still names that directory and that it
//!   is the very directory (device and inode) and epoch the writer opened, so a writer queued
//!   while the root moved or was archived refuses `root_moved`. From then on every open, create,
//!   rename and stat under the root is relative to that descriptor (`openat`, `mkdirat`,
//!   `renameat`, `statat`, in `at.rs`), with `O_NOFOLLOW` and `AT_SYMLINK_NOFOLLOW`. A root moved
//!   or replaced by path while a guard is held cannot redirect a write: it lands in the directory
//!   the guard verified, and the guard's next `lock()` refuses `root_moved`. No symbolic link under
//!   the root is followed (`symlink`). The projection is the one exception, since SQLite needs a
//!   path: it is written only while the root's path still names the held directory, and a write
//!   the root moved under fails (SQLite makes its journal by path) and is reported skipped. It is
//!   a cache; the log always wins.
//! - **Host binding and identity.** A genesis runs only on an empty tree: no lock, no root and no
//!   archive marker, checked again once its lock is linked (an archive needs that lock, so none
//!   can slip in after). It builds the whole root at a stage, then links a lock written whole and
//!   held under a private name as `flows.lock` (a hard link fails if a lock exists, so one first
//!   writer is elected and the others wait on the lock, then see its `HOST` and refuse
//!   `foreign_host`), then renames the stage into place. A missing, empty or partial lock beside a
//!   live root is `lock_lost`, never a new epoch.
//! - **A lock is never rewritten.** A new lock is written whole under a private name, synced, and
//!   renamed over the old one; a crash before the rename leaves the old lock as it was.
//! - **Archive and fresh ledgers are operator steps.** [`operator::archive`] takes the lock file
//!   exclusive (waiting for readers and writers) and the root directory's lock as a writer does,
//!   moves only a whole ledger, re-checks the root's path just before moving it, then moves the
//!   root and its lock away together and leaves `flows.archived`; every writer then refuses
//!   `root_moved` and creates nothing. A new ledger there is [`operator::create_fresh_ledger`],
//!   never a writer's job. A root moved by hand leaves its lock behind, which refuses `root_moved`
//!   too.
//! - **Crash recovery.** A crash after a genesis links its lock and before its stage moves in
//!   leaves a lock naming a stage, refused `genesis_incomplete`; [`operator::recover`] moves the
//!   stage in. A crash before the link leaves only a stage no lock names, which nothing reads.
//!   `recover` also restores the lock from the root's own epoch: a missing one after taking the
//!   root directory's lock (so no writer still holds a guard through the deleted one) and
//!   checking no lock appeared meanwhile, an empty or broken one under the lock file itself. It
//!   removes crash debris of the exact shapes above that no process holds.
//! - **Retire.** [`operator::retire`] moves `instances/<id>` to `retired/<id>` under the lock.
//!   Any later write or read naming that id is refused `retired`; the kernel only stats
//!   `retired/<id>` and never opens anything inside it.
//! - **Log and projection.** Each line of `events.jsonl` is one JSON event with `seq` and
//!   `prev_event`, the sha256 of the line before ([`log`]). A last line without its newline is a
//!   torn tail: it reads as unread and blocks appends (`torn_tail`) until
//!   [`operator::truncate_torn_tail`]. Any whole line that fails is `chain_broken`, which
//!   truncation refuses. The projection has a row per live instance, whatever its log holds, and
//!   every row is a function of its log file alone, so the incremental cache and a rebuild agree.
//!   Reads take the shared lock and write nothing, the cache included: they open SQLite
//!   read-only and only compare. The log wins every disagreement.
//!
//! Every refusal carries a stable code, [`FlowError::code`].
//!
//! The flow kernel built on this layer (definitions, the compiler, the scheduler and the claim
//! and complete writers) is [`kernel`]. The kernel is the only part of the crate that starts a
//! process: `kernel::complete` runs a node's command.

/// A pause point for the crate's own unit tests (`hooks`). Outside `cfg(test)` it expands to
/// nothing, so no other build, binary or dependent crate can reach one.
macro_rules! pause {
    ($point:literal) => {
        #[cfg(test)]
        crate::hooks::pause($point);
    };
}

mod at;
pub mod clock;
mod error;
#[cfg(test)]
extern crate self as maestro_flow;
#[cfg(test)]
mod hooked;
#[cfg(test)]
mod hooks;
mod ids;
pub mod kernel;
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
#[doc(hidden)]
pub use projection::ProjectionDump;
