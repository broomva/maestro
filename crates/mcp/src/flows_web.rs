//! The Flows pages' read: every instance in the workspace's flow ledger, with the IR its `Started`
//! event pinned, the kernel's own replay at the read's clock and the stop gate's judgment, as
//! [`maestro_view::flows`] models them. Read under the shared lock and writing nothing, exactly as
//! `maestro_flow_status` reads one instance ([`crate::flow`]): the same root rule, the same
//! landing of answers, the same assessment. A ledger that cannot be read is said, never empty.

use maestro_flow::kernel::events::Owner;
use maestro_flow::kernel::ir::{Class, Input, Ir, Need};
use maestro_flow::kernel::stop::{Waiting, Work};
use maestro_flow::kernel::{self, Reason};
use maestro_flow::log::Event;
use maestro_flow::{InstanceId, Options, ReadGuard, Reader};
use maestro_store::Paths;
use maestro_view::flows as view;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Read the whole flow ledger for the pages, under one shared lock taken once, as
/// `maestro_flow_status` takes it for one instance. A lock that cannot be had (a writer holding it
/// past the lock's wait) fails the whole read once, said as unread; it never costs a wait per run.
/// Callers go through [`read_shared`], which bounds how often this runs.
pub fn read(paths: &Paths) -> view::Ledger {
    let unread = |root: String, why: String| view::Ledger {
        root,
        host: None,
        read: Err(why),
        unread: vec![],
        read_at: Some(maestro_flow::clock::now()),
    };
    let root = match crate::flow::flows_root(&paths.workspace) {
        Ok(root) => root,
        Err(refusal) => {
            let why = refusal.get("reason").and_then(Value::as_str).unwrap_or("the flows root was refused");
            return unread(paths.workspace.join(crate::flow::FLOWS).display().to_string(), why.to_string());
        }
    };
    let shown = root.display().to_string();
    let guard = match Reader::new(&root, Options::default()).and_then(|r| r.lock()) {
        Ok(g) => g,
        Err(e) => return unread(shown, e.to_string()),
    };
    let ids = match guard.instances() {
        Ok(ids) => ids,
        Err(e) => return unread(shown, e.to_string()),
    };
    // The clock is read under the lock, as status reads it, so no receipt is newer than the read.
    let now = maestro_flow::clock::now();
    let (mut read, mut unread) = (Vec::new(), Vec::new());
    for id in ids {
        match instance(paths, &guard, &id, now) {
            Ok(i) => read.push(i),
            Err(reason) => unread.push(view::Unread { instance: id.as_str().to_string(), reason }),
        }
    }
    view::Ledger { root: shown, host: Some(guard.host().to_string()), read: Ok(read), unread, read_at: Some(now) }
}

/// How long a finished walk serves every page, counted from when it finished.
pub const SHARED_FOR: std::time::Duration = std::time::Duration::from_secs(1);

/// [`read`], shared by every page of a workspace. One walk runs at a time, and a walk serves every
/// caller for [`SHARED_FOR`] after it finished. So however many tabs poll, the ledger is walked at
/// most once per window and the pages then leave the flows lock alone for the rest of it. Shared
/// locks are not queued behind a waiting writer (it retries a held lock every few milliseconds for
/// ten seconds), so it is this gap that bounds a writer's wait to about one walk; back-to-back walks
/// would leave it only the instants between them.
pub fn read_shared(paths: &Paths) -> Arc<view::Ledger> {
    static LAST: Mutex<BTreeMap<PathBuf, (Instant, Arc<view::Ledger>)>> = Mutex::new(BTreeMap::new());
    let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((done, ledger)) = last.get(&paths.workspace)
        && done.elapsed() < SHARED_FOR
    {
        return ledger.clone();
    }
    let ledger = Arc::new(read(paths));
    last.insert(paths.workspace.clone(), (Instant::now(), ledger.clone()));
    ledger
}

