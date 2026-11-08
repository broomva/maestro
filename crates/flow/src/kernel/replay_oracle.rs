//! A differential oracle (from P20 round 4, stratum B): for random logs over flows that exercise
//! repeats, switches, forks and freshness, the fold that keeps cores across events must read
//! exactly as one that keeps nothing, at every prefix and at several clocks. The default run is
//! small; `ORACLE_SEEDS` and `ORACLE_LEN` run it longer.
use super::*;
use crate::kernel::compile;
use serde_json::json;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pct(&mut self, p: u64) -> bool {
        self.below(100) < p
    }
}

const DEFS: &[(&str, &str)] = &[
    ("verify", include_str!("../../flows/verify.yaml")),
    (
        "poll",
        "flow: f
version: 1
nodes:
  ci: {class: observe, run: [x], fresh_for: 10m, exit_map: {0: green, 1: red}}
  land: {class: work, verify: [x], needs: [poll]}
  give_up: {class: work, verify: [x]}
repeats:
  poll: {body: [ci], max: 3, until: '.nodes.ci.value == \"green\"', on_exhausted: give_up}
",
    ),
    (
        "nountil",
        "flow: f
version: 1
nodes:
  pre: {class: observe, run: [x], fresh_for: 10m, exit_map: {0: ok, 1: no}}
  w: {class: work, verify: [x], needs: [pre]}
  ci: {class: observe, run: [x], fresh_for: 10m, needs: [w], exit_map: {0: green, 1: red}}
  land: {class: effect, needs: [poll], reconcile: [x]}
  give_up: {class: work, verify: [x]}
  peek: {class: pure, run: [x], needs: [ci]}
repeats:
  poll: {body: [w, ci], max: 3, on_exhausted: give_up}
",
    ),
    (
        "forkswitch",
        "flow: f
version: 1
nodes:
  o1: {class: observe, run: [x], fresh_for: 5m, exit_map: {0: a, 1: b}}
  o2: {class: observe, run: [x], fresh_for: 1h, needs: [o1], exit_map: {0: ok}}
  pick: {class: choice, needs: [o2], edges: {x: {guard: 'true'}, y: {guard: 'true'}}}
  bx: {class: work, verify: [x], switch: {on: pick, case: x}}
  by: {class: work, verify: [x], switch: {on: pick, case: y}}
  sw1: {class: pure, run: [x], switch: {on: o1, case: a}}
  p1: {class: pure, run: [x], needs: [o1]}
  p2: {class: pure, run: [x], needs: [o2]}
  join: {class: effect, needs: [both], reconcile: [x]}
  after: {class: work, verify: [x], needs: [bx, join]}
  ask: {class: human, ask: a1, needs: [by]}
forks:
  both: {all: [p1, p2, sw1]}
",
    ),
    (
        "cross",
        "flow: f
version: 1
nodes:
  src: {class: observe, run: [x], fresh_for: 10m, exit_map: {0: ok, 1: no}}
  a1: {class: pure, run: [x], needs: [src]}
  a2: {class: observe, run: [x], fresh_for: 10m, needs: [a1], exit_map: {0: g, 1: r}}
  b1: {class: work, verify: [x], needs: [a2]}
  b2: {class: pure, run: [x], needs: [b1]}
  out1: {class: pure, run: [x], needs: [a1]}
  out2: {class: work, verify: [x], needs: [r2]}
  out3: {class: pure, run: [x], needs: [out1, b2]}
  ex: {class: work, verify: [x]}
repeats:
  r1: {body: [a1, a2], max: 3, until: '.nodes.a2.value == \"g\"', on_exhausted: ex}
  r2: {body: [b1, b2], max: 2}
",
    ),
    (
        "choicebody",
        "flow: f
version: 1
nodes:
  pick: {class: choice, edges: {quick: {guard: 'true'}, deep: {guard: 'true'}}}
  skim: {class: work, verify: [x], switch: {on: pick, case: quick}}
  dig: {class: observe, run: [x], fresh_for: 2m, switch: {on: pick, case: deep}, exit_map: {0: y, 1: n}}
  after_dig: {class: work, verify: [x], needs: [dig]}
  after: {class: effect, needs: [tries], reconcile: [x]}
  tail: {class: pure, run: [x], needs: [after]}
  noway: {class: work, verify: [x]}
repeats:
  tries: {body: [pick, skim, dig, after_dig], max: 3, until: '.nodes.pick.value == \"quick\"', on_exhausted: noway}
",
    ),
    (
        "merge",
        "flow: f
version: 1
nodes:
  head: {class: observe, run: [x], fresh_for: 3m, exit_map: {0: same, 1: moved}}
  review: {class: work, verify: [x], needs: [head]}
  fix: {class: work, verify: [x], needs: [review]}
  merge: {class: effect, needs: [rounds, head], reconcile: [x]}
  owner: {class: human, ask: x, needs: [rounds]}
repeats:
  rounds: {body: [review, fix], max: 5, until: '.nodes.review.outputs.verdict == \"pass\"', on_exhausted: owner}
",
    ),
    (
        "twonountil",
        "flow: f
version: 1
nodes:
  x1: {class: work, verify: [x]}
  x2: {class: observe, run: [x], fresh_for: 10m, exit_map: {0: ok, 1: no}}
  y1: {class: work, verify: [x], needs: [x2]}
  out: {class: effect, needs: [r2], reconcile: [x]}
  e1: {class: work, verify: [x]}
  e2: {class: pure, run: [x], needs: [x1]}
repeats:
  r1: {body: [x1, x2], max: 2, on_exhausted: e1}
  r2: {body: [y1], max: 2}
",
    ),
    (
        "cross2",
        "flow: f
version: 1
nodes:
  a1: {class: work, verify: [x]}
  a2: {class: observe, run: [x], fresh_for: 10m, exit_map: {0: g, 1: r}}
  b: {class: pure, run: [x], needs: [a2]}
  c1: {class: work, verify: [x], needs: [a2]}
  d: {class: pure, run: [x], needs: [b]}
  ex: {class: work, verify: [x]}
repeats:
  r1: {body: [a1, a2], max: 2, until: '.nodes.a2.value == \"g\"', on_exhausted: ex}
  r2: {body: [c1], max: 2}
",
    ),
];

