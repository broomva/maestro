//! Initiatives: `.control/initiatives/<id>.yaml`, the state of started work.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InitiativeStatus {
    Active,
    BlockedOnOwner,
    Parked,
    Done,
    Killed,
}

impl InitiativeStatus {
    pub fn is_live(self) -> bool {
        matches!(self, Self::Active | Self::BlockedOnOwner)
    }
}

/// Who drives an initiative: `owner`, `fleet`, `none`, `coordinator:<8hex>` or `session:<id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum Driver {
    Owner,
    Fleet,
    None,
    Coordinator(String),
    Session(String),
}

impl From<Driver> for String {
    fn from(d: Driver) -> String {
        match d {
            Driver::Owner => "owner".into(),
            Driver::Fleet => "fleet".into(),
            Driver::None => "none".into(),
            Driver::Coordinator(h) => format!("coordinator:{h}"),
            Driver::Session(s) => format!("session:{s}"),
        }
    }
}

impl TryFrom<String> for Driver {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        match s.as_str() {
            "owner" => Ok(Self::Owner),
            "fleet" => Ok(Self::Fleet),
            "none" => Ok(Self::None),
            _ => {
                if let Some(h) = s.strip_prefix("coordinator:") {
                    if h.len() == 8 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Ok(Self::Coordinator(h.into()));
                    }
                } else if let Some(id) = s.strip_prefix("session:").filter(|id| !id.is_empty()) {
                    return Ok(Self::Session(id.into()));
                }
                Err(format!("driver `{s}` is not owner|fleet|none|coordinator:<8hex>|session:<id>"))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitingOn {
    pub kind: String,
    #[serde(rename = "ref")]
    pub reference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Initiative {
    pub id: String,
    pub title: String,
    pub intent: String,
    pub stated: serde_json::Value,
    pub status: InitiativeStatus,
    pub next_action: String,
    pub driver: Driver,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_on: Vec<WaitingOn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_by: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_grammar() {
        assert_eq!(Driver::try_from("coordinator:f5307ce7".to_string()), Ok(Driver::Coordinator("f5307ce7".into())));
        assert!(Driver::try_from("coordinator:short".to_string()).is_err());
        assert!(Driver::try_from("session:".to_string()).is_err());
        assert_eq!(String::from(Driver::Session("abc".into())), "session:abc");
    }
}