fn instance(paths: &Paths, guard: &ReadGuard, id: &InstanceId, now: jiff::Timestamp) -> Result<view::Instance, String> {
    let log = guard.log(id).map_err(|e| e.to_string())?;
    let started = log.events.first().filter(|e| e.kind == "Started").ok_or("its log has no Started event")?;
    let body = &started.body;
    let flow = body.get("flow").and_then(Value::as_str).ok_or("Started names no flow")?;
    let ir: Ir = serde_json::from_value(body.get("ir").cloned().unwrap_or_default())
        .map_err(|e| format!("Started's IR does not read: {e}"))?;
    let name = flow.rsplit_once('@').map_or(flow, |(n, _)| n).to_string();
    let starter =
        body.get("owner").cloned().and_then(|o| serde_json::from_value::<Owner>(o).ok()).map(|o| o.session.to_string());
    let replay = live(paths, guard, id, now);
    // What replay set aside, by sequence number; none is marked when replay did not run, and the
    // page says no event is then known to have been accepted.
    let rejected = replay.as_ref().map(|(_, r)| r.clone()).unwrap_or_default();
    let params = body
        .get("params")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), str::to_string))).collect())
        .unwrap_or_default();
    Ok(view::Instance {
        id: id.as_str().to_string(),
        flow: name,
        version: ir.version,
        ir_hash: body.get("ir_hash").and_then(Value::as_str).unwrap_or_default().to_string(),
        started_at: started.at.clone(),
        last_at: log.events.last().map_or_else(String::new, |e| e.at.clone()),
        seq: log.seq(),
        starter,
        params,
        shape: shape(&ir),
        live: replay.map(|(l, _)| l),
        steps: log.events.iter().map(|e| step(e, rejected.get(&e.seq).cloned())).collect(),
    })
}

fn session(owner: &Value) -> String {
    serde_json::from_value::<Owner>(owner.clone())
        .map_or_else(|_| "an unrecorded session".into(), |o| o.session.to_string())
}

fn argv(v: &[String]) -> String {
    shlex::try_join(v.iter().map(String::as_str)).unwrap_or_else(|_| v.join(" "))
}

