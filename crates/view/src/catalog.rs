//! The generative-UI catalog: the block types an agent may compose, and the validator
//! that turns an agent's spec into a tree Maestro will render.
//!
//! An agent sends block types and ledger handles only. No block carries text, a number
//! or markup for the screen: every value shown is resolved by Rust from a fresh Store.
//! Anything the validator cannot place is dropped, and every drop is recorded with the
//! block it came from and why, so the agent (and the owner) can see what was not shown.

use maestro_core::work::WorkState;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The catalog's version. Draft until node `catalog-freeze` publishes `catalog.v1.json`.
pub const VERSION: &str = "0-draft";
/// Most blocks one spec may place.
pub const MAX_BLOCKS: usize = 64;
/// Deepest nesting below the root (the root is depth 0).
pub const MAX_DEPTH: usize = 6;
/// Longest handle (ledger name, ask id, project, loop id), in characters.
pub const MAX_HANDLE: usize = 200;
/// Largest `limit` a list block takes.
pub const MAX_LIMIT: u8 = 50;
/// Most blocks a spec may send, placed or not; most children one block may list. Past these
/// the spec is refused whole, so the work and the drop report stay bounded.
pub const MAX_SPEC_BLOCKS: usize = 4 * MAX_BLOCKS;
/// Most drops reported one by one; the rest are counted in one closing drop.
pub const MAX_DROPS: usize = MAX_BLOCKS;

/// Text echoed back that is not a handle (a serde message, a non-string child): at most
/// [`MAX_HANDLE`] characters.
fn clip(text: &str) -> String {
    match text.char_indices().nth(MAX_HANDLE) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Gap {
    Tight,
    #[default]
    Normal,
    Loose,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StackProps {
    /// Space between children.
    #[serde(default)]
    pub gap: Gap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoProps {}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NeedsProps {
    /// Only holds for this project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 200))]
    pub project: Option<String>,
    /// Rows shown (1–50, default 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: Option<u8>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AskFilter {
    #[default]
    Open,
    Answered,
    Archived,
    All,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AsksProps {
    #[serde(default)]
    pub state: AskFilter,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 200))]
    pub project: Option<String>,
    /// Only asks that block a lane.
    #[serde(default)]
    pub blocking: bool,
    /// Rows shown (1–50, default 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AskProps {
    /// Ledger file name, e.g. `bro-2827-answers-landing.yaml`.
    #[schemars(length(min = 1, max = 200))]
    pub ledger: String,
    /// Ask `uid` or `id`.
    #[schemars(length(min = 1, max = 200))]
    pub id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkProps {
    /// Canonical state label: `Queued`, `Running`, `Stuck`, `Needs you`, `Done`, `Standing`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>", extend("enum" = ["Queued", "Running", "Stuck", "Needs you", "Done", "Standing", null]))]
    pub state: Option<WorkState>,
    /// Rows shown (1–50, default 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoopsProps {
    /// One loop id; omit for all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 200))]
    pub id: Option<String>,
    /// Only loops with drift or notices.
    #[serde(default)]
    pub drifting: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InitiativesProps {
    /// Include parked, done and killed initiatives.
    #[serde(default)]
    pub all: bool,
}

/// The inbox's tabs. Authority and Ideas are their classes; Decisions is every other open
/// ask (decision, credential, scope, external, artifact, or none), so the three tabs add up to All.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tab {
    All,
    #[default]
    Decisions,
    Authority,
    Ideas,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Self::All, Self::Decisions, Self::Authority, Self::Ideas];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Decisions => "decisions",
            Self::Authority => "authority",
            Self::Ideas => "ideas",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Decisions => "Decisions",
            Self::Authority => "Authority",
            Self::Ideas => "Ideas",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionsProps {
    /// The tab shown first; the others are one tap away on the screen.
    #[serde(default)]
    pub tab: Tab,
    /// Only asks of this project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 200))]
    pub project: Option<String>,
}

/// One validated block. The type names are the catalog's; props are typed, so a value the
/// catalog does not name cannot reach the renderer.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", content = "props", rename_all = "snake_case")]
pub enum Block {
    Stack(StackProps),
    Snapshot(NoProps),
    Needs(NeedsProps),
    Asks(AsksProps),
    Ask(AskProps),
    Work(WorkProps),
    Loops(LoopsProps),
    Initiatives(InitiativesProps),
    Fleet(NoProps),
    Decisions(DecisionsProps),
    Decision(AskProps),
}

struct Kind {
    name: &'static str,
    description: &'static str,
    children: bool,
    schema: fn() -> Value,
    parse: fn(Value) -> Result<Block, String>,
}

fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| json!({"type": "object"}))
}