fn t0() -> Timestamp {
    "2026-11-08T00:00:00Z".parse().unwrap()
}

fn mk(seq: u64, at: Timestamp, kind: &str, body: Value) -> Event {
    Event { seq, prev_event: None, at: at.to_string(), kind: kind.into(), body }
}

type NextView = Vec<(String, Iteration, BTreeMap<String, String>)>;

fn next_of(fold: &Fold, log: &Log, started: &Started, now: Timestamp) -> (String, NextView) {
    let (status, stale) = fold.read(log, started, now);
    let js = serde_json::to_string(&status).unwrap();
    let mut enabled = Vec::new();
    for (id, s) in status.nodes {
        let rerun = s.state == State::Stale && stale[&id].iter().all(|x| *x == id);
        if s.state == State::Ready || rerun {
            enabled.push((id, s.iteration, s.inputs));
        }
    }
    (js, enabled)
}

fn view(log: &Log, started: &Started, cache: bool, clocks: &[Timestamp]) -> Vec<(String, NextView)> {
    NO_CACHE.with(|c| c.set(!cache));
    let fold = Fold::replay(log, started).unwrap();
    let out = clocks.iter().map(|t| next_of(&fold, log, started, *t)).collect();
    NO_CACHE.with(|c| c.set(false));
    out
}

/// No-cache reference, one fresh fold per clock.
fn reference(log: &Log, started: &Started, clocks: &[Timestamp]) -> Vec<(String, NextView)> {
    clocks.iter().map(|t| view(log, started, false, &[*t]).remove(0)).collect()
}

