//! The Stop-hook gate (BRO-3636; flow kernel spec, "L4: gates"): Claude Code's and Codex's block
//! format, the loop bound, fail-open on each error class, `StopUnblockable` and the fleet alert,
//! `maestro_flow_open`, and the install check. Each guard is mutation-checked in
//! `scripts/mutation-check.sh`. The kernel half (the predicate itself) is `maestro_flow`'s
//! `kernel::stopped`.

use maestro_hosts::{DirectRegistry, DirectSession, SessionLiveness, SurfaceRegistry};
use maestro_mcp::flow::{CLAIM, COMPLETE, NEXT, OPEN, START, STATUS};
use maestro_mcp::stop::{self, Gate, Harness, Outcome};
use maestro_mcp::{Ambient, call_with_hosts};
use maestro_store::{Paths, Store, TreeSource};
use serde_json::{Value, json};
use std::path::PathBuf;

#[test]
fn guard_process_entry() {
    maestro_flow::kernel::guard_entry();
}

/// `one`, then `two`, in the work folder.
const RELAY: &str = r#"
flow: relay
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  one: {class: work, verify: [sh, -c, 'test -f ok']}
  two: {class: pure, run: ['true'], needs: [one]}
"#;

/// A scratch machine: a workspace with `.broomva/` and an asks ledger whose writer of record is
/// the stub, a work folder, and a Direct registry with two live sessions there.
struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    paths: Paths,
    store: Store,
    work: PathBuf,
    hosts: SurfaceRegistry,
}

fn fx() -> Fx {
    maestro_flow::kernel::guard_entry();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let ws = root.join("workspace");
    std::fs::create_dir_all(ws.join(".broomva/asks")).unwrap();
    std::fs::create_dir_all(ws.join("scripts")).unwrap();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/requests");
    std::fs::copy(fixtures.join("ledger.yaml"), ws.join(".broomva/asks/ledger.yaml")).unwrap();
    std::fs::copy(fixtures.join("ask_ledger_stub.py"), ws.join("scripts/ask_ledger.py")).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let direct = DirectRegistry::new();
    for (id, token) in [("d1", "tok-1"), ("d2", "tok-2")] {
        direct.register(DirectSession {
            id: id.into(),
            cwd: work.to_string_lossy().into_owned(),
            token: token.into(),
            liveness: SessionLiveness::Live,
            harness: None,
            pending_permissions: vec![],
        });
    }
    let mut paths = Paths::under(&root);
    paths.ask_locks = root.join("ask-locks");
    let store = Store::new(paths.clone(), TreeSource::WorkingTree);
    Fx { _tmp: tmp, root, paths, store, work, hosts: SurfaceRegistry::new(None, None, Some(direct)) }
}

fn claude_stop() -> Vec<u8> {
    json!({
        "session_id": "abc123", "transcript_path": "/tmp/t.jsonl", "cwd": "/tmp", "permission_mode": "default",
        "hook_event_name": "Stop", "stop_hook_active": false, "last_assistant_message": "Done.",
    })
    .to_string()
    .into_bytes()
}

/// Codex 0.164's `stop.command.input`, every required field.
fn codex_stop() -> Vec<u8> {
    json!({
        "cwd": "/tmp", "hook_event_name": "Stop", "last_assistant_message": "Done.", "model": "gpt",
        "permission_mode": "bypassPermissions", "session_id": "01a1", "stop_hook_active": false,
        "transcript_path": null, "turn_id": "01a2",
    })
    .to_string()
    .into_bytes()
}

impl Fx {
    fn call(&self, tool: &str, args: Value) -> Value {
        call_with_hosts(&self.store, &self.hosts, tool, args).unwrap_or_else(|e| panic!("{tool}: {}", e.message))
    }

    fn claim_of(&self, id: &str) -> Value {
        let token = if id == "d1" { "tok-1" } else { "tok-2" };
        json!({ "token": token, "cwd": self.work })
    }

    fn as_(&self, id: &str, mut args: Value) -> Value {
        args["session"] = json!(format!("direct:-/{id}"));
        for (k, v) in self.claim_of(id).as_object().unwrap() {
            args[k] = v.clone();
        }
        args
    }

    fn begin(&self, id: &str) -> String {
        let v = self.call(START, self.as_(id, json!({ "definition": RELAY, "params": { "dir": self.work } })));
        assert_eq!(v["ok"], true, "{v}");
        v["instance"].as_str().unwrap().to_string()
    }

    fn gate_as(&self, id: &str, harness: Harness, input: &[u8]) -> Outcome {
        self.gate_with(id, harness, false, input)
    }

