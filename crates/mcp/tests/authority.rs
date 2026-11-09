//! No owner verb is reachable through MCP (BRO-3267 acceptance; the agent CLI half is
//! crates/cli/tests/requests.rs). Every tool is enumerated: a read, or an agent request whose
//! kind is not an owner verb. A new tool that maps to an `OwnerVerb`, or that claims owner
//! authority, fails here.

use maestro_core::authority::{Authority, OwnerVerb, Principal};
use maestro_core::request::{Disposition, RequestKind};
use maestro_mcp::{MaestroServer, TOOLS, call};
use maestro_store::{Paths, Store, TreeSource};
use serde_json::json;
use std::process::Command;

struct HeldJournalLock(Option<maestro_store::locks::Lock>);

impl Drop for HeldJournalLock {
    fn drop(&mut self) {
        if let Some(lock) = self.0.take() {
            maestro_store::locks::release(lock);
        }
    }
}

fn verb_name(verb: OwnerVerb) -> String {
    serde_json::to_value(verb).unwrap().as_str().unwrap().to_string()
}

#[test]
fn every_tool_is_a_read_or_an_agent_request() {
    for spec in TOOLS {
        match spec.authority {
            Authority::Read => assert!(spec.request.is_none() && spec.scope == "read", "{}", spec.name),
            Authority::AgentRequest => {
                assert!(spec.request.is_some(), "{} makes no request", spec.name);
                assert!(spec.scope != "read" && !spec.scope.is_empty(), "{} names no scope", spec.name);
            }
            Authority::OwnerAction => panic!("{} takes an owner action", spec.name),
            // A flow write takes effect, so it is no held request, and it may write only the
            // flow ledger (docs/adrs/2026-11-06-flow-ledger.md).
            Authority::AgentWrite => {
                assert!(spec.request.is_none(), "{} is a flow write but makes a request", spec.name);
                assert!(within_flow_ledger(spec.scope), "{} writes outside the flow ledger", spec.name);
            }
        }
    }
    assert_eq!(MaestroServer::tools().len(), TOOLS.len());
    // The flow verbs, explicitly (BRO-3634): the five writes are agent writes into the flow
    // ledger and make no request; next and status are reads; nothing else is an agent write.
    let writes = ["maestro_flow_start", "maestro_flow_claim", "maestro_flow_complete", "maestro_flow_choose"];
    // The sixth, `ask` (BRO-3635), writes the flow ledger too, and its one other effect, an ask
    // filed through the asks ledger's writer of record, is pinned by
    // `the_flow_ask_writes_its_instance_and_one_ask_through_the_writer_of_record` below.
    for name in writes.into_iter().chain(["maestro_flow_heartbeat", "maestro_flow_ask"]) {
        let spec = TOOLS.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("no tool {name}"));
        assert_eq!(
            (spec.authority, spec.request, spec.scope),
            (Authority::AgentWrite, None, ".broomva/flows"),
            "{name}"
        );
    }
    for name in ["maestro_flow_next", "maestro_flow_status"] {
        let spec = TOOLS.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("no tool {name}"));
        assert_eq!((spec.authority, spec.request, spec.scope), (Authority::Read, None, "read"), "{name}");
    }
    assert_eq!(TOOLS.iter().filter(|t| t.authority == Authority::AgentWrite).count(), 6);
}

/// The flow ledger's scope: its root `.broomva/flows` or a path below it, never a sibling
/// (`.broomva/flows-other`) and never a path that climbs out (`..`).
fn within_flow_ledger(scope: &str) -> bool {
    (scope == ".broomva/flows" || scope.starts_with(".broomva/flows/")) && !scope.split('/').any(|p| p == "..")
}

#[test]
fn a_flow_write_scope_is_the_flow_ledger_and_nothing_beside_it() {
    for ok in [".broomva/flows", ".broomva/flows/", ".broomva/flows/fi_01/events.jsonl"] {
        assert!(within_flow_ledger(ok), "{ok}");
    }
    for bad in [".broomva/flows-other", ".broomva/flowsx", ".broomva/flows/../asks", ".broomva/asks", "read", ""] {
        assert!(!within_flow_ledger(bad), "{bad}");
    }
}

