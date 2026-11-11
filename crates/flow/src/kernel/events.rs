//! The kernel's event kinds, each a typed body stored in the storage layer's generic event
//! (`kind` plus a JSON `body`). Replay reads only these kinds; any other kind fails the replay.
//!
//! The kernel writes `Started` (`start`), and in `write`: `Claimed` (`claim`), `Heartbeat`
//! (`heartbeat`), `Completed`, `Refused` and `EffectStarted` (`complete`), `Chosen` (`choose`) and
//! `Guarded` (a repeat's `until` verdict, written when an iteration settles). A verdict is
//! recorded with the digest of the input its guard read, so replay never evaluates one. A human
//! node's ask (`asks`) writes `AskPinned`, `AskLinked`, `AskWithdrawn` and `Answered`.
//!
//! A receipt (`Completed`, `Chosen`, and `Answered` for a human node) records in `inputs` the digest of every receipt it consumed:
//! each node it needs or switches on, by node id ([`receipt_digest`]). `status` and `next` name
//! those digests for every node. A receipt whose recorded
//! inputs no longer match is not done.

use super::error::KernelError;
use super::ir::{Ir, Iteration};
use crate::log::NewEvent;
use maestro_core::session::SessionRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A kernel event: its kind and body.
pub trait KernelEvent: Serialize {
    const KIND: &'static str;

    fn to_event(&self) -> Result<NewEvent, KernelError> {
        let body = serde_json::to_value(self).map_err(|e| KernelError::Encode(e.to_string()))?;
        Ok(NewEvent::new(Self::KIND, body))
    }
}

/// The first event of every instance. It pins the whole IR, so the instance keeps the
/// definition it started with whatever happens to the definition later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Started {
    /// `<flow>@<version>`.
    pub flow: String,
    pub ir_hash: String,
    pub ir: Ir,
    pub params: Value,
    /// The session that started the instance, and so the only one that may write it. `None` in
    /// a log written before owners were recorded: such an instance takes no write.
    #[serde(default)]
    pub owner: Option<Owner>,
}

/// An instance's owner: a session and the digest of its owner marker, together. The marker alone
/// is not an identity (Direct sessions sharing a token share a marker), and a replacement session
/// keeps the `SessionRef` with a new marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub session: SessionRef,
    /// [`marker_digest`] of the owner marker; the marker itself is never written.
    pub marker_sha256: String,
}

/// The `sha256:<hex>` digest of an owner marker, as `Started` records it.
pub fn marker_digest(marker: &str) -> String {
    crate::log::line_hash(marker.as_bytes())
}

/// Who performed an attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Performer {
    pub harness: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
}

/// A verify or observe command the kernel ran, and its exit status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyRecord {
    pub argv: Vec<String>,
    pub exit: i32,
    pub stdout_sha256: String,
    pub ran_at: String,
}

/// An attempt taken: a lease on (node, iteration, attempt) until `lease_until` (RFC 3339).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claimed {
    pub node: String,
    pub iteration: Iteration,
    pub attempt: u32,
    pub lease_until: String,
    /// The sha256 of the single-use token handed to the performer; never the token itself.
    pub token_sha256: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub performer: Option<Performer>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// When the node's `cancel.after` ends this attempt, heartbeats or not (RFC 3339). The lease
    /// never runs past it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_at: Option<String>,
}

/// The receipt: a node accepted in an iteration, with the verify the kernel ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Completed {
    pub node: String,
    pub iteration: Iteration,
    pub attempt: u32,
    /// Node id to the digest its receipt had ([`receipt_digest`]).
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub outputs: Value,
    #[serde(default)]
    pub verify: Option<VerifyRecord>,
    #[serde(default)]
    pub performer: Option<Performer>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
}

/// A completion the kernel refused. The attempt stays open while its lease holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refused {
    pub node: String,
    pub iteration: Iteration,
    pub attempt: u32,
    pub code: String,
    pub reason: String,
}

/// A choice node's edge, accepted because it was declared and its guard held.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chosen {
    pub node: String,
    pub iteration: Iteration,
    pub attempt: u32,
    pub edge: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// The digest of the input document the edge's guard held over (see `guard`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_digest: Option<String>,
}

