//! The fixture the claim tests share (BRO-3633 PR 3b2a): a ledger, a compiled flow and its
//! workdir, and the owner's session. Two test binaries include this file: the kernel's
//! integration tests (`claims`), and the crate's own unit tests (`src/kernel/clocked.rs`), the
//! only tests that set or shift the clock the writers read (see `clock`'s module docs).
#![allow(dead_code)]

pub use super::common::*;
use jiff::{SignedDuration, Timestamp};
use maestro_core::session::VerifiedSession;
use maestro_flow::kernel::{
    Claim, CompiledFlow, Completion, KernelError, Limits, Status, claim, compile, complete, start, status,
};
use maestro_flow::{InstanceId, Ledger, NewEvent, Options, Reader};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

/// head (observe) -> build (pure) -> review (work); a choice and a human node beside them.
pub const CHAIN: &str = r#"
flow: chain
version: 1
params: [dir, big?]
workdir: "{{params.dir}}"
nodes:
  head: {class: observe, run: [sh, -c, 'touch observed; sleep "$(cat nap 2>/dev/null || echo 0)"; exit "$(cat head 2>/dev/null || echo 0)"'], fresh_for: 1m, exit_map: {0: up, 1: down}}
  build: {class: pure, needs: [head], run: [sh, -c, 'touch built; sleep "$(cat nap 2>/dev/null || echo 0)"']}
  review: {class: work, needs: [build], verify: [sh, -c, 'test -f ok']}
  pick: {class: choice, edges: {a: {guard: "true"}}}
  owner: {class: human, ask: budget}
"#;

/// build (work) -> push (effect): the effect appends a line to `effects` and exits with the
/// content of `fail` (0 without it); its reconcile confirms a line is there unless `unconfirmed`
/// exists. Its precondition reads the build's outputs.
pub const EFFECT: &str = r#"
flow: effect
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  build: {class: work, verify: ['true']}
  push: {class: effect, needs: [build], preconditions: ['.nodes.build.outputs.ready == true'], run: [sh, -c, 'echo ran >> effects; exit "$(cat fail 2>/dev/null || echo 0)"'], reconcile: [sh, -c, 'test -s effects && test ! -f unconfirmed']}
"#;

/// How many times the effect ran.
pub fn runs(f: &Fx) -> usize {
    std::fs::read_to_string(f.dir.join("effects")).map_or(0, |t| t.lines().count())
}

pub struct Fx {
    pub _tmp: tempfile::TempDir,
    pub root: PathBuf,
    /// The flow's workdir.
    pub dir: PathBuf,
    pub ledger: Ledger,
    pub flow: CompiledFlow,
    pub me: VerifiedSession,
}

pub fn fx(yaml: &str) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let dir = tmp.path().join("work");
    std::fs::create_dir(&dir).unwrap();
    let ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    let flow = compile(yaml).unwrap_or_else(|e| panic!("{e}"));
    Fx { _tmp: tmp, root, dir, ledger, flow, me: agent("owner", "marker-1") }
}

pub fn code<T>(r: Result<T, KernelError>) -> String {
    r.map_or_else(|e| e.code().to_string(), |_| "ok".to_string())
}

pub fn secs(s: i64) -> SignedDuration {
    SignedDuration::from_secs(s)
}

pub fn ms(n: i64) -> SignedDuration {
    SignedDuration::from_millis(n)
}

pub fn quick() -> Limits {
    Limits { timeout: Duration::from_secs(5), max_output: 64 << 10 }
}

/// The refusal code of a completion, or `completed`.
pub fn outcome(c: &Completion) -> &str {
    match c {
        Completion::Completed { .. } => "completed",
        Completion::Refused { code, .. } => code,
    }
}

impl Fx {
    pub fn begin(&mut self) -> InstanceId {
        let params = json!({ "dir": self.dir.to_str().unwrap() });
        start(&mut self.ledger, &self.me, &self.flow, params).unwrap().instance
    }

    pub fn claim(&mut self, fi: &InstanceId, node: &str) -> Claim {
        claim(&mut self.ledger, &self.me, fi, node).unwrap_or_else(|e| panic!("claim {node}: {e}"))
    }

    pub fn complete(&mut self, token: &str, outputs: Value) -> Completion {
        complete(&mut self.ledger, &self.me, token, outputs, &quick()).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Claims and completes `node` now; the receipt's stamp.
    pub fn done(&mut self, fi: &InstanceId, node: &str) -> Timestamp {
        let c = self.claim(fi, node);
        match self.complete(&c.token, Value::Null) {
            Completion::Completed { appended, .. } => appended.event.at.parse().unwrap(),
            other => panic!("{node}: {other:?}"),
        }
    }

    pub fn status(&self, fi: &InstanceId, now: Timestamp) -> Status {
        let reader = Reader::new(&self.root, Options::default()).unwrap();
        status(&reader.lock().unwrap(), fi, now).unwrap()
    }

    pub fn log_text(&self, fi: &InstanceId) -> String {
        std::fs::read_to_string(self.root.join("instances").join(fi.as_str()).join("events.jsonl")).unwrap()
    }

    pub fn kinds(&self, fi: &InstanceId) -> Vec<String> {
        self.log_text(fi)
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["kind"].as_str().unwrap().to_string())
            .collect()
    }

    pub fn last_body(&self, fi: &InstanceId) -> Value {
        let text = self.log_text(fi);
        serde_json::from_str::<Value>(text.lines().last().unwrap()).unwrap()["body"].clone()
    }

    /// Appends `ev` at the head through the storage API, as a fixture.
    pub fn append(&mut self, fi: &InstanceId, ev: NewEvent) {
        let mut guard = self.ledger.lock().unwrap();
        let seq = guard.log(fi).unwrap().seq();
        guard.append(fi, seq, ev).unwrap();
    }
}