fn gen_event(rng: &mut Rng, ir: &Ir, st: &Status, seq: u64, at: Timestamp) -> Event {
    let nodes: Vec<&String> = ir.nodes.keys().collect();
    let reps: Vec<&String> = ir.repeats.keys().collect();
    let roll = rng.below(100);
    if roll < 18 && !reps.is_empty() {
        let r = reps[rng.below(reps.len() as u64) as usize];
        let cur = st.repeats[r].iteration;
        let i = if rng.pct(85) { cur } else { rng.below(4) as u32 };
        return mk(seq, at, "Guarded", json!({"repeat": r, "iteration": i, "holds": rng.pct(50)}));
    }
    let n = nodes[rng.below(nodes.len() as u64) as usize];
    let ns = &st.nodes[n];
    let mut iteration = ns.iteration.clone();
    if rng.pct(8)
        && let Some((_, v)) = iteration.iter_mut().next()
    {
        *v = rng.below(3) as u32;
    }
    let mut inputs = ns.inputs.clone();
    if rng.pct(10)
        && let Some((_, v)) = inputs.iter_mut().next()
    {
        *v = "sha256:bogus".into();
    }
    if rng.pct(4) {
        inputs.clear();
    }
    if roll < 30 {
        let kind = ["Claimed", "Heartbeat", "Refused"][rng.below(3) as usize];
        return mk(
            seq,
            at,
            kind,
            json!({"node": n, "iteration": iteration, "attempt": 1, "lease_until": at.to_string(), "token_sha256": "x", "code": "c", "reason": "r"}),
        );
    }
    match &ir.nodes[n].class {
        Class::Choice { edges, .. } => {
            let es: Vec<&String> = edges.keys().collect();
            let edge = if rng.pct(8) { "zzz".to_string() } else { es[rng.below(es.len() as u64) as usize].clone() };
            if rng.pct(5) {
                return mk(
                    seq,
                    at,
                    "Completed",
                    json!({"node": n, "iteration": iteration, "attempt": 1, "inputs": inputs}),
                );
            }
            mk(
                seq,
                at,
                "Chosen",
                json!({"node": n, "iteration": iteration, "attempt": 1, "edge": edge, "inputs": inputs}),
            )
        }
        Class::Observe { exit_map, .. } => {
            let exit = if rng.pct(8) || exit_map.is_empty() {
                9
            } else {
                exit_map[rng.below(exit_map.len() as u64) as usize].exit
            };
            let verify = if rng.pct(3) {
                Value::Null
            } else {
                json!({"argv": ["x"], "exit": exit, "stdout_sha256": "s", "ran_at": "x"})
            };
            mk(
                seq,
                at,
                "Completed",
                json!({"node": n, "iteration": iteration, "attempt": 1, "inputs": inputs, "outputs": null, "verify": verify}),
            )
        }
        _ => {
            let outputs = match rng.below(4) {
                0 => Value::Null,
                1 => json!({"verdict": "pass"}),
                2 => json!({"verdict": "fail"}),
                _ => json!({"v": rng.below(2)}),
            };
            mk(
                seq,
                at,
                "Completed",
                json!({"node": n, "iteration": iteration, "attempt": 1, "inputs": inputs, "outputs": outputs}),
            )
        }
    }
}

fn run_def(name: &str, yaml: &str, seeds: u64, len: usize) -> (usize, usize) {
    let flow = compile(yaml).unwrap_or_else(|e| panic!("{name}: {e}"));
    let params = if name == "verify" { json!({"repo": "r", "head": "h", "check": "c"}) } else { json!({}) };
    let started =
        Started { flow: flow.name(), ir_hash: flow.hash().into(), ir: flow.ir().clone(), params, owner: None };
    let ir = flow.ir();
    let mut checks = 0;
    let mut accepted = 0;
    for seed in 0..seeds {
        let mut rng = Rng(0x9E3779B97F4A7C15 ^ (seed.wrapping_mul(0x2545F4914F6CDD1D) + 1));
        let mut events = vec![mk(1, t0(), "Started", serde_json::to_value(&started).unwrap())];
        let mut clock = t0();
        for k in 0..len {
            let step = match rng.below(10) {
                0 | 1 => SignedDuration::ZERO,
                2 | 3 => SignedDuration::from_secs(1),
                4 | 5 => SignedDuration::from_secs(30),
                6 => SignedDuration::from_mins(3),
                7 => SignedDuration::from_mins(11),
                8 => SignedDuration::from_hours(2),
                _ => SignedDuration::from_mins(-7),
            };
            clock = clock.checked_add(step).unwrap();
            let log = Log { instance: InstanceId::new("fi_x").unwrap(), events: events.clone(), last_hash: None };
            let st = {
                NO_CACHE.with(|c| c.set(true));
                let f = Fold::replay(&log, &started).unwrap();
                let s = f.read(&log, &started, clock).0;
                NO_CACHE.with(|c| c.set(false));
                s
            };
            let e = gen_event(&mut rng, ir, &st, k as u64 + 2, clock);
            events.push(e);
            let log = Log { instance: InstanceId::new("fi_x").unwrap(), events: events.clone(), last_hash: None };
            let clocks = [
                clock,
                clock + SignedDuration::from_secs(59),
                clock + SignedDuration::from_mins(61),
                clock + SignedDuration::from_hours(3),
                clock - SignedDuration::from_hours(1),
                clock + SignedDuration::from_mins(4),
            ];
            let cached = view(&log, &started, true, &clocks);
            let refv = reference(&log, &started, &clocks);
            checks += clocks.len();
            for (j, (c, r)) in cached.iter().zip(refv.iter()).enumerate() {
                if c != r {
                    let dump: Vec<String> =
                        events.iter().skip(1).map(|e| format!("{} {} {}", e.at, e.kind, e.body)).collect();
                    panic!(
                        "DIVERGENCE def={name} seed={seed} step={k} clock#{j}={}\nlog:\n{}\ncached:\n{}\n{:?}\nref:\n{}\n{:?}",
                        clocks[j],
                        dump.join("\n"),
                        c.0,
                        c.1,
                        r.0,
                        r.1
                    );
                }
            }
            let s: Status = serde_json::from_str::<Value>(&cached[0].0)
                .map(|_| ())
                .map(|()| {
                    NO_CACHE.with(|c| c.set(false));
                    let f = Fold::replay(&log, &started).unwrap();
                    f.read(&log, &started, clock).0
                })
                .unwrap();
            accepted = accepted.max(events.len() - 1 - s.rejected.len());
        }
    }
    (checks, accepted)
}

