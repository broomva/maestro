//! The flow ledger's storage layer (BRO-3633). `maestro-flow` is the only writer of the flow
//! ledger (`docs/adrs/2026-11-06-flow-ledger.md`); nothing else reads or writes these files.
//!
//! # Threat model
//!
//! - **In scope:** writers that use this API concurrently (threads, processes, sessions), and
//!   operator mistakes that can be detected: a lock file deleted, emptied or replaced, a root
//!   moved, archived or recreated, a crash at any point, a symbolic link placed under the root,
//!   a torn or corrupted log. Each is refused with a code, never written through.
//! - **Out of scope:** a hostile local process with write access to the tree. Such a process can
//!   edit any file directly; the checks here narrow the windows for mistakes, they do not defend
//!   against it.
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
//!   flows.archived             left by `operator::archive`: a ledger was here
//!   .flows.genesis-<epoch>/    a root being built (only during a genesis, or after a crash in one)
//!   .flows.lock.genesis-<epoch>  that genesis's own lock, held while it runs
//! ```
//!
//! # Rules
//!
//! - **One lock, one concurrency rule.** Writers take the lock exclusive ([`Ledger::lock`]),
//!   readers shared ([`Reader::lock`]). The lock is two OS file locks taken in order: on
//!   `flows.lock`, then on the root directory itself, so writers exclude each other even if the
//!   lock file is replaced under them. An append names the sequence number it expects the log
//!   to be at and is refused `stale_seq` otherwise; that check, under the lock, is the only
//!   concurrency rule. The guard releases the lock when dropped, so a caller drops it before
//!   slow external work (filing an ask can wait 30 s on the ask ledger's lock) and comes back
//!   with the sequence number it saw.
//! - **Checked at the write, not only at the lock.** Every write first re-checks that the lock
//!   path is still the file the guard locked (`lock_lost`), that the root is still the directory
//!   the writer opened, by device and inode (`root_moved`), and that its `EPOCH` is unchanged
//!   (`epoch_mismatch`). The write path never creates the root or `instances/`, only an
//!   instance's own directory, and follows no symbolic link under the root (`symlink`): it uses
//!   `lstat`, `create_dir` and `O_NOFOLLOW`.
//! - **Host binding and identity.** A genesis runs only on an empty tree: no lock, no root (so no
//!   `HOST` or `EPOCH`), no archive marker and no orphan stage. It holds its own lock stage, builds
//!   the root at a stage path, links the lock stage as `flows.lock` with a hard link (which fails
//!   if a lock exists, so one first writer is elected and the others wait on the lock, then see
//!   its `HOST` and refuse `foreign_host`), then renames the root into place. A writer remembers
//!   the epoch and root it opened (`epoch_mismatch`, `root_moved`). A missing or empty lock
//!   beside a live root is `lock_lost`, never a new epoch.
//! - **Archive and fresh ledgers are operator steps.** [`operator::archive`] moves the root and
//!   its lock away together, under the lock, and leaves `flows.archived`; every writer then
//!   refuses `root_moved` and creates nothing. A new ledger there is
//!   [`operator::create_fresh_ledger`], never a writer's job. A root moved by hand leaves its
//!   lock behind, which refuses `root_moved` too.
//! - **Crash recovery.** Every crash point of a genesis leaves a refusable state,
//!   `genesis_incomplete`: an orphan stage (no lock yet; a stage whose lock stage is unlocked is
//!   not in flight) or a lock naming an epoch whose stage was never moved into place.
//!   [`operator::recover`] clears the first and finishes the second. It also restores a lost lock
//!   from the root's own epoch, after taking the root directory's lock so no writer still holds a
//!   guard through the deleted lock.
//! - **Retire.** [`operator::retire`] moves `instances/<id>` to `retired/<id>` under the lock.
//!   Any later write or read naming that id is refused `retired`; the kernel only stats
//!   `retired/<id>` and never opens anything inside it.
//! - **Log and projection.** Each line of `events.jsonl` is one JSON event with `seq` and
//!   `prev_event`, the sha256 of the line before ([`log`]). A last line cut short or not parsing
//!   is a torn tail: it reads as unread and blocks appends (`torn_tail`) until
//!   [`operator::truncate_torn_tail`]. A whole line that does not chain is `chain_broken`, which
//!   truncation refuses. The projection has a row per live instance, whatever its log holds, and
//!   every row is a function of its log file alone, so the incremental cache and a rebuild agree.
//!   Reads take the shared lock and write nothing, the cache included: they open SQLite
//!   read-only and only compare. The log wins every disagreement.
//!
//! Every refusal carries a stable code, [`FlowError::code`].

mod error;
#[doc(hidden)]
pub mod hooks;
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
