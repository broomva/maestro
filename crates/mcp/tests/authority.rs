//! No owner verb is reachable through MCP (BRO-3267 acceptance; the agent CLI half is
//! crates/cli/tests/requests.rs). Every tool is enumerated: a read, or an agent request whose
//! kind is not an owner verb. A new tool that maps to an `OwnerVerb`, or that claims owner
//! authority, fails here.

use maestro_core::authority::{Authority, OwnerVerb, Principal};
use maestro_core::request::{Disposition, RequestKind};
use maestro_mcp::{MaestroServer, TOOLS, call};
use maestro_store::{Paths, Store, TreeSource};
use serde_json::json;

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
        }
    }
    assert_eq!(MaestroServer::tools().len(), TOOLS.len());
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
}