    fn gate_with(&self, id: &str, harness: Harness, cannot_block: bool, input: &[u8]) -> Outcome {
        let gate =
            Gate { harness, cannot_block, session: Some(format!("direct:-/{id}")), alert_ledger: "ledger.yaml".into() };
        stop::gate(&self.paths, &self.hosts, Ambient::Shared, &self.claim_of(id), &gate, input)
    }

    fn events(&self, fi: &str) -> Vec<Value> {
        let log = self.root.join("workspace/.broomva/flows/instances").join(fi).join("events.jsonl");
        std::fs::read_to_string(log).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn ledger(&self) -> String {
        std::fs::read_to_string(self.root.join("workspace/.broomva/asks/ledger.yaml")).unwrap()
    }
}

fn kinds(events: &[Value]) -> Vec<String> {
    events.iter().map(|e| e["kind"].as_str().unwrap().to_string()).collect()
}

/// The refusal a harness reads: exactly `decision` and `reason`.
fn refusal(out: &Outcome) -> String {
    assert!(out.blocked, "{out:?}");
    let v: Value = serde_json::from_str(&out.stdout).unwrap();
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["decision", "reason"], "{v}");
    assert_eq!(v["decision"], "block");
    v["reason"].as_str().unwrap().to_string()
}

fn allowed(out: &Outcome) {
    assert!(!out.blocked, "{out:?}");
    assert_eq!(out.stdout, "{}");
}

#[test]
fn a_claude_stop_over_work_is_refused_with_the_next_step() {
    let f = fx();
    let fi = f.begin("d1");
    let reason = refusal(&f.gate_as("d1", Harness::Claude, &claude_stop()));
    assert!(reason.contains(&format!("Instance {fi} (relay@1)")), "{reason}");
    assert!(
        reason.contains(&format!(
            "node one is enabled: claim it with `maestro flow claim --instance {fi} --node-id one`"
        )),
        "{reason}"
    );
}

#[test]
fn a_codex_stop_over_work_is_refused_in_codexs_format() {
    let f = fx();
    let fi = f.begin("d1");
    let reason = refusal(&f.gate_as("d1", Harness::Codex, &codex_stop()));
    assert!(reason.contains(&fi), "{reason}");
}

#[test]
fn a_stop_with_no_work_of_its_own_goes_through() {
    let f = fx();
    // Another session's instance is not this one's to finish.
    let theirs = f.begin("d2");
    allowed(&f.gate_as("d1", Harness::Claude, &claude_stop()));
    // A finished instance holds nothing either.
    std::fs::write(f.work.join("ok"), "").unwrap();
    for node in ["one", "two"] {
        let c = f.call(CLAIM, f.as_("d2", json!({ "instance": theirs, "node_id": node })));
        let done = f.call(COMPLETE, f.as_("d2", json!({ "claim_token": c["claim_token"] })));
        assert_eq!(done["ok"], true, "{done}");
    }
    allowed(&f.gate_as("d2", Harness::Claude, &claude_stop()));
}

#[test]
fn the_fourth_stop_at_one_position_goes_through_recorded_and_alerted() {
    let f = fx();
    let fi = f.begin("d1");
    for n in 1..=3 {
        assert!(f.gate_as("d1", Harness::Claude, &claude_stop()).blocked, "refusal {n}");
    }
    let out = f.gate_as("d1", Harness::Claude, &claude_stop());
    allowed(&out);
    let events = f.events(&fi);
    assert_eq!(kinds(&events), ["Started", "StopUnblockable"]);
    let body = &events[1]["body"];
    assert_eq!(
        (&body["reason"], &body["harness"], &body["open_instances"]),
        (&json!("no_progress"), &json!("claude"), &json!([fi]))
    );
    assert_eq!(out.recorded, [(fi.clone(), 2)]);
    // The alert is an ask in the owner's ledger, filed once.
    assert_eq!(out.alerts.len(), 1, "{out:?}");
    let id = &out.alerts[0];
    assert!(id.starts_with("stop-gate-"), "{id}");
    let ledger = f.ledger();
    assert!(ledger.contains(&format!("id: \"{id}\"")), "{ledger}");
    assert!(ledger.contains(stop::LEFT_WORK), "{ledger}");
    // `status` reports it.
    let status = f.call(STATUS, json!({ "instance": fi }));
    assert_eq!(status["status"]["stop_unblockable"].as_array().unwrap().len(), 1, "{status}");
    // The record moved the instance, so the count starts again: refused anew.
    assert!(f.gate_as("d1", Harness::Claude, &claude_stop()).blocked);
}

