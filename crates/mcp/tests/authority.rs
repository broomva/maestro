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
        for word in ["answer", "approve", "allow", "deny", "dispatch", "verdict", "undo", "review", "decide"] {
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