#[test]
fn oracle_cached_fold_matches_no_cache_fold() {
    let seeds: u64 = std::env::var("ORACLE_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let len: usize = std::env::var("ORACLE_LEN").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    for (name, yaml) in DEFS {
        let (checks, acc) = run_def(name, yaml, seeds, len);
        eprintln!("oracle {name}: {checks} comparisons, max accepted {acc}");
    }
}

const CROSS3: &str = "flow: f
version: 1
params: [p?]
nodes:
  u: {class: observe, run: [x], inputs: [params.p], exit_map: {0: one, 1: two}}
  a1: {class: work, verify: [x], needs: [u]}
  a2: {class: observe, run: [x], inputs: [params.p], exit_map: {0: g, 1: r}}
  b: {class: pure, run: [x], needs: [a2]}
  d: {class: pure, run: [x], needs: [b]}
repeats:
  r1: {body: [a1, a2], max: 2, until: '.nodes.a2.value == \"g\"'}
";

fn fold_status(log: &Log, started: &Started, cache: bool, now: Timestamp) -> Status {
    NO_CACHE.with(|c| c.set(!cache));
    let f = Fold::replay(log, started).unwrap();
    let s = f.read(log, started, now).0;
    NO_CACHE.with(|c| c.set(false));
    s
}

#[test]
fn cross3_outside_reader_of_body_node_sees_verdict_revoked_by_upstream_of_other_body_node() {
    let flow = compile(CROSS3).unwrap();
    let started = Started {
        flow: flow.name(),
        ir_hash: flow.hash().into(),
        ir: flow.ir().clone(),
        params: json!({}),
        owner: None,
    };
    let mut events = vec![mk(1, t0(), "Started", serde_json::to_value(&started).unwrap())];
    let push = |events: &mut Vec<Event>, kind: &str, body: Value| {
        let seq = events.len() as u64 + 1;
        events.push(mk(seq, t0(), kind, body));
    };
    let log_of = |events: &Vec<Event>| Log {
        instance: InstanceId::new("fi_x").unwrap(),
        events: events.clone(),
        last_hash: None,
    };
    let inputs =
        |events: &Vec<Event>, n: &str| fold_status(&log_of(events), &started, false, t0()).nodes[n].inputs.clone();
    let obs = |exit: i32| json!({"argv": ["x"], "exit": exit, "stdout_sha256": "s", "ran_at": "x"});
    push(&mut events, "Completed", json!({"node": "u", "iteration": {}, "attempt": 1, "verify": obs(0)}));
    let i = inputs(&events, "a1");
    push(&mut events, "Completed", json!({"node": "a1", "iteration": {"r1": 0}, "attempt": 1, "inputs": i}));
    push(&mut events, "Completed", json!({"node": "a2", "iteration": {"r1": 0}, "attempt": 1, "verify": obs(0)}));
    push(&mut events, "Guarded", json!({"repeat": "r1", "iteration": 0, "holds": true}));
    let i = inputs(&events, "b");
    push(&mut events, "Completed", json!({"node": "b", "iteration": {}, "attempt": 1, "inputs": i}));
    let i = inputs(&events, "d");
    push(&mut events, "Completed", json!({"node": "d", "iteration": {}, "attempt": 1, "inputs": i}));
    let s = fold_status(&log_of(&events), &started, true, t0());
    eprintln!("before: r1 {:?} b {:?} d {:?}", s.repeats["r1"], s.nodes["b"].state, s.nodes["d"].state);
    push(&mut events, "Completed", json!({"node": "u", "iteration": {}, "attempt": 1, "verify": obs(1)}));
    let c = fold_status(&log_of(&events), &started, true, t0());
    let r = fold_status(&log_of(&events), &started, false, t0());
    eprintln!(
        "cached: r1 {:?} a1 {:?} b {:?} d {:?}",
        c.repeats["r1"], c.nodes["a1"].state, c.nodes["b"].state, c.nodes["d"].state
    );
    eprintln!(
        "ref:    r1 {:?} a1 {:?} b {:?} d {:?}",
        r.repeats["r1"], r.nodes["a1"].state, r.nodes["b"].state, r.nodes["d"].state
    );
    assert_eq!(serde_json::to_string(&c).unwrap(), serde_json::to_string(&r).unwrap());
}