fn props<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}

const KINDS: &[Kind] = &[
    Kind {
        name: "stack",
        description: "A vertical group of child blocks. The only block that takes children.",
        children: true,
        schema: schema_of::<StackProps>,
        parse: |v| props(v).map(Block::Stack),
    },
    Kind {
        name: "snapshot",
        description: "Counts across the control plane: needs, open asks, work by state, loops with drift, live initiatives. An unreadable section shows as unavailable, never 0.",
        children: false,
        schema: schema_of::<NoProps>,
        parse: |v| props(v).map(Block::Snapshot),
    },
    Kind {
        name: "needs",
        description: "Holds on the owner (the Needs you list), optionally for one project.",
        children: false,
        schema: schema_of::<NeedsProps>,
        parse: |v| props(v).map(Block::Needs),
    },
    Kind {
        name: "asks",
        description: "Asks from the ask ledgers, filtered by state, project and blocking.",
        children: false,
        schema: schema_of::<AsksProps>,
        parse: |v| props(v).map(Block::Asks),
    },
    Kind {
        name: "ask",
        description: "One ask in full by its ledger and id: the question, its options with the recommended one, and its entry hash.",
        children: false,
        schema: schema_of::<AskProps>,
        parse: |v| props(v).map(Block::Ask),
    },
    Kind {
        name: "work",
        description: "Work items with their canonical state, optionally one state.",
        children: false,
        schema: schema_of::<WorkProps>,
        parse: |v| props(v).map(Block::Work),
    },
    Kind {
        name: "loops",
        description: "Standing loops with their last tick, drift and notices.",
        children: false,
        schema: schema_of::<LoopsProps>,
        parse: |v| props(v).map(Block::Loops),
    },
    Kind {
        name: "initiatives",
        description: "Initiatives with status, driver and next action. Live ones by default.",
        children: false,
        schema: schema_of::<InitiativesProps>,
        parse: |v| props(v).map(Block::Initiatives),
    },
    Kind {
        name: "fleet",
        description: "The latest fleet-reconcile report per scope.",
        children: false,
        schema: schema_of::<NoProps>,
        parse: |v| props(v).map(Block::Fleet),
    },
    Kind {
        name: "decisions",
        description: "The Decisions inbox: open asks in tabs (All, Decisions, Authority, Ideas) with counts, answers waiting to land, grouped by project with age; each row opens to its full detail. Read-only: answering stays in Paseo's Maestro until a later phase.",
        children: false,
        schema: schema_of::<DecisionsProps>,
        parse: |v| props(v).map(Block::Decisions),
    },
    Kind {
        name: "decision",
        description: "One ask as the decision screen: stakes, the default if unanswered, summary, options with Recommended / Reversible / Status quo and their cost and risk, sources, what the agent tried first, and where to answer it.",
        children: false,
        schema: schema_of::<AskProps>,
        parse: |v| props(v).map(Block::Decision),
    },
];

