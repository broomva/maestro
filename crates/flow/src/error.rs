//! Every refusal the storage layer gives, each with a stable machine code ([`FlowError::code`]).
//!
//! A refusal is a normal result: nothing was written, and the code says why. Callers match on
//! the code, never on the message, which is written for a person.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    /// The flows root is bound to another host. Phase 1 is single host.
    #[error("This flows root belongs to host {owner}, not {host}, so nothing was written.")]
    ForeignHost { host: String, owner: String },
    /// The lock names a ledger whose root is gone, for example archived (moved away).
    #[error("The flows root {root} has moved away from beside its lock, so nothing was read or written.")]
    RootMoved { root: PathBuf },
    /// The root's epoch is not the one this writer opened, or not the one its lock names.
    #[error("The flows root has epoch {found}, not {expected}, so it is not the ledger this writer opened.")]
    EpochMismatch { expected: String, found: String },
    /// The lock file beside a live root is missing, empty or was replaced. Never re-epochs the root.
    #[error(
        "The flows lock {lock} is missing or does not match its root ({detail}). An operator restores it with `operator::recover`."
    )]
    LockLost { lock: PathBuf, detail: String },
    /// A genesis started and did not finish. An operator finishes it with `operator::recover`.
    #[error(
        "A new flows ledger was started and not finished ({detail}). An operator finishes it with `operator::recover`."
    )]
    GenesisIncomplete { detail: String },
    /// The root's identity files are missing or unreadable.
    #[error("The flows root is damaged ({detail}), so nothing was read or written.")]
    LedgerDamaged { detail: String },
    /// No ledger exists here yet. A read never creates one.
    #[error("There is no flows ledger at {root} yet.")]
    NoLedger { root: PathBuf },
    /// Another writer, or a reader, held the lock for the whole wait.
    #[error(
        "Another process held the flows lock {lock} for the whole wait, so nothing was done. Try again in a moment."
    )]
    Busy { lock: PathBuf },
    /// The caller's expected sequence number is not the log's head.
    #[error("Instance {instance} is at event {current}, not {expected}, so the event was not appended.")]
    StaleSeq { instance: String, expected: u64, current: u64 },
    /// The log's last line is incomplete or does not verify. Reads report it unread; appends wait for an operator truncation.
    #[error(
        "Instance {instance} has a torn last line after event {verified_seq} ({detail}). An operator truncates it with `operator::truncate_torn_tail`."
    )]
    TornTail { instance: String, verified_seq: u64, detail: String },
    /// A line before the last does not verify. This is corruption, not a torn tail.
    #[error("Instance {instance}'s log does not verify at line {line} ({detail}).")]
    ChainBroken { instance: String, line: u64, detail: String },
    /// The instance was retired. Its log is kept and never reopened.
    #[error("Instance {instance} is retired, so it is never written again.")]
    Retired { instance: String },
    /// No such instance, and the caller did not ask to create one (expected sequence 0).
    #[error("There is no instance {instance}.")]
    UnknownInstance { instance: String },
    /// A path under the root is a symbolic link. Nothing under the root is followed out of it.
    #[error("{path} is a symbolic link, so nothing was read or written through it.")]
    Symlink { path: PathBuf },
    /// An id, host or event kind that is not allowed.
    #[error("{what} {value:?} is not allowed: {reason}.")]
    Invalid { what: &'static str, value: String, reason: &'static str },
    /// An event line or a log over its bound ([`crate::log::MAX_LINE`], [`crate::log::MAX_LOG`]):
    /// refused at append, and never read into memory.
    #[error("The {what} is {size} bytes, over its limit of {limit}, so it was not written or read.")]
    TooLarge { what: &'static str, size: u64, limit: u64 },
    /// An event body nested deeper than [`crate::log::MAX_NESTING`]: refused at append, since the
    /// reader could not read it back.
    #[error("The event body nests {depth} levels deep, more than {limit}, so it was not written.")]
    TooDeep { depth: usize, limit: usize },
    /// A log that holds [`crate::log::MAX_EVENTS`] events already, or more: refused at append,
    /// and never replayed.
    #[error("The log holds more than {limit} events, or would, so nothing was written or read.")]
    TooManyEvents { limit: u64 },
    /// An operator function was asked to act on a state it does not handle.
    #[error("{0}")]
    OperatorRefused(String),
    /// The SQLite projection could not be read or written.
    #[error("The flows projection could not be used: {0}.")]
    Projection(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl FlowError {
    /// The stable machine code of this refusal.
    pub fn code(&self) -> &'static str {
        match self {
            FlowError::ForeignHost { .. } => "foreign_host",
            FlowError::RootMoved { .. } => "root_moved",
            FlowError::EpochMismatch { .. } => "epoch_mismatch",
            FlowError::LockLost { .. } => "lock_lost",
            FlowError::GenesisIncomplete { .. } => "genesis_incomplete",
            FlowError::LedgerDamaged { .. } => "ledger_damaged",
            FlowError::NoLedger { .. } => "no_ledger",
            FlowError::Busy { .. } => "busy",
            FlowError::StaleSeq { .. } => "stale_seq",
            FlowError::TornTail { .. } => "torn_tail",
            FlowError::ChainBroken { .. } => "chain_broken",
            FlowError::Retired { .. } => "retired",
            FlowError::UnknownInstance { .. } => "unknown_instance",
            FlowError::Symlink { .. } => "symlink",
            FlowError::Invalid { .. } => "invalid",
            FlowError::TooLarge { .. } => "too_large",
            FlowError::TooManyEvents { .. } => "too_many_events",
            FlowError::TooDeep { .. } => "too_deep",
            FlowError::OperatorRefused(_) => "operator_refused",
            FlowError::Projection(_) => "projection",
            FlowError::Io { .. } => "io",
        }
    }

    pub(crate) fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> FlowError {
        let path = path.into();
        move |source| FlowError::Io { path, source }
    }
}

impl From<rusqlite::Error> for FlowError {
    fn from(e: rusqlite::Error) -> Self {
        FlowError::Projection(e.to_string())
    }
}

pub type Result<T, E = FlowError> = std::result::Result<T, E>;