#[test]
fn no_tool_maps_to_an_owner_verb() {
    let owner: Vec<String> = OwnerVerb::ALL.into_iter().map(verb_name).collect();
    for spec in TOOLS {
        let as_verb = spec.name.trim_start_matches("maestro_").replacen('_', ".", 1);
        assert!(!owner.contains(&as_verb), "{} is the owner verb {as_verb}", spec.name);
        if let Some(kind) = spec.request {
            assert!(
                !owner.iter().any(|v| v == kind.as_str()),
                "{} requests the owner verb {}",
                spec.name,
                kind.as_str()
            );
        }
        for word in [
            "answer", "approve", "allow", "deny", "dispatch", "verdict", "undo", "review", "decide", "resolve",
            "withdraw", "archive", "default",
        ] {
            assert!(!spec.name.split('_').any(|w| w == word), "{} reads as an owner verb", spec.name);
        }
    }
    // Every agent request has exactly one tool, and none of them applies anything for an
    // unverified caller but an ask.
    for kind in RequestKind::ALL {
        assert_eq!(TOOLS.iter().filter(|t| t.request == Some(kind)).count(), 1, "{}", kind.as_str());
        assert_ne!(kind.disposition(&Principal::Unverified), Disposition::Recorded, "{}", kind.as_str());
    }
}

#[test]
fn owner_verbs_are_unknown_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(Paths::under(tmp.path()), TreeSource::WorkingTree);
    for verb in OwnerVerb::ALL {
        let name = format!("maestro_{}", verb_name(verb).replace('.', "_"));
        let err = call(&store, &name, json!({})).unwrap_err();
        assert!(err.message.contains("unknown tool"), "{name}: {}", err.message);
    }
    // A request cannot smuggle an owner's answer into a ledger.
    let err = call(
        &store,
        "maestro_ask_file",
        json!({ "session": "t3:-/x", "ledger": "a.yaml", "ask": { "id": "x", "answer": "yes" } }),
    )
    .unwrap_err();
    assert!(err.message.contains("not the filer's"), "{}", err.message);
}

