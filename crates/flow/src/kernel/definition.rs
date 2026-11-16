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
    pub flow: String,
    pub version: u32,
    /// `name` is required, `name?` optional.
    #[serde(default)]
    pub params: Vec<String>,
    #[serde(deserialize_with = "entries")]
    pub nodes: Vec<(String, NodeDef)>,
    #[serde(default, deserialize_with = "entries")]
    pub forks: Vec<(String, ForkDef)>,
    #[serde(default, deserialize_with = "entries")]
    pub repeats: Vec<(String, RepeatDef)>,
    /// The directory every command runs in (`workdir: "{{params.repo}}"`).
    pub workdir: Option<String>,
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
    pub wait: Option<WaitDef>,
    /// A subflow (`uses: pr-head@1`): phase 2.
    pub uses: Option<serde_yaml::Value>,
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
            ("wait", self.wait.is_some()),
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