/// The view's shape of an IR: its nodes in the IR's order, with each class's settings as facts.
pub fn shape(ir: &Ir) -> view::Shape {
    let nodes = ir
        .nodes
        .iter()
        .map(|(id, n)| {
            let mut facts = Vec::new();
            match &n.class {
                Class::Pure { run } => facts.push(("run".into(), argv(run))),
                Class::Observe { run, fresh_for, inputs, exit_map } => {
                    facts.push(("run".into(), argv(run)));
                    if let Some(f) = fresh_for {
                        facts.push(("fresh for".into(), format!("{f} s")));
                    }
                    let ins: Vec<String> = inputs
                        .iter()
                        .map(|i| match i {
                            Input::Param { name } => format!("param {name}"),
                            Input::Node { node } => format!("node {node}"),
                        })
                        .collect();
                    if !ins.is_empty() {
                        facts.push(("inputs".into(), ins.join(", ")));
                    }
                    for e in exit_map {
                        facts.push((format!("exit {}", e.exit), e.value.clone()));
                    }
                }
                Class::Work { verify, .. } => facts.push(("verify".into(), argv(verify))),
                Class::Choice { edges, .. } => {
                    for (edge, guard) in edges {
                        facts.push((format!("edge {edge}"), guard.clone()));
                    }
                }
                Class::Effect { run, preconditions, reconcile, .. } => {
                    if !run.is_empty() {
                        facts.push(("run".into(), argv(run)));
                    }
                    for p in preconditions {
                        facts.push(("precondition".into(), p.clone()));
                    }
                    facts.push(("reconcile".into(), argv(reconcile)));
                }
                Class::Human { ask, required_body_fields, .. } => {
                    facts.push(("ask".into(), ask.clone()));
                    if !required_body_fields.is_empty() {
                        facts.push(("required fields".into(), required_body_fields.join(", ")));
                    }
                }
                Class::Subflow { uses, .. } => {
                    facts.push(("uses".into(), uses.clone()));
                }
                Class::Map { uses, over, item_key, max_items, .. } => {
                    facts.push(("uses".into(), uses.clone()));
                    facts.push(("over".into(), over.clone()));
                    facts.push(("item key".into(), item_key.clone()));
                    facts.push(("max items".into(), max_items.to_string()));
                }
            }
            if let Some(s) = &n.snapshot {
                facts.push(("snapshot of".into(), s.clone()));
            }
            let launch = n.class.launch();
            view::Node {
                id: id.clone(),
                class: n.class.name().to_string(),
                needs: n
                    .needs
                    .iter()
                    .map(|need| match need {
                        Need::Node { node } => view::Need { on: node.clone(), kind: view::NeedKind::Node },
                        Need::Repeat { repeat } => view::Need { on: repeat.clone(), kind: view::NeedKind::Repeat },
                        Need::Exhausted { repeat } => {
                            view::Need { on: repeat.clone(), kind: view::NeedKind::Exhausted }
                        }
                    })
                    .collect(),
                switch: n.switch.as_ref().map(|c| (c.on.clone(), c.value.clone())),
                performer: launch.and_then(|l| l.performer.clone()),
                skill: launch.and_then(|l| l.skill.clone()),
                routing: launch
                    .map(|l| l.routing.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default(),
                retry: n.retry,
                cancel_after: n.cancel_after,
                wait: n.class.wait().map(|w| (w.external.clone(), w.heartbeat)),
                facts,
            }
        })
        .collect();
    view::Shape {
        ir_version: ir.ir_version,
        params: ir
            .params
            .iter()
            .map(|(p, required)| (p.clone(), *required, ir.types.get(p).map(|t| format!("{t:?}").to_lowercase())))
            .collect(),
        workdir: ir.workdir.clone(),
        nodes,
        repeats: ir
            .repeats
            .iter()
            .map(|(id, r)| view::Repeat {
                id: id.clone(),
                body: r.body.clone(),
                max: r.max,
                until: r.until.clone(),
                on_exhausted: r.on_exhausted.clone(),
            })
            .collect(),
    }
}

fn iteration(i: &kernel::Iteration) -> String {
    i.iter().map(|(r, n)| format!("{r}={n}")).collect::<Vec<_>>().join(", ")
}

/// A replay reason as a phrase.
fn unmet(r: &Reason) -> String {
    match r {
        Reason::Needs { node } => format!("Needs {node}"),
        Reason::Unreachable { node } => format!("{node} will never run"),
        Reason::Switch { on, took, case } => match took {
            Some(t) => format!("{on} took {t}, not {case}"),
            None => format!("Runs only if {on} takes {case}"),
        },
        Reason::RepeatRunning { repeat } => format!("Repeat {repeat} is still running"),
        Reason::RepeatExhausted { repeat } => format!("Repeat {repeat} ran out"),
        Reason::RepeatFinished { repeat } => format!("Repeat {repeat} finished, so this route will not run"),
        Reason::RepeatUnreachable { repeat } => format!("No node of repeat {repeat} will run"),
        Reason::Stale { node } => format!("{node} is stale"),
        Reason::Expired { age_secs, fresh_for_secs } => {
            format!("Observed {age_secs} s ago, fresh for {fresh_for_secs} s")
        }
        Reason::InputChanged { input } => format!("Its input {input} changed"),
        Reason::InputsChanged { .. } => "Its inputs changed since it was asked".into(),
        Reason::ExitUnmapped { exit } => match exit {
            Some(e) => format!("Exit {e} is not in its exit map"),
            None => "No exit status recorded".into(),
        },
        Reason::AttemptsExhausted { attempts } => format!("All {attempts} attempts ended without a receipt"),
        Reason::Unfiled { .. } => "Its ask is not filed yet".into(),
        Reason::Owner { ask_id, ledger, .. } => format!("Waits on ask {ask_id} in {ledger}"),
        Reason::AnswerWaitingToLand { ask_id, ledger, .. } => format!("Waits on ask {ask_id} in {ledger}"),
    }
}

/// Who holds each running node's claim, as the stop gate's assessment judges it: the run's owner,
/// or a former owner whose lease still runs. Never read from raw `Claimed` events, which replay
/// may have set aside.
fn claims(work: &[Work], waiting: &[Waiting]) -> BTreeMap<String, String> {
    let owner = work.iter().filter_map(|w| match w {
        Work::Node { node, held: true, .. } => Some((node.clone(), "Its claim is held by the run's owner".to_string())),
        _ => None,
    });
    let lease = waiting.iter().filter_map(|w| match w {
        Waiting::Lease { node, lease_until, .. } => {
            Some((node.clone(), format!("A former owner's claim holds it until {lease_until}")))
        }
        _ => None,
    });
    owner.chain(lease).collect()
}

/// A performer as the pages name it: its harness and model when known, else its session.
fn performer(p: &Value) -> String {
    let named: Vec<&str> =
        [p["harness"].as_str().filter(|h| *h != "unknown"), p["model"].as_str()].into_iter().flatten().collect();
    if !named.is_empty() {
        return named.join(" ");
    }
    p["session"].as_str().map_or_else(|| "an unrecorded performer".into(), str::to_string)
}

/// The nodes whose claim is held now, by the owner (`Work::Node` held) or by a former owner whose
/// lease still runs (`Waiting::Lease`): the only nodes the pages call running.
fn held<'a>(work: &'a [Work], waiting: &'a [Waiting]) -> Vec<&'a str> {
    let owner = work.iter().filter_map(|w| match w {
        Work::Node { node, held: true, .. } => Some(node.as_str()),
        _ => None,
    });
    let lease = waiting.iter().filter_map(|w| match w {
        Waiting::Lease { node, .. } => Some(node.as_str()),
        _ => None,
    });
    owner.chain(lease).collect()
}

/// Whom a run waits on, as the stop gate's assessment says, without judging it: a wait on a filed
/// ask names the ask (whether it is answered, and by whom, is the Decisions page's to say; the
/// assessment's own reading of that is a hint, BRO-4206), and a lease names its holder's term.
fn wait(w: &Waiting) -> view::Wait {
    match w {
        // The landing branch the assessment names is a hint as well, so the wait names only the ask.
        Waiting::Owner { node, ask_id, ledger, .. } => view::Wait {
            ask: Some((ledger.clone(), ask_id.clone())),
            text: format!("{node}: waits on ask {ask_id} in {ledger}"),
        },
        Waiting::Lease { node, lease_until, .. } => {
            view::Wait { ask: None, text: format!("{node}: a former owner's claim, until {lease_until}") }
        }
    }
}

/// The run as replay and the stop gate judge it, and the events replay set aside (sequence number
/// to why).
fn live(
    paths: &Paths,
    guard: &ReadGuard,
    id: &InstanceId,
    now: jiff::Timestamp,
) -> Result<(view::Live, BTreeMap<u64, String>), String> {
    let mut status = kernel::status(guard, id, now).map_err(|e| format!("{}: {e}", e.code()))?;
    for s in status.nodes.values_mut() {
        kernel::landing(paths, &mut s.unmet);
    }
    kernel::adoption::landing(paths, &mut status.proposals);
    let judged = kernel::stop::assess_instance(guard, id, now, &|l, a| kernel::seen(paths, l, a))
        .map_err(|e| format!("{}: {e}", e.code()))?;
    let held = held(&judged.work, &judged.waiting);
    let claims = claims(&judged.work, &judged.waiting);
    let nodes = status
        .nodes
        .iter()
        .map(|(node, s)| {
            let running = held.contains(&node.as_str());
            let state = if running {
                view::NodeState::Running
            } else {
                match s.state {
                    kernel::State::Done => view::NodeState::Done,
                    kernel::State::Ready => view::NodeState::Ready,
                    kernel::State::Blocked => view::NodeState::Blocked,
                    kernel::State::Stale => view::NodeState::Stale,
                    kernel::State::Waiting => view::NodeState::Waiting,
                    kernel::State::Exhausted => view::NodeState::Exhausted,
                    kernel::State::Failed => view::NodeState::Failed,
                }
            };
            let live = view::NodeLive {
                state,
                unmet: s.unmet.iter().map(unmet).collect(),
                iteration: iteration(&s.iteration),
                claim: claims.get(node).cloned(),
            };
            (node.clone(), live)
        })
        .collect();
    let work = judged
        .work
        .iter()
        .map(|w| match w {
            Work::Node { node, class, held: true, .. } => format!("{node} ({class}) is running: finish its claim"),
            Work::Node { node, class, .. } => format!("{node} ({class}) is ready to claim"),
            Work::Verdict { repeat, iteration, .. } => {
                format!("Repeat {repeat} iteration {iteration} waits for its until verdict")
            }
            Work::Ask { node, .. } => format!("{node}: file its ask"),
            Work::Answer { node, ask_id, .. } => {
                format!("{node}: take the answer to ask {ask_id} with maestro_flow_ask, which reads it from main")
            }
        })
        .collect();
    let waiting = judged.waiting.iter().map(wait).collect();
    let verdicts = judged
        .work
        .iter()
        .filter_map(|w| match w {
            Work::Verdict { repeat, .. } => Some(repeat.clone()),
            _ => None,
        })
        .collect();
    let rejected = status.rejected.iter().map(|r| (r.seq, r.reason.clone())).collect();
    let label = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_string);
    let live = view::Live {
        terminal: status.terminal,
        retired: status.retired.clone(),
        nodes,
        repeats: status
            .repeats
            .iter()
            .map(|(id, r)| (id.clone(), label(&serde_json::to_value(r.state).unwrap_or_default()), r.iteration))
            .collect(),
        work,
        waiting,
        proposals: status
            .proposals
            .iter()
            .map(|p| {
                let kind = label(&serde_json::to_value(p.proposal).unwrap_or_default());
                format!("{kind} by {} (ask {} in {}): {}", p.by, p.ask_id, p.ledger, p.state.replace('_', " "))
            })
            .collect(),
        owner: status.owner.as_ref().map(ToString::to_string),
        verdicts,
        rejected: status.rejected.len(),
        stops_let_through: status.stop_unblockable.len(),
    };
    Ok((live, rejected))
}