/// The catalog as an agent reads it from `maestro_ui_catalog`.
pub fn catalog() -> Value {
    let blocks: Vec<Value> = KINDS
        .iter()
        .map(|k| json!({ "type": k.name, "description": k.description, "children": k.children, "props": (k.schema)() }))
        .collect();
    json!({
        "version": VERSION,
        "spec": {
            "shape": "{ root: <block id>, blocks: { <block id>: { type, props?, children?: [<block id>] } } }",
            "rule": "Send block types and ledger handles only. Every value on screen is read by Maestro from the ledgers; a block that does not validate is dropped and the drop is reported.",
        },
        "limits": { "blocks": MAX_BLOCKS, "spec_blocks": MAX_SPEC_BLOCKS, "depth": MAX_DEPTH, "handle_chars": MAX_HANDLE, "limit": MAX_LIMIT },
        "blocks": blocks,
    })
}

/// One block or reference the validator did not place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Drop {
    /// The block id from the spec.
    pub block: String,
    /// The block's type, when it named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    pub reason: String,
}

/// A placed block and its placed children, in the spec's order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Node {
    pub id: String,
    #[serde(flatten)]
    pub block: Block,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

/// What validation produced: the tree to render, every drop, and the spec of what was kept
/// (which the app polls with, so a poll never re-sends what was dropped).
#[derive(Debug, Clone, PartialEq)]
pub struct Validated {
    pub root: Node,
    pub drops: Vec<Drop>,
    pub kept: Value,
}

