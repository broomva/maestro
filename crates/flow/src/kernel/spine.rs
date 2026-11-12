//! The accepting spine of an instance (BRO-3636; flow kernel spec, "Phases and acceptance" item 2):
//! what two runs of one flow are compared on, since their raw logs legitimately differ.
//!
//! The spine is one record per (node, iteration) that has an accepted receipt, in log order: the
//! node, its iteration, the chosen edge (a choice node's `Chosen`, none for any other) and the
//! exit status of the verify the kernel ran (a `Completed`'s `verify.exit`, none for a choice or a
//! human node's `Answered`). A node recorded again in its iteration (an observation re-run once
//! stale, a receipt redone over new inputs) keeps one record, at the place of its first receipt,
//! holding what its latest receipt says. Events replay set aside are not receipts and are left out.
//!
//! Everything else in the log is left out: `Claimed`, `Heartbeat` and `Refused` events (leases,
//! lost leases and re-claims, refusals and the repairs after them), attempt numbers, refusal
//! codes, performers, `tool_use_id`s, timestamps and `prev_event`. So two runs whose performers
//! were refused and repaired, or lost a lease and claimed again, a different number of times, or
//! that ran under different sessions, have equal spines.
//!
//! [`compare`] finds the first difference between two spines: another flow (by name and IR hash),
//! a record one has and the other lacks (a node skipped), a record at another place (reordered),
//! another edge, or another exit status. [`spine`] reads under the shared lock and writes nothing.

use super::error::KernelError;
use super::events::{Answered, Chosen, Completed, KernelEvent};
use super::ir::Iteration;
use super::replay::{body, pinned, rejected};
use crate::ids::InstanceId;
use crate::ledger::ReadGuard;
use serde::Serialize;
use std::collections::BTreeMap;

/// An instance's accepting spine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Spine {
    pub instance: String,
    pub flow: String,
    pub ir_hash: String,
    pub records: Vec<SpineRecord>,
}

/// One (node, iteration) with an accepted receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpineRecord {
    pub node: String,
    pub iteration: Iteration,
    /// The edge a choice node took; none for any other node.
    pub edge: Option<String>,
    /// The exit status of the verify the kernel ran; none where it ran none.
    pub exit: Option<i32>,
}

impl SpineRecord {
    fn key(&self) -> (&str, &Iteration) {
        (&self.node, &self.iteration)
    }
}

/// Which of the two compared instances a record is missing from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Instance,
    Against,
}

/// The first difference between two spines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "difference", rename_all = "snake_case")]
pub enum Difference {
    /// The instances run different flows, or different definitions of one.
    Flow { flow: String, ir_hash: String, against_flow: String, against_ir_hash: String },
    /// A (node, iteration) one spine has at `index` and the other has nowhere.
    Skipped { node: String, iteration: Iteration, index: usize, missing_from: Side },
    /// A (node, iteration) both have, at `index` in the instance and `against_index` in the other.
    Reordered { node: String, iteration: Iteration, index: usize, against_index: usize },
    /// Another edge at the same place.
    Edge { node: String, iteration: Iteration, index: usize, edge: Option<String>, against: Option<String> },
    /// Another verify exit status at the same place.
    Exit { node: String, iteration: Iteration, index: usize, exit: Option<i32>, against: Option<i32> },
}