#[test]
fn progress_starts_the_count_again() {
    let f = fx();
    let fi = f.begin("d1");
    for _ in 0..3 {
        assert!(f.gate_as("d1", Harness::Claude, &claude_stop()).blocked);
    }
    // A claim moves the instance: the next stops are refused again, three times.
    f.call(CLAIM, f.as_("d1", json!({ "instance": fi, "node_id": "one" })));
    for n in 1..=3 {
        let reason = refusal(&f.gate_as("d1", Harness::Claude, &claude_stop()));
        assert!(reason.contains("you hold a live claim on node one"), "{n}: {reason}");
    }
    allowed(&f.gate_as("d1", Harness::Claude, &claude_stop()));
}

#[test]
fn a_harness_that_cannot_block_records_once_and_alerts_once() {
    let f = fx();
    let fi = f.begin("d1");
    let out = f.gate_with("d1", Harness::Codex, true, &codex_stop());
    allowed(&out);
    assert_eq!(kinds(&f.events(&fi)), ["Started", "StopUnblockable"]);
    assert_eq!(f.events(&fi)[1]["body"]["reason"], "harness_cannot_block");
    assert_eq!(out.alerts.len(), 1);
    // Nothing moved since: no second record, no second alert.
    let again = f.gate_with("d1", Harness::Codex, true, &codex_stop());
    allowed(&again);
    assert!(again.recorded.is_empty() && again.alerts.is_empty(), "{again:?}");
    assert_eq!(f.events(&fi).len(), 2);
}

/// Each error class lets the stop through, says why, and files the alert when it can.
#[test]
fn the_gate_fails_open() {
    // Standard input that is not a Stop hook's.
    let f = fx();
    f.begin("d1");
    for input in [&b"not json"[..], br#"{"hook_event_name": "PreToolUse"}"#, b"[]"] {
        let out = f.gate_as("d1", Harness::Claude, input);
        allowed(&out);
        assert!(out.notes.iter().any(|n| n.contains("input_malformed") && n.contains("fails open")), "{out:?}");
    }
    let out = f.gate_as("d1", Harness::Claude, b"{}");
    assert_eq!(out.alerts.len(), 1, "the alert is filed once a day: {out:?}");
    assert!(out.notes.iter().any(|n| n.contains("was filed before")), "{out:?}");
    assert!(f.ledger().contains(stop::UNCHECKED));

    // A session that does not verify.
    let gate = Gate {
        harness: Harness::Claude,
        cannot_block: false,
        session: Some("direct:-/d1".into()),
        alert_ledger: "ledger.yaml".into(),
    };
    let wrong = json!({ "token": "tok-2", "cwd": f.work });
    let out = stop::gate(&f.paths, &f.hosts, Ambient::Shared, &wrong, &gate, &claude_stop());
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("caller_refused")), "{out:?}");

    // An instance whose log does not read.
    let f = fx();
    let fi = f.begin("d2");
    let log = f.root.join("workspace/.broomva/flows/instances").join(&fi).join("events.jsonl");
    let mut text = std::fs::read_to_string(&log).unwrap();
    text.push_str("{\"torn\n");
    std::fs::write(&log, text).unwrap();
    let out = f.gate_as("d1", Harness::Claude, &claude_stop());
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("instances_unread")), "{out:?}");

    // A ledger that does not open.
    let f = fx();
    f.begin("d1");
    std::fs::remove_file(f.root.join("workspace/.broomva/flows/EPOCH")).unwrap();
    let out = f.gate_as("d1", Harness::Claude, &claude_stop());
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("fails open")), "{out:?}");

    // Refusal counts that do not read: the session's count file is a folder.
    let f = fx();
    f.begin("d1");
    let counts = f.root.join("workspace/.broomva/flows.stop-gate");
    assert!(f.gate_as("d1", Harness::Claude, &claude_stop()).blocked);
    let file = std::fs::read_dir(&counts).unwrap().next().unwrap().unwrap().path();
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    let out = f.gate_as("d1", Harness::Claude, &claude_stop());
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("refusals_unread")), "{out:?}");

    // Refusal counts that cannot be written: their folder takes no new file.
    let f = fx();
    f.begin("d1");
    let counts = f.root.join("workspace/.broomva/flows.stop-gate");
    std::fs::create_dir(&counts).unwrap();
    std::fs::set_permissions(&counts, std::os::unix::fs::PermissionsExt::from_mode(0o500)).unwrap();
    let out = f.gate_as("d1", Harness::Claude, &claude_stop());
    std::fs::set_permissions(&counts, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("refusals_unwritten")), "{out:?}");
}

#[test]
fn stdin_past_its_bound_is_not_read_as_a_stop() {
    let f = fx();
    f.begin("d1");
    let mut input = br#"{"hook_event_name": "Stop"}"#.to_vec();
    assert!(f.gate_as("d1", Harness::Claude, &input).blocked);
    input.resize(stop::MAX_INPUT + 1, b' ');
    let out = f.gate_as("d1", Harness::Claude, &input);
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("input_malformed")), "{out:?}");
}