/// One log line as the activity list shows it.
fn step(e: &Event, rejected: Option<String>) -> view::Step {
    let b = &e.body;
    let s = |k: &str| b.get(k).and_then(Value::as_str);
    let subject = s("node").or(s("repeat")).map(str::to_string);
    let performer = || performer(&b["performer"]);
    let attempt = b["attempt"].as_u64().map(|a| format!("attempt {a}"));
    let note = match e.kind.as_str() {
        "Started" => format!("{} by {}", s("flow").unwrap_or("a flow"), session(&b["owner"])),
        "Claimed" => {
            [Some(format!("by {}", performer())), attempt, s("lease_until").map(|l| format!("lease until {l}"))]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ")
        }
        "Heartbeat" => s("lease_until").map_or_else(String::new, |l| format!("lease until {l}")),
        "Completed" => {
            let exit = b["verify"]["exit"].as_i64().map(|x| format!("verify exit {x}"));
            [exit, Some(format!("by {}", performer())), attempt].into_iter().flatten().collect::<Vec<_>>().join(", ")
        }
        "Chosen" => s("edge").map_or_else(String::new, |edge| format!("took {edge}")),
        "Guarded" => match b["holds"].as_bool() {
            Some(true) => "until holds".into(),
            Some(false) => "until does not hold yet".into(),
            None => "verdict unrecorded".into(),
        },
        "Refused" | "AskWithdrawn" | "ProposalWithdrawn" => {
            [s("code"), s("reason")].into_iter().flatten().collect::<Vec<_>>().join(": ")
        }
        "AskPinned" | "AskLinked" | "Answered" | "ProposalPinned" | "ProposalLinked" => {
            [s("ask_id"), s("ledger")].into_iter().flatten().collect::<Vec<_>>().join(" in ")
        }
        "Adopted" => format!("from {} to {}", session(&b["from"]), session(&b["to"])),
        "Retired" => format!("by {}", session(&b["by"])),
        "StopUnblockable" | "StopUnverified" => {
            [s("harness"), s("reason")].into_iter().flatten().collect::<Vec<_>>().join(": ")
        }
        _ => String::new(),
    };
    let note: String = note.chars().take(400).collect();
    view::Step { seq: e.seq, at: e.at.clone(), kind: e.kind.clone(), subject, note, rejected }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn owner(landing: Option<&str>) -> Waiting {
        Waiting::Owner {
            node: "approve".into(),
            iteration: BTreeMap::new(),
            ask_id: "a1".into(),
            ledger: "flows.yaml".into(),
            landing: landing.map(str::to_string),
            supersedes: None,
            reason: None,
        }
    }

    #[test]
    fn a_wait_names_its_ask_and_judges_no_answer() {
        let w = wait(&owner(None));
        assert_eq!(w.ask, Some(("flows.yaml".to_string(), "a1".to_string())));
        assert!(!w.text.contains("owner's"), "the wait does not say whose answer it needs: {}", w.text);
        let landing = wait(&owner(Some("maestro/ledger")));
        assert_eq!(landing.ask, Some(("flows.yaml".to_string(), "a1".to_string())));
        assert_eq!(landing.text, "approve: waits on ask a1 in flows.yaml", "a landing hint is not said as an answer");
        assert!(!w.text.contains("answered") && !landing.text.contains("answered"), "{} / {}", w.text, landing.text);
        let lease = Waiting::Lease { node: "build".into(), iteration: BTreeMap::new(), lease_until: "t".into() };
        assert_eq!(wait(&lease).ask, None, "a lease waits on no ask");
    }

    #[test]
    fn running_is_a_held_claim_or_a_live_lease_and_nothing_else() {
        let node =
            |n: &str, held| Work::Node { node: n.into(), class: "work".into(), iteration: BTreeMap::new(), held };
        let work = [node("mine", true), node("ready", false)];
        let lease = [Waiting::Lease { node: "theirs".into(), iteration: BTreeMap::new(), lease_until: "t".into() }];
        assert_eq!(held(&work, &lease), ["mine", "theirs"]);
        assert_eq!(held(&work, &[owner(None)]), ["mine"], "an owner wait is not a claim");
    }
}
