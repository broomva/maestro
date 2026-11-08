//! The kernel's event kinds, each a typed body stored in the storage layer's generic event
//! (`kind` plus a JSON `body`). Replay reads only these kinds; any other kind fails the replay.
//!
//! This PR writes only `Started` (through `start`). `Claimed`, `Completed`, `Refused`, `Chosen`
//! and `Heartbeat` are defined here so their writers can be added without changing what replay
//! reads.

use super::error::KernelError;
use super::ir::{Ir, Iteration};
use crate::log::NewEvent;
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
}

/// The receipt: a node accepted in an iteration, with the verify the kernel ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Completed {
    pub node: String,
    pub iteration: Iteration,
    pub attempt: u32,
    /// Input name (`params.<name>` or a node id) to the digest of the value it had.
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
    pub tool_use_id: Option<String>,
}

/// A lease extended to `lease_until`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub node: String,
    pub iteration: Iteration,
    pub attempt: u32,
    pub lease_until: String,
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

/// The `sha256:<hex>` digest of a JSON value, as a receipt records an input.
pub fn digest(value: &Value) -> String {
    // serde_json's map keeps keys sorted (no `preserve_order`), so equal values hash alike.
    crate::log::line_hash(value.to_string().as_bytes())
}