#[test]
fn no_session_and_no_ledger_are_no_flow_at_all() {
    let f = fx();
    let gate =
        Gate { harness: Harness::Claude, cannot_block: false, session: None, alert_ledger: "ledger.yaml".into() };
    let out = stop::gate(&f.paths, &f.hosts, Ambient::Shared, &json!({}), &gate, &claude_stop());
    allowed(&out);
    assert!(out.alerts.is_empty());
    let out = f.gate_as("d1", Harness::Claude, &claude_stop());
    allowed(&out);
    assert!(out.alerts.is_empty() && out.notes.iter().any(|n| n.contains("no flow ledger")), "{out:?}");
}

#[test]
fn open_and_next_carry_the_gates_own_judgment() {
    let f = fx();
    let fi = f.begin("d1");
    let open = f.call(OPEN, f.as_("d1", json!({})));
    assert_eq!((&open["ok"], &open["refuses_stop"]), (&json!(true), &json!(true)), "{open}");
    let listed = &open["open"]["instances"][0];
    assert_eq!(listed["instance"], json!(fi));
    let next = f.call(NEXT, json!({ "instance": fi }));
    assert_eq!(next["work"], listed["work"], "{next}");
    assert_eq!(next["waiting"], listed["waiting"]);
    assert_eq!(next["work"][0], json!({ "kind": "node", "node": "one", "iteration": {}, "held": false }));
    let status = f.call(STATUS, json!({ "instance": fi }));
    assert_eq!(status["work"], listed["work"]);
    // Another session owns none of it; an unverified one is refused.
    let theirs = f.call(OPEN, f.as_("d2", json!({})));
    assert_eq!((&theirs["refuses_stop"], &theirs["open"]["instances"]), (&json!(false), &json!([])));
    let unverified = f.call(OPEN, json!({ "session": "t3:-/nobody" }));
    assert_eq!(unverified["code"], "caller_unverified", "{unverified}");
}

#[test]
fn the_install_check_reads_the_harness_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let hook = |command: &str| json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": command, "timeout": 60 }] }] } });
    let path = tmp.path().join("settings.json");
    let write = |v: Value| std::fs::write(&path, v.to_string()).unwrap();

    write(hook("/usr/local/bin/maestro flow stop-gate --harness claude"));
    let i = stop::check_install(Harness::Claude, &path);
    assert!(i.wired, "{i:?}");
    assert_eq!(i.commands, ["/usr/local/bin/maestro flow stop-gate --harness claude"]);
    // Wired for Claude is not wired for Codex.
    assert!(!stop::check_install(Harness::Codex, &path).wired);
    write(hook("maestro flow stop-gate --harness=codex"));
    let i = stop::check_install(Harness::Codex, &path);
    assert!(i.wired && i.detail.contains("trusted"), "{i:?}");
    for not in [
        "maestro flow next --harness claude",
        "echo maestro flow stop-gate --harness claude",
        "maestro flow stop-gate --check-install --harness claude",
    ] {
        write(hook(not));
        assert!(!stop::check_install(Harness::Claude, &path).wired, "{not}");
    }
    write(
        json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "prompt", "command": "maestro flow stop-gate --harness claude" }] }] } }),
    );
    assert!(!stop::check_install(Harness::Claude, &path).wired);
    assert!(!stop::check_install(Harness::Claude, &tmp.path().join("none.json")).wired);
}

#[test]
fn an_alert_is_an_ask_the_validator_of_record_takes() {
    let ask = stop::alert_ask("stop-gate-0123456789ab", stop::LEFT_WORK, "words");
    for headline in [stop::LEFT_WORK, stop::UNCHECKED] {
        assert!(headline.ends_with('?') && headline.chars().count() <= 100, "{headline}");
    }
    let options = ask["options"].as_array().unwrap();
    assert_eq!(options.iter().filter(|o| o["recommended"] == true).count(), 1);
    for rung in ask["exhausted"].as_array().unwrap() {
        let label = rung.as_str().unwrap().split(':').next().unwrap();
        assert!(["read", "preauth", "delegate", "reroute", "loop", "default"].contains(&label), "{rung}");
    }
    for key in [
        "id",
        "ask",
        "class",
        "gates",
        "exhausted",
        "blocking",
        "preanswerable",
        "headline",
        "stakes",
        "why_you",
        "default",
    ] {
        assert!(ask.contains_key(key), "{key}");
    }
    assert_eq!(stop::alert_id("x"), stop::alert_id("x"));
    assert_ne!(stop::alert_id("x"), stop::alert_id("y"));
}