/// Validate an agent's spec. An error means nothing can be rendered (no root to start from);
/// anything less is a drop.
pub fn validate(spec: &Value) -> Result<Validated, String> {
    let obj = spec.as_object().ok_or("the spec must be an object { root, blocks }")?;
    for key in obj.keys() {
        if key != "root" && key != "blocks" {
            return Err("the spec takes only `root` and `blocks`".into());
        }
    }
    let root_id = obj.get("root").and_then(Value::as_str).ok_or("`root` must be a block id")?;
    let blocks = obj.get("blocks").and_then(Value::as_object).ok_or("`blocks` must be an object of blocks by id")?;
    if blocks.len() > MAX_SPEC_BLOCKS {
        return Err(format!("the spec sends {} blocks; at most {MAX_SPEC_BLOCKS}", blocks.len()));
    }
    names(root_id, blocks)?;
    if let Some((id, n)) = blocks
        .iter()
        .filter_map(|(id, b)| Some((id, b.get("children")?.as_array()?.len())))
        .find(|(_, n)| *n > MAX_BLOCKS)
    {
        return Err(format!("block `{id}` lists {n} children; at most {MAX_BLOCKS}"));
    }
    if !blocks.contains_key(root_id) {
        return Err(format!("`root` names `{root_id}`, which is not in `blocks`"));
    }
    let mut walk = Walk {
        blocks,
        placed: BTreeSet::new(),
        dropped: BTreeSet::new(),
        drops: Vec::new(),
        unlisted: 0,
        kept: Map::new(),
    };
    let root = walk.place(root_id, 0).ok_or_else(|| {
        let why = walk.drops.first().map_or_else(String::new, |d| format!(": {}", d.reason));
        format!("the root block `{root_id}` does not validate{why}")
    })?;
    for (id, raw) in blocks {
        if !walk.placed.contains(id.as_str()) && !walk.dropped.contains(id.as_str()) {
            let kind = raw.get("type").and_then(Value::as_str).map(str::to_string);
            walk.record(Drop { block: id.clone(), r#type: kind, reason: "not reachable from the root".into() });
        }
    }
    let mut drops = walk.drops;
    if walk.unlisted > 0 {
        let more = walk.unlisted;
        drops.push(Drop { block: "…".into(), r#type: None, reason: format!("{more} more drops not listed") });
    }
    let kept = json!({ "root": root_id, "blocks": Value::Object(walk.kept) });
    Ok(Validated { root, drops, kept })
}

struct Walk<'a> {
    blocks: &'a Map<String, Value>,
    placed: BTreeSet<&'a str>,
    dropped: BTreeSet<&'a str>,
    drops: Vec<Drop>,
    /// Drops past [`MAX_DROPS`], counted and not built.
    unlisted: usize,
    kept: Map<String, Value>,
}

impl<'a> Walk<'a> {
    fn record(&mut self, drop: Drop) {
        if self.drops.len() < MAX_DROPS {
            self.drops.push(drop);
        } else {
            self.unlisted += 1;
        }
    }

    fn drop(&mut self, id: &'a str, kind: Option<&str>, reason: impl Into<String>) -> Option<Node> {
        self.dropped.insert(id);
        self.record(Drop { block: id.to_string(), r#type: kind.map(str::to_string), reason: reason.into() });
        None
    }

    fn place(&mut self, id: &'a str, depth: usize) -> Option<Node> {
        let Some((key, raw)) = self.blocks.get_key_value(id) else {
            // A child that names no block: recorded under the name it was given.
            self.record(Drop { block: id.to_string(), r#type: None, reason: "no block has this id".into() });
            return None;
        };
        let id = key.as_str();
        if self.placed.contains(id) {
            return self.drop(id, None, "already placed: a block appears once (a cycle or a second parent)");
        }
        let Some(raw) = raw.as_object() else { return self.drop(id, None, "a block must be an object") };
        let Some(kind_name) = raw.get("type").and_then(Value::as_str) else {
            return self.drop(id, None, "a block must name its `type`");
        };
        let Some(kind) = KINDS.iter().find(|k| k.name == kind_name) else {
            return self.drop(id, Some(kind_name), format!("unknown block type `{kind_name}`"));
        };
        if let Some(extra) = raw.keys().find(|k| !matches!(k.as_str(), "type" | "props" | "children")) {
            return self.drop(
                id,
                Some(kind_name),
                format!("a block takes only type, props and children, not `{extra}`"),
            );
        }
        if depth > MAX_DEPTH {
            return self.drop(id, Some(kind_name), format!("nested deeper than {MAX_DEPTH}"));
        }
        if self.placed.len() >= MAX_BLOCKS {
            return self.drop(id, Some(kind_name), format!("over the limit of {MAX_BLOCKS} blocks"));
        }
        let raw_props = raw.get("props").cloned().unwrap_or_else(|| json!({}));
        let block = match (kind.parse)(raw_props).and_then(|b| check(&b).map(|()| b)) {
            Ok(b) => b,
            // serde quotes the offending key or value, so its message is clipped.
            Err(e) => return self.drop(id, Some(kind_name), format!("invalid props: {}", clip(&e))),
        };
        let child_ids: Vec<&'a str> = match raw.get("children") {
            None => Vec::new(),
            Some(Value::Array(list)) => {
                let mut ids = Vec::new();
                for c in list {
                    match c.as_str() {
                        Some(s) => ids.push(s),
                        None => {
                            self.record(Drop {
                                block: clip(&c.to_string()),
                                r#type: None,
                                reason: format!("a child of `{id}` must be a block id string"),
                            });
                        }
                    }
                }
                ids
            }
            Some(_) => return self.drop(id, Some(kind_name), "`children` must be a list of block ids"),
        };
        self.placed.insert(id);
        let mut children = Vec::new();
        let mut kept_children = Vec::new();
        for child in child_ids {
            if !kind.children {
                // The block stays; what it cannot hold is dropped and named.
                if let Some((ckey, craw)) = self.blocks.get_key_value(child)
                    && !self.placed.contains(ckey.as_str())
                {
                    let ckind = craw.get("type").and_then(Value::as_str);
                    self.drop(ckey.as_str(), ckind, format!("`{id}` is a {kind_name}, which takes no children"));
                } else {
                    self.record(Drop {
                        block: child.to_string(),
                        r#type: None,
                        reason: format!("`{id}` is a {kind_name}, which takes no children"),
                    });
                }
                continue;
            }
            if let Some(node) = self.place(child, depth + 1) {
                kept_children.push(Value::String(node.id.clone()));
                children.push(node);
            }
        }
        let mut kept = Map::new();
        kept.insert("type".into(), Value::String(kind_name.into()));
        if let Ok(Value::Object(mut tagged)) = serde_json::to_value(&block)
            && let Some(p) = tagged.remove("props")
        {
            kept.insert("props".into(), p);
        }
        if !kept_children.is_empty() {
            kept.insert("children".into(), Value::Array(kept_children));
        }
        self.kept.insert(id.to_string(), Value::Object(kept));
        Some(Node { id: id.to_string(), block, children })
    }
}

/// Every name a spec sends that a drop or the page may echo back (the root, block ids, block
/// keys, type names, child references) is a handle, or the spec is refused whole: each id and
/// reason stays bounded and holds no control or invisible formatting character.
fn names(root: &str, blocks: &Map<String, Value>) -> Result<(), String> {
    handle("root", root)?;
    for (id, raw) in blocks {
        handle("block id", id)?;
        let Some(raw) = raw.as_object() else { continue };
        for key in raw.keys() {
            handle("block key", key)?;
        }
        if let Some(kind) = raw.get("type").and_then(Value::as_str) {
            handle("type", kind)?;
        }
        for child in raw.get("children").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            handle("child id", child)?;
        }
    }
    Ok(())
}

fn handle(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("`{field}` is empty"));
    }
    if value.chars().count() > MAX_HANDLE {
        return Err(format!("`{field}` is longer than {MAX_HANDLE} characters"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("`{field}` holds a control character"));
    }
    if value.chars().any(is_format) {
        return Err(format!("`{field}` holds an invisible formatting character"));
    }
    Ok(())
}

/// The Unicode format characters (category Cf, Unicode 16): bidi overrides and isolates,
/// zero-width marks, the soft hyphen, the BOM, invisible tags. A handle is shown back to the owner.
fn is_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{0600}'..='\u{0605}' | '\u{061C}' | '\u{06DD}' | '\u{070F}' | '\u{0890}'..='\u{0891}'
        | '\u{08E2}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}' | '\u{110BD}' | '\u{110CD}'
        | '\u{13430}'..='\u{1343F}' | '\u{1BCA0}'..='\u{1BCA3}' | '\u{1D173}'..='\u{1D17A}'
        | '\u{E0001}' | '\u{E0020}'..='\u{E007F}')
}

fn limit(value: Option<u8>) -> Result<(), String> {
    match value {
        Some(n) if n == 0 || n > MAX_LIMIT => Err(format!("`limit` must be 1–{MAX_LIMIT}")),
        _ => Ok(()),
    }
}

/// Checks serde cannot express: handle length and characters, list limits.
fn check(block: &Block) -> Result<(), String> {
    match block {
        Block::Stack(_) | Block::Snapshot(_) | Block::Initiatives(_) | Block::Fleet(_) => Ok(()),
        Block::Needs(p) => {
            p.project.as_deref().map_or(Ok(()), |v| handle("project", v))?;
            limit(p.limit)
        }
        Block::Asks(p) => {
            p.project.as_deref().map_or(Ok(()), |v| handle("project", v))?;
            limit(p.limit)
        }
        Block::Ask(p) | Block::Decision(p) => {
            handle("ledger", &p.ledger)?;
            handle("id", &p.id)
        }
        Block::Decisions(p) => p.project.as_deref().map_or(Ok(()), |v| handle("project", v)),
        Block::Work(p) => limit(p.limit),
        Block::Loops(p) => p.id.as_deref().map_or(Ok(()), |v| handle("id", v)),
    }
}

/// Every type the catalog names, in catalog order.
pub fn type_names() -> Vec<&'static str> {
    KINDS.iter().map(|k| k.name).collect()
}

/// The catalog's per-type props schemas, by type name.
pub fn schemas() -> BTreeMap<&'static str, Value> {
    KINDS.iter().map(|k| (k.name, (k.schema)())).collect()
}
