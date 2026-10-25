//! Plan graphs, `docs/plans/*.graph.yaml` in the workspace: one Linear ticket per node and
//! `depends_on` between nodes. The delegation script owns the format; Maestro reads the fields
//! it shows and ignores the rest.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanGraph {
    /// The plan's parent ticket, e.g. `BRO-3261`.
    #[serde(default)]
    pub ticket: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub nodes: Vec<PlanNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanNode {
    pub id: String,
    #[serde(default)]
    pub ticket: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}
