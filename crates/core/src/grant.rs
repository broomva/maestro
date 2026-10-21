//! Standing grants: `.control/preauth.yaml`. A grant is written only from an
//! ask the owner actually answered; `granted: false` means always ask.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrantAct {
    OpenPr,
    Commit,
    PushBranch,
    FileTicket,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GrantMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act: Option<GrantAct>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    #[serde(rename = "match")]
    pub matcher: GrantMatch,
    pub granted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Preauth {
    #[serde(default)]
    pub grants: Vec<Grant>,
}