impl std::fmt::Display for Difference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "none".to_string());
        let exit = |v: &Option<i32>| v.map_or_else(|| "none".to_string(), |e| e.to_string());
        match self {
            Difference::Flow { flow, ir_hash, against_flow, against_ir_hash } => {
                write!(f, "the instance runs {flow} ({ir_hash}), the other {against_flow} ({against_ir_hash})")
            }
            Difference::Skipped { node, iteration, index, missing_from } => {
                let (has, lacks) = match missing_from {
                    Side::Against => ("the instance", "the other"),
                    Side::Instance => ("the other", "the instance"),
                };
                write!(f, "{has} accepted {node}{} at step {index}; {lacks} never did", iter(iteration))
            }
            Difference::Reordered { node, iteration, index, against_index } => write!(
                f,
                "{node}{} was accepted at step {index} in the instance and {against_index} in the other",
                iter(iteration)
            ),
            Difference::Edge { node, iteration, index, edge, against } => write!(
                f,
                "at step {index}, {node}{} took edge {} in the instance and {} in the other",
                iter(iteration),
                show(edge),
                show(against)
            ),
            Difference::Exit { node, iteration, index, exit: e, against } => write!(
                f,
                "at step {index}, {node}{} was accepted with exit {} in the instance and {} in the other",
                iter(iteration),
                exit(e),
                exit(against)
            ),
        }
    }
}

/// ` (iteration {...})` inside a repeat; nothing outside one.
fn iter(iteration: &Iteration) -> String {
    if iteration.is_empty() {
        return String::new();
    }
    format!(" (iteration {})", serde_json::to_string(iteration).unwrap_or_default())
}

/// The spine of `instance`. See the module docs. Reads under the shared lock; writes nothing.
pub fn spine(guard: &ReadGuard, instance: &InstanceId) -> Result<Spine, KernelError> {
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    let set_aside = rejected(&log, &started)?;
    let mut records: Vec<SpineRecord> = Vec::new();
    let mut place: BTreeMap<(String, Iteration), usize> = BTreeMap::new();
    for e in log.events.iter().filter(|e| !set_aside.contains(&e.seq)) {
        let record = match e.kind.as_str() {
            Completed::KIND => {
                let c: Completed = body(&log, e)?;
                SpineRecord { node: c.node, iteration: c.iteration, edge: None, exit: c.verify.map(|v| v.exit) }
            }
            Chosen::KIND => {
                let c: Chosen = body(&log, e)?;
                SpineRecord { node: c.node, iteration: c.iteration, edge: Some(c.edge), exit: None }
            }
            Answered::KIND => {
                let a: Answered = body(&log, e)?;
                SpineRecord { node: a.node, iteration: a.iteration, edge: None, exit: None }
            }
            _ => continue,
        };
        let key = (record.node.clone(), record.iteration.clone());
        match place.get(&key) {
            Some(&i) => records[i] = record,
            None => {
                place.insert(key, records.len());
                records.push(record);
            }
        }
    }
    Ok(Spine { instance: instance.to_string(), flow: started.flow, ir_hash: started.ir_hash, records })
}

