//! The YAML a flow author writes. Every struct refuses unknown fields, so a misspelt key is an
//! error, never ignored. `compile` maps this onto the IR; the rules live there and in
//! `ir::validate`.

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::{Deserialize, de::DeserializeOwned};
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Definition {
    pub agents: Option<String>,
    pub flow: String,
    pub version: u32,
    /// `name` is required, `name?` optional.
    #[serde(default)]
    pub params: Vec<String>,
    #[serde(default)]
    pub types: BTreeMap<String, super::ir::ParamType>,
    #[serde(deserialize_with = "entries")]
    pub nodes: Vec<(String, NodeDef)>,
    #[serde(default, deserialize_with = "entries")]
    pub forks: Vec<(String, ForkDef)>,
    #[serde(default, deserialize_with = "entries")]
    pub repeats: Vec<(String, RepeatDef)>,
    /// The directory every command runs in (`workdir: "{{params.repo}}"`).
    pub workdir: Option<String>,
    /// Opt in to the receipt-aware command stdin contract.
    #[serde(default)]
    pub receipt_stdin: bool,
    /// Optional inline pinned subflow definitions (`subflows: { "lane@1": "<yaml>" }`, IR v10).
    #[serde(default)]
    pub subflows: BTreeMap<String, String>,
    /// Traits (`uses: [verified-finish@1]`): phase 2.
    pub uses: Option<serde_yaml::Value>,
    /// Templates: phase 2.
    pub templates: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClassDef {
    Pure,
    Work,
    Observe,
    Choice,
    Effect,
    Human,
    Subflow,
    Map,
}

impl ClassDef {
    pub(crate) fn name(self) -> &'static str {
        match self {
            ClassDef::Pure => "pure",
            ClassDef::Work => "work",
            ClassDef::Observe => "observe",
            ClassDef::Choice => "choice",
            ClassDef::Effect => "effect",
            ClassDef::Human => "human",
            ClassDef::Subflow => "subflow",
            ClassDef::Map => "map",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NodeDef {
    pub class: ClassDef,
    #[serde(default)]
    pub needs: Vec<String>,
    pub switch: Option<SwitchDef>,
    pub retry: Option<RetryDef>,
    pub cancel: Option<CancelDef>,
    // Fields that belong to some classes only (`compile::CLASS_FIELDS`).
    pub run: Option<Vec<String>>,
    /// A pure node's: the commit its `run` runs in a snapshot of (`kernel::snapshot`).
    pub snapshot: Option<String>,
    pub verify: Option<Vec<String>>,
    pub performer: Option<String>,
    pub skill: Option<String>,
    pub routing: Option<BTreeMap<String, String>>,
    pub fresh_for: Option<String>,
    pub inputs: Option<Vec<String>>,
    #[serde(default, deserialize_with = "some_entries")]
    pub exit_map: Option<Vec<(i32, String)>>,
    #[serde(default, deserialize_with = "some_entries")]
    pub edges: Option<Vec<(String, EdgeDef)>>,
    pub preconditions: Option<Vec<String>>,
    pub reconcile: Option<Vec<String>>,
    pub ask: Option<String>,
    pub required_body_fields: Option<Vec<String>>,
    pub wait: Option<WaitDef>,
    /// A subflow (`uses: lane@1`): valid on `subflow` and `map` nodes (IR v10).
    pub uses: Option<serde_yaml::Value>,
    /// Input parameter bindings for `subflow` / `map` (`with: { repo: "{{params.repo}}" }`).
    pub with: Option<BTreeMap<String, String>>,
    /// `map` fields (IR v10).
    pub over: Option<String>,
    pub item_key: Option<String>,
    pub max_items: Option<u32>,
    pub max_concurrency_param: Option<String>,
    pub ready_when: Option<super::ir::ReadyWhen>,
    pub reserve_providers: Option<u32>,
    pub provider_pool: Option<Vec<String>>,
    pub on_item_failure: Option<super::ir::OnItemFailure>,
    pub item_retry: Option<ItemRetryDef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ItemRetryDef {
    pub max: u32,
}

impl NodeDef {
    /// The class-specific fields this node sets, by name.
    pub(crate) fn class_fields(&self) -> Vec<&'static str> {
        let set = [
            ("run", self.run.is_some()),
            ("snapshot", self.snapshot.is_some()),
            ("verify", self.verify.is_some()),
            ("performer", self.performer.is_some()),
            ("skill", self.skill.is_some()),
            ("routing", self.routing.is_some()),
            ("fresh_for", self.fresh_for.is_some()),
            ("inputs", self.inputs.is_some()),
            ("exit_map", self.exit_map.is_some()),
            ("edges", self.edges.is_some()),
            ("preconditions", self.preconditions.is_some()),
            ("reconcile", self.reconcile.is_some()),
            ("ask", self.ask.is_some()),
            ("required_body_fields", self.required_body_fields.is_some()),
            ("wait", self.wait.is_some()),
            ("uses", self.uses.is_some()),
            ("with", self.with.is_some()),
            ("over", self.over.is_some()),
            ("item_key", self.item_key.is_some()),
            ("max_items", self.max_items.is_some()),
            ("max_concurrency_param", self.max_concurrency_param.is_some()),
            ("ready_when", self.ready_when.is_some()),
            ("reserve_providers", self.reserve_providers.is_some()),
            ("provider_pool", self.provider_pool.is_some()),
            ("on_item_failure", self.on_item_failure.is_some()),
            ("item_retry", self.item_retry.is_some()),
        ];
        set.into_iter().filter(|(_, on)| *on).map(|(name, _)| name).collect()
    }
}

/// Follow one branch: this node applies only when `on` (a choice node's edge or an observe
/// node's `exit_map` value) is `case`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SwitchDef {
    pub on: String,
    pub case: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetryDef {
    pub attempts: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancelDef {
    pub after: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EdgeDef {
    pub guard: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WaitDef {
    pub external: String,
    pub heartbeat: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ForkDef {
    pub all: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepeatDef {
    pub body: Vec<String>,
    pub max: Option<u32>,
    pub until: Option<String>,
    pub on_exhausted: Option<String>,
}

/// A mapping read as its entries in order, duplicates kept, so `compile` can refuse a repeated
/// id instead of the parser silently keeping the last one.
fn entries<'de, D, K, V>(d: D) -> Result<Vec<(K, V)>, D::Error>
where
    D: Deserializer<'de>,
    K: DeserializeOwned,
    V: DeserializeOwned,
{
    struct Entries<K, V>(PhantomData<(K, V)>);
    impl<'de, K: DeserializeOwned, V: DeserializeOwned> Visitor<'de> for Entries<K, V> {
        type Value = Vec<(K, V)>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a mapping")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(entry) = map.next_entry::<K, V>()? {
                out.push(entry);
            }
            Ok(out)
        }
    }
    d.deserialize_map(Entries(PhantomData))
}

fn some_entries<'de, D, K, V>(d: D) -> Result<Option<Vec<(K, V)>>, D::Error>
where
    D: Deserializer<'de>,
    K: DeserializeOwned,
    V: DeserializeOwned,
{
    entries(d).map(Some)
}
