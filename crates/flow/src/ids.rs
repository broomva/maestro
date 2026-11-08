//! Names the ledger puts on disk: instance ids, host ids and event kinds. Each is checked before it
//! touches a path, so no name can climb out of the flows root.

use crate::error::{FlowError, Result};
use std::fmt;

const MAX_ID: usize = 128;
const MAX_KIND: usize = 64;

/// `[A-Za-z0-9_-]`, not starting with `-`, at most `max` bytes.
fn check_name(what: &'static str, value: &str, max: usize) -> Result<()> {
    let reason = if value.is_empty() {
        Some("it is empty")
    } else if value.len() > max {
        Some("it is too long")
    } else if value.starts_with('-') {
        Some("it starts with a dash")
    } else if !value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        Some("only letters, digits, underscores and dashes are allowed")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(FlowError::Invalid { what, value: value.to_string(), reason }),
        None => Ok(()),
    }
}

/// A flow instance's id: its directory name under `instances/` and `retired/`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstanceId(String);

impl InstanceId {
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        check_name("Instance id", &id, MAX_ID)?;
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The host a flows root is bound to. The caller supplies a stable id for its machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostId(String);

impl HostId {
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        check_name("Host id", &id, MAX_ID)?;
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An event kind, such as `Started`. Kept generic here; the kernel defines its kinds.
pub(crate) fn check_kind(kind: &str) -> Result<()> {
    check_name("Event kind", kind, MAX_KIND)
}