/// The first difference between the spines of `instance` and `against`, or none when they are
/// equal. See the module docs.
pub fn compare(instance: &Spine, against: &Spine) -> Option<Difference> {
    if instance.flow != against.flow || instance.ir_hash != against.ir_hash {
        return Some(Difference::Flow {
            flow: instance.flow.clone(),
            ir_hash: instance.ir_hash.clone(),
            against_flow: against.flow.clone(),
            against_ir_hash: against.ir_hash.clone(),
        });
    }
    let (a, b) = (&instance.records, &against.records);
    let find = |records: &[SpineRecord], r: &SpineRecord| records.iter().position(|x| x.key() == r.key());
    let skipped = |r: &SpineRecord, index: usize, missing_from: Side| Difference::Skipped {
        node: r.node.clone(),
        iteration: r.iteration.clone(),
        index,
        missing_from,
    };
    for index in 0..a.len().max(b.len()) {
        let (x, y) = match (a.get(index), b.get(index)) {
            (Some(x), Some(y)) => (x, y),
            (Some(x), None) => return Some(skipped(x, index, Side::Against)),
            (None, Some(y)) => return Some(skipped(y, index, Side::Instance)),
            (None, None) => break,
        };
        if x.key() != y.key() {
            // A record the other spine lacks is skipped there; one it has elsewhere is reordered.
            let Some(against_index) = find(b, x) else {
                return Some(skipped(x, index, Side::Against));
            };
            if find(a, y).is_none() {
                return Some(skipped(y, index, Side::Instance));
            }
            return Some(Difference::Reordered {
                node: x.node.clone(),
                iteration: x.iteration.clone(),
                index,
                against_index,
            });
        }
        if x.edge != y.edge {
            return Some(Difference::Edge {
                node: x.node.clone(),
                iteration: x.iteration.clone(),
                index,
                edge: x.edge.clone(),
                against: y.edge.clone(),
            });
        }
        if x.exit != y.exit {
            return Some(Difference::Exit {
                node: x.node.clone(),
                iteration: x.iteration.clone(),
                index,
                exit: x.exit,
                against: y.exit,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(node: &str, edge: Option<&str>, exit: Option<i32>) -> SpineRecord {
        SpineRecord { node: node.into(), iteration: Iteration::new(), edge: edge.map(str::to_string), exit }
    }

    fn spine_of(records: Vec<SpineRecord>) -> Spine {
        Spine { instance: "fi_a".into(), flow: "f@1".into(), ir_hash: "sha256:a".into(), records }
    }

    fn kind(d: Option<Difference>) -> String {
        d.map_or("equal".into(), |d| serde_json::to_value(d).unwrap()["difference"].as_str().unwrap().to_string())
    }

    #[test]
    fn spines_compare_on_flow_records_edges_and_exits_in_order() {
        let base = vec![rec("pin", None, Some(0)), rec("pick", Some("ship"), None), rec("check", None, Some(0))];
        let a = spine_of(base.clone());
        assert_eq!(compare(&a, &spine_of(base.clone())), None);
        // Another instance id is not a difference; another flow or definition is.
        assert_eq!(compare(&a, &Spine { instance: "fi_b".into(), ..a.clone() }), None);
        assert_eq!(kind(compare(&a, &Spine { flow: "f@2".into(), ..a.clone() })), "flow");
        assert_eq!(kind(compare(&a, &Spine { ir_hash: "sha256:b".into(), ..a.clone() })), "flow");

        let mut edge = base.clone();
        edge[1].edge = Some("hold".into());
        assert_eq!(kind(compare(&a, &spine_of(edge))), "edge");
        let mut exit = base.clone();
        exit[2].exit = Some(1);
        assert_eq!(kind(compare(&a, &spine_of(exit))), "exit");
        let mut iteration = base.clone();
        iteration[2].iteration = Iteration::from([("rounds".to_string(), 1)]);
        assert_eq!(kind(compare(&a, &spine_of(iteration))), "skipped");

        // Skipped, on either side, at the end or in the middle; reordered when both have it.
        let short = spine_of(base[..2].to_vec());
        let d = compare(&a, &short).unwrap();
        assert_eq!(
            d,
            Difference::Skipped {
                node: "check".into(),
                iteration: Iteration::new(),
                index: 2,
                missing_from: Side::Against
            }
        );
        assert_eq!(d.to_string(), "the instance accepted check at step 2; the other never did");
        assert_eq!(
            compare(&short, &a),
            Some(Difference::Skipped {
                node: "check".into(),
                iteration: Iteration::new(),
                index: 2,
                missing_from: Side::Instance
            })
        );
        let gap = spine_of(vec![base[0].clone(), base[2].clone()]);
        assert_eq!(
            compare(&a, &gap),
            Some(Difference::Skipped {
                node: "pick".into(),
                iteration: Iteration::new(),
                index: 1,
                missing_from: Side::Against
            })
        );
        assert_eq!(
            compare(&gap, &a),
            Some(Difference::Skipped {
                node: "pick".into(),
                iteration: Iteration::new(),
                index: 1,
                missing_from: Side::Instance
            })
        );
        let swapped = spine_of(vec![base[0].clone(), base[2].clone(), base[1].clone()]);
        assert_eq!(
            compare(&a, &swapped),
            Some(Difference::Reordered {
                node: "pick".into(),
                iteration: Iteration::new(),
                index: 1,
                against_index: 2
            })
        );
    }
}