/// An effect's command about to run for an attempt, written before it runs. An effect started over
/// some inputs and not completed is never run again over them: the next completion runs its
/// `reconcile`. A `Refused` with code `spawn_failed` for the attempt withdraws it, since the command
/// never started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectStarted {
    pub node: String,
    pub iteration: Iteration,
    pub attempt: u32,
    /// The claim's input digests: an effect is one run over one set of inputs.
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    /// The digest of the input document its preconditions held over; none without preconditions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_digest: Option<String>,
}

/// A lease extended to `lease_until`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub node: String,
    pub iteration: Iteration,
    pub attempt: u32,
    pub lease_until: String,
}

/// The recorded verdict of a repeat's `until` for one iteration, evaluated once when the
/// iteration settled. Replay reads verdicts; it never evaluates a guard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guarded {
    pub repeat: String,
    pub iteration: u32,
    pub holds: bool,
    /// The digest of the input document the `until` was evaluated over (see `guard`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

/// A human node's ask, pinned in the log before it is filed: the id the kernel minted, the ledger
/// it goes to, and the exact body handed to the writer of record (`asked_at` included), with the
/// hash of its comparable part (`asks::comparable_hash`). A filing that crashed is filed again with
/// this same body, so it is linked, never filed twice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskPinned {
    pub node: String,
    pub iteration: Iteration,
    pub ask_id: String,
    pub ledger: String,
    pub body: Value,
    pub body_hash: String,
}

/// The pinned ask, confirmed in the asks ledger's working tree: a row with its id and its hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskLinked {
    pub node: String,
    pub iteration: Iteration,
    pub ask_id: String,
    pub ledger: String,
    pub body_hash: String,
}

/// A pinned ask the writer of record refused, with no row of its id in the working tree: it was
/// not filed, so the pin no longer stands and the next `ask` mints another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskWithdrawn {
    pub node: String,
    pub iteration: Iteration,
    pub ask_id: String,
    pub code: String,
    pub reason: String,
}

/// The owner's answer to a linked ask, read from the answers ledger, never from an agent: a human
/// node's receipt. `outputs` holds the answer's fields (see `asks`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answered {
    pub node: String,
    pub iteration: Iteration,
    pub ask_id: String,
    pub ledger: String,
    /// Node id to the digest its receipt had, as every receipt records them.
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub outputs: Value,
    /// The revision the answer was read at.
    pub source: String,
    /// The answered row's entry hash (the plugin's `entryHash`), as read.
    pub row_hash: String,
}

impl KernelEvent for AskPinned {
    const KIND: &'static str = "AskPinned";
}
impl KernelEvent for AskLinked {
    const KIND: &'static str = "AskLinked";
}
impl KernelEvent for AskWithdrawn {
    const KIND: &'static str = "AskWithdrawn";
}
impl KernelEvent for Answered {
    const KIND: &'static str = "Answered";
}

impl KernelEvent for Started {
    const KIND: &'static str = "Started";
}
impl KernelEvent for Claimed {
    const KIND: &'static str = "Claimed";
}
impl KernelEvent for Completed {
    const KIND: &'static str = "Completed";
}
impl KernelEvent for Refused {
    const KIND: &'static str = "Refused";
}
impl KernelEvent for Chosen {
    const KIND: &'static str = "Chosen";
}
impl KernelEvent for Heartbeat {
    const KIND: &'static str = "Heartbeat";
}
impl KernelEvent for Guarded {
    const KIND: &'static str = "Guarded";
}
impl KernelEvent for EffectStarted {
    const KIND: &'static str = "EffectStarted";
}

/// The `sha256:<hex>` digest of a JSON value.
pub fn digest(value: &Value) -> String {
    // serde_json's map keeps keys sorted (no `preserve_order`), so equal values hash alike.
    crate::log::line_hash(value.to_string().as_bytes())
}

/// A receipt's digest, as a downstream receipt records it: its outputs and its value (the
/// observed value or the chosen edge). A re-run with the same result keeps downstream receipts.
pub fn receipt_digest(outputs: &Value, value: Option<&str>) -> String {
    digest(&serde_json::json!({ "outputs": outputs, "value": value }))
}