fn tree(root: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.insert(path.strip_prefix(root).unwrap().to_path_buf(), Vec::new());
                stack.push(path);
            } else {
                out.insert(path.strip_prefix(root).unwrap().to_path_buf(), std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

fn copy(from: &std::path::Path, to: &std::path::Path) {
    for (rel, bytes) in tree(from) {
        if from.join(&rel).is_dir() {
            std::fs::create_dir_all(to.join(&rel)).unwrap();
        } else {
            std::fs::create_dir_all(to.join(&rel).parent().unwrap()).unwrap();
            std::fs::write(to.join(&rel), bytes).unwrap();
        }
    }
}

/// What a read does, not what it declares: every tool declared a read leaves a whole machine
/// (ledgers, work, home, fleet state) byte for byte as it was.
#[test]
fn a_read_tool_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    copy(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine"), tmp.path());
    let mut paths = Paths::under(tmp.path());
    paths.ask_locks = tmp.path().join("locks");
    let store = Store::new(paths, TreeSource::WorkingTree);
    let before = tree(tmp.path());
    for spec in TOOLS.iter().filter(|t| t.authority == Authority::Read) {
        let args = match spec.name {
            "maestro_ask_get" => json!({ "ledger": "demo-arc.yaml", "id": "merge-now" }),
            "maestro_ui_render" => json!({ "root": "s", "blocks": { "s": { "type": "snapshot" } } }),
            "maestro_ui_poll" => json!({ "spec": { "root": "s", "blocks": { "s": { "type": "snapshot" } } } }),
            "maestro_flow_next" | "maestro_flow_status" => json!({ "instance": "fi_none" }),
            _ => json!({}),
        };
        call(&store, spec.name, args).unwrap_or_else(|e| panic!("{}: {}", spec.name, e.message));
        assert!(tree(tmp.path()) == before, "{} changed the machine", spec.name);
    }
}

/// What a held request does, not what it declares: it adds one journal line and changes nothing else.
#[test]
fn a_held_request_changes_nothing_but_the_journal() {
    let tmp = tempfile::tempdir().unwrap();
    copy(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine"), tmp.path());
    let mut paths = Paths::under(tmp.path());
    paths.ask_locks = tmp.path().join("locks");
    let store = Store::new(paths, TreeSource::WorkingTree);
    let journal = std::path::Path::new("broomva-home/ledger/requests/requests.jsonl");
    let outside = |t: std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>| {
        t.into_iter()
            .filter(|(p, _)| !journal.starts_with(p) && !p.starts_with("broomva-home/locks"))
            .collect::<Vec<_>>()
    };
    let before = outside(tree(tmp.path()));
    let mut lines = 0;
    for spec in TOOLS.iter().filter(|t| t.request.is_some_and(|k| k != RequestKind::AskFile)) {
        let args = json!({
            "session": "paseo:-/a1", "title": "t", "brief": "b", "action": "a", "reason": "r",
            "receipt": "r", "summary": "s",
        });
        let v = call(&store, spec.name, args).unwrap_or_else(|e| panic!("{}: {}", spec.name, e.message));
        assert_eq!(v["request"]["disposition"], "held", "{}", spec.name);
        lines += 1;
        assert!(outside(tree(tmp.path())) == before, "{} changed the machine", spec.name);
        let written = std::fs::read_to_string(tmp.path().join(journal)).unwrap();
        assert_eq!(written.lines().count(), lines, "{}", spec.name);
    }
    assert_eq!(lines, RequestKind::ALL.len() - 1);
    // A flow write is not a request: from an unverified caller it is refused, and it writes no
    // journal line and nothing else, flow ledger included (BRO-3634).
    let whole = tree(tmp.path());
    let session = "paseo:-/a1";
    for spec in TOOLS.iter().filter(|t| t.authority == Authority::AgentWrite) {
        let args = match spec.name {
            "maestro_flow_start" => json!({ "session": session, "definition": "flow: x" }),
            "maestro_flow_claim" => json!({ "session": session, "instance": "fi_x", "node_id": "n" }),
            "maestro_flow_choose" => json!({ "session": session, "claim_token": "fi_x.00", "edge": "e" }),
            "maestro_flow_ask" => json!({
                "session": session, "instance": "fi_x", "node_id": "n", "ledger": "demo-arc.yaml", "ask": { "class": "decision" },
            }),
            _ => json!({ "session": session, "claim_token": "fi_x.00" }),
        };
        let v = call(&store, spec.name, args).unwrap_or_else(|e| panic!("{}: {}", spec.name, e.message));
        assert_eq!((&v["ok"], &v["code"]), (&json!(false), &json!("caller_unverified")), "{}", spec.name);
        assert!(tree(tmp.path()) == whole, "{} changed the machine", spec.name);
    }
}

#[test]
fn guard_process_entry() {
    maestro_flow::kernel::guard_entry();
}

/// What a verified flow write does (BRO-3624 item 9): the first start makes the ledger (its root
/// and the lock beside it); after that each write verb changes only its instance's log and the
/// projection (a cache), never the requests journal or anything else on the machine.
#[test]
fn a_flow_write_changes_only_its_instance_and_the_flow_ledger() {
    use std::path::{Path, PathBuf};
    type Tree = std::collections::BTreeMap<PathBuf, Vec<u8>>;
    maestro_flow::kernel::guard_entry();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    copy(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine"), &root);
    let mut paths = Paths::under(&root);
    paths.ask_locks = root.join("locks");
    let store = Store::new(paths, TreeSource::WorkingTree);
    let work = root.join("flow-work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("ok"), "").unwrap();
    let direct = maestro_hosts::DirectRegistry::new();
    direct.register(maestro_hosts::DirectSession {
        id: "d1".into(),
        cwd: work.to_string_lossy().into_owned(),
        token: "tok-1".into(),
        liveness: maestro_hosts::SessionLiveness::Live,
        harness: None,
        pending_permissions: vec![],
    });
    let hosts = maestro_hosts::SurfaceRegistry::new(None, None, Some(direct));
    let call = |tool: &str, mut args: serde_json::Value| {
        args["session"] = json!("direct:-/d1");
        args["token"] = json!("tok-1");
        args["cwd"] = json!(work);
        let v = maestro_mcp::call_with_hosts(&store, &hosts, tool, args)
            .unwrap_or_else(|e| panic!("{tool}: {}", e.message));
        assert_eq!(v["ok"], true, "{tool}: {v}");
        v
    };
    let definition = r#"
flow: decide
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  review: {class: work, verify: [sh, -c, 'test -f ok']}
  pick: {class: choice, needs: [review], edges: {ship: {guard: '.nodes.review.outputs.verdict == "pass"'}}}
"#;
    let ledger = Path::new("workspace/.broomva/flows");
    // Every path a write added, changed or removed.
    let changed = |before: &Tree| {
        let after = tree(&root);
        let mut paths: Vec<PathBuf> = after
            .iter()
            .filter(|(p, b)| before.get(*p) != Some(b))
            .map(|(p, _)| p.clone())
            .chain(before.keys().filter(|p| !after.contains_key(*p)).cloned())
            .collect();
        paths.sort();
        paths
    };

    let before = tree(&root);
    let begun = call("maestro_flow_start", json!({ "definition": definition, "params": { "dir": work } }));
    let fi = begun["instance"].as_str().unwrap().to_string();
    let made = changed(&before);
    assert!(made.contains(&ledger.join("instances").join(&fi).join("events.jsonl")), "{made:?}");
    for p in made {
        assert!(p.starts_with(ledger) || p == Path::new("workspace/.broomva/flows.lock"), "start changed {p:?}");
    }
    let instance = ledger.join("instances").join(&fi);
    let mut token = String::new();
    let steps = [
        ("maestro_flow_claim", json!({ "instance": fi, "node_id": "review" })),
        ("maestro_flow_heartbeat", json!({})),
        ("maestro_flow_complete", json!({ "outputs": { "verdict": "pass" } })),
        ("maestro_flow_claim", json!({ "instance": fi, "node_id": "pick" })),
        ("maestro_flow_choose", json!({ "edge": "ship" })),
    ];
    for (tool, mut args) in steps {
        if tool != "maestro_flow_claim" {
            args["claim_token"] = json!(token);
        }
        let before = tree(&root);
        let v = call(tool, args);
        if let Some(t) = v["claim_token"].as_str() {
            token = t.to_string();
        }
        let changed = changed(&before);
        assert!(changed.contains(&instance.join("events.jsonl")), "{tool} wrote no event: {changed:?}");
        for p in &changed {
            let cache = p.parent() == Some(ledger)
                && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("projection.sqlite"));
            assert!(p.starts_with(&instance) || cache, "{tool} changed {p:?}");
        }
    }
    assert!(!root.join("broomva-home/ledger/requests/requests.jsonl").exists(), "a flow write journals nothing");
}

/// The flow verb that files a human node's ask (BRO-3635), explicitly: it changes its instance's
/// log, the projection, and one asks ledger, through the writer of record (`ask_ledger.py file`,
/// here the stub), and nothing else: no requests journal line, no other ledger. A read of the
/// instance afterwards still writes nothing, and relays the question with the ask's id.
#[test]
fn the_flow_ask_writes_its_instance_and_one_ask_through_the_writer_of_record() {
    use std::path::{Path, PathBuf};
    maestro_flow::kernel::guard_entry();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    copy(&fixtures.join("machine"), &root);
    std::fs::create_dir_all(root.join("workspace/scripts")).unwrap();
    std::fs::copy(fixtures.join("requests/ask_ledger_stub.py"), root.join("workspace/scripts/ask_ledger.py")).unwrap();
    let mut paths = Paths::under(&root);
    paths.ask_locks = root.join("locks");
    let store = Store::new(paths, TreeSource::WorkingTree);
    let direct = maestro_hosts::DirectRegistry::new();
    direct.register(maestro_hosts::DirectSession {
        id: "d1".into(),
        cwd: root.to_string_lossy().into_owned(),
        token: "tok-1".into(),
        liveness: maestro_hosts::SessionLiveness::Live,
        harness: None,
        pending_permissions: vec![],
    });
    let hosts = maestro_hosts::SurfaceRegistry::new(None, None, Some(direct));
    let call = |tool: &str, mut args: serde_json::Value| {
        args["session"] = json!("direct:-/d1");
        args["token"] = json!("tok-1");
        args["cwd"] = json!(root);
        maestro_mcp::call_with_hosts(&store, &hosts, tool, args).unwrap_or_else(|e| panic!("{tool}: {}", e.message))
    };
    let definition = "flow: gate\nversion: 1\nnodes:\n  owner: {class: human, ask: Merge the gate?}\n  ship: {class: pure, run: ['true'], needs: [owner]}\n";
    let begun = call("maestro_flow_start", json!({ "definition": definition }));
    let fi = begun["instance"].as_str().unwrap().to_string();
    let before = tree(&root);
    let ask = json!({
        "class": "decision", "gates": ["merge"], "blocking": false, "default": "hold", "preanswerable": false,
        "exhausted": ["read: a fixture", "default: a fixture"],
    });
    let v =
        call("maestro_flow_ask", json!({ "instance": fi, "node_id": "owner", "ledger": "demo-arc.yaml", "ask": ask }));
    assert_eq!((&v["ok"], &v["state"]), (&json!(true), &json!("waiting")), "{v}");
    let id = v["ask_id"].as_str().unwrap().to_string();
    let after = tree(&root);
    let changed: Vec<PathBuf> = after
        .iter()
        .filter(|(p, b)| before.get(*p) != Some(b))
        .map(|(p, _)| p.clone())
        .chain(before.keys().filter(|p| !after.contains_key(*p)).cloned())
        .collect();
    let ledger = Path::new("workspace/.broomva/flows");
    let asks = Path::new("workspace/.control/asks/demo-arc.yaml");
    assert!(changed.iter().any(|p| p == asks), "{changed:?}");
    for p in &changed {
        let flows = p.starts_with(ledger.join("instances").join(&fi))
            || (p.parent() == Some(ledger)
                && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("projection.sqlite")));
        assert!(flows || p == asks || p.starts_with("locks"), "ask changed {p:?}");
    }
    let text = std::fs::read_to_string(root.join(asks)).unwrap();
    assert_eq!(text.matches(&format!("id: \"{id}\"")).count(), 1, "one ask filed");
    assert!(!root.join("broomva-home/ledger/requests/requests.jsonl").exists(), "nothing journalled");
    // A read relays the waiting question with its ask, and writes nothing.
    let quiet = tree(&root);
    let next = maestro_mcp::call_with_hosts(&store, &hosts, "maestro_flow_next", json!({ "instance": fi })).unwrap();
    assert_eq!(next["relay"][0]["ask_id"], json!(id), "{next}");
    assert_eq!((&next["relay"][0]["question"], &next["relay"][0]["linked"]), (&json!("Merge the gate?"), &json!(true)));
    assert!(tree(&root) == quiet, "a read wrote");
}

/// An ask filed whose journal line failed on a busy lock is not a busy request: the ask is
/// written, and a retry would be refused as a duplicate, so it never says retry (exit 75).
#[test]
fn an_unjournalled_filing_is_never_busy() {
    use maestro_store::locks::LockError;
    use maestro_store::requests::FileError;
    let busy = FileError::Lock(LockError::Busy { holder: "pid 1".into(), what: "the requests journal".into() });
    let e = maestro_mcp::unjournalled(std::path::Path::new("/ws/.control/asks/l.yaml"), "a-1", &busy);
    let data = e.data.expect("data names the filed ask");
    assert_eq!(data["filed"]["id"], "a-1");
    assert!(data.get("busy").is_none_or(|b| b != &json!(true)), "{data}");
    assert!(e.message.contains("filed `a-1`") && !e.message.contains(".."), "{}", e.message);
    assert!(!e.message.to_lowercase().contains("try again"), "{}", e.message);
}

/// Run in a child test process so its request-wait override cannot affect parallel tests. This
/// goes through `call` and the real ask writer, holding the actual requests-journal lock.
#[test]
fn a_filed_ask_with_a_busy_journal_lock_has_no_busy_flag() {
    const CHILD: &str = "BRO3267_MCP_BUSY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "a_filed_ask_with_a_busy_journal_lock_has_no_busy_flag", "--nocapture"])
            .env(CHILD, "1")
            .env("MAESTRO_REQUESTS_WAIT_MS", "100")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        return;
    }

    use maestro_store::locks::{self, AcquireOptions};
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let asks = workspace.join(".control/asks");
    std::fs::create_dir_all(&asks).unwrap();
    std::fs::create_dir_all(workspace.join("scripts")).unwrap();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/requests");
    std::fs::copy(fixtures.join("ledger.yaml"), asks.join("ledger.yaml")).unwrap();
    std::fs::copy(fixtures.join("ask_ledger_stub.py"), workspace.join("scripts/ask_ledger.py")).unwrap();
    let mut paths = Paths::under(tmp.path());
    paths.ask_locks = tmp.path().join("ask-locks");
    let store = Store::new(paths.clone(), TreeSource::WorkingTree);
    let journal = paths.broomva_home.join("ledger/requests/requests.jsonl");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let _lock = HeldJournalLock(Some(
        locks::acquire(
            &journal,
            &paths.broomva_home.join("locks"),
            &AcquireOptions {
                wait: std::time::Duration::from_secs(1),
                what: "the test journal".into(),
                ..Default::default()
            },
        )
        .unwrap(),
    ));
    let mut ask: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixtures.join("ask.json")).unwrap()).unwrap();
    ask["id"] = json!("mcp-busy-journal-1");
    let error =
        call(&store, "maestro_ask_file", json!({ "session": "t3:local/test", "ledger": "ledger.yaml", "ask": ask }))
            .unwrap_err();
    assert!(
        error.message.contains("filed `mcp-busy-journal-1`") && error.message.contains("journal line was not written"),
        "{}",
        error.message
    );
    let data = error.data.expect("the filed ask is named in the error data");
    assert_eq!(data["filed"]["id"], "mcp-busy-journal-1");
    assert_ne!(data.get("busy"), Some(&json!(true)), "{data}");
    assert!(std::fs::read_to_string(asks.join("ledger.yaml")).unwrap().contains("id: \"mcp-busy-journal-1\""));
    assert!(!journal.exists(), "the journal line must be absent");
}
