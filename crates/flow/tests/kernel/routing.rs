//! BRO-3828: `routing.provider: different_from(...)` is enforced, against a performer the owner
//! names and the registry attests (here a `VerifiedSession` built as the registry would), never
//! against a provider the performer reports about itself.

use crate::claiming::*;
use maestro_core::session::{Harness, VerifiedSession};
use maestro_flow::kernel::{Choosing, PROVIDERS, choose, claim_as, compile, complete_as, ir, provider_of, start};
use serde_json::{Value, json};

/// write -> review_a -> review_b -> gate. Each review writes its verify's stdin to a file.
const ROUTED: &str = r#"
flow: routed
version: 1
params: [dir, writer]
workdir: "{{params.dir}}"
receipt_stdin: true
nodes:
  write: {class: work, verify: ['true']}
  review_a: {class: work, needs: [write], routing: {provider: "different_from(params.writer)", slot: a}, verify: [sh, -c, 'cat > env_a']}
  review_b: {class: work, needs: [write, review_a], routing: {provider: "different_from(params.writer, review_a)"}, verify: [sh, -c, 'cat > env_b']}
  gate: {class: choice, needs: [review_b], edges: {go: {guard: '.nodes.review_b.performer.harness == "gemini"'}}}
"#;

/// A session the registry attested as the owner's child, running `harness`.
fn child(name: &str, harness: Harness) -> VerifiedSession {
    VerifiedSession::new(format!("t3:-/{name}").parse().unwrap(), "/tmp", Some(harness), Some("owner".into()), vec![])
}

fn routed(writer: &str) -> (Fx, maestro_flow::InstanceId) {
    let mut f = fx(ROUTED);
    let params = json!({ "dir": f.dir.to_str().unwrap(), "writer": writer });
    let fi = start(&mut f.ledger, &f.me, &f.flow, params).unwrap().instance;
    f.done(&fi, "write");
    (f, fi)
}

/// Claims `node`, naming no performer: the token.
fn take(f: &mut Fx, fi: &maestro_flow::InstanceId, node: &str) -> String {
    claim_as(&mut f.ledger, &f.me, fi, node, None).unwrap_or_else(|e| panic!("claim {node}: {e}")).token
}

/// Completes the claim `token`, naming `who`: the outcome. A refusal keeps the token.
fn finish(f: &mut Fx, token: &str, who: Option<&VerifiedSession>) -> String {
    outcome(&complete_as(&mut f.ledger, &f.me, token, Value::Null, &quick(), who).unwrap()).to_string()
}

/// Claims and completes `node`, naming `who` at complete: the outcome.
fn perform(f: &mut Fx, fi: &maestro_flow::InstanceId, node: &str, who: Option<&VerifiedSession>) -> String {
    let token = take(f, fi, node);
    finish(f, &token, who)
}

fn compiles(provider: &str, extra: &str) -> String {
    let yaml = format!(
        "flow: f\nversion: 1\nparams: [writer]\nnodes:\n  a: {{class: work, verify: ['true']}}\n  p: {{class: pure, run: ['true']}}\n{extra}  r: {{class: work, needs: [a, p], routing: {{provider: \"{provider}\"}}, verify: ['true']}}\n"
    );
    compile(&yaml).map_or_else(|e| e.to_string(), |_| "ok".to_string())
}

#[test]
fn different_from_names_only_declared_params_and_needed_work_nodes() {
    assert_eq!(compiles("different_from(params.writer, a)", ""), "ok");
    for (provider, why) in [
        ("different_from(writer)", "a bare word that is no node"),
        ("different_from(params.nope)", "an undeclared param"),
        ("different_from(p)", "a node that is not a work node"),
        ("different_from(c)", "a work node it does not need"),
        ("different_from()", "nothing"),
        ("different_from(a, a)", "a repeat"),
        ("different_from(params.writer", "no closing parenthesis"),
        ("different_from(a b)", "an argument that is not a word"),
    ] {
        let extra = "  c: {class: work, verify: ['true']}\n";
        assert_ne!(compiles(provider, extra), "ok", "{provider}: {why} compiled");
    }
    let on_choice = "flow: f\nversion: 1\nparams: [writer]\nnodes:\n  c: {class: choice, routing: {provider: \"different_from(params.writer)\"}, edges: {x: {guard: 'true'}}}\n";
    assert!(compile(on_choice).is_err(), "a choice node routed by different_from compiled");
}

#[test]
fn only_different_from_makes_an_ir_version_9_and_a_hint_stays_a_hint() {
    assert_eq!(compile(ROUTED).unwrap().ir().ir_version, 9);
    let hint =
        compile("flow: f\nversion: 1\nnodes:\n  r: {class: work, routing: {provider: codex}, verify: ['true']}\n")
            .unwrap();
    assert!(hint.ir().ir_version < 9);
    assert!(hint.ir().nodes["r"].class.launch().unwrap().different_from.is_empty());
}

#[test]
fn an_older_pinned_ir_with_different_from_text_still_validates() {
    // An IR compiled before BRO-3828 kept the text as a hint, with no parsed field and its own
    // version: made here from a hint-only IR whose text is then set as such a log pins it.
    let hint = compile("flow: f\nversion: 1\nparams: [writer]\nnodes:\n  r: {class: work, routing: {provider: codex}, verify: ['true']}\n").unwrap();
    let mut old = serde_json::to_value(hint.ir()).unwrap();
    let text = old.to_string().replace("\"provider\":\"codex\"", "\"provider\":\"different_from(writer)\"");
    assert_ne!(text, old.to_string(), "the routing text was not found in the IR");
    old = serde_json::from_str(&text).unwrap();
    let parsed: ir::Ir = serde_json::from_value(old).unwrap_or_else(|e| panic!("{e}"));
    assert!(parsed.ir_version < 9);
    ir::validate(&parsed).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn a_performer_whose_provider_is_the_writers_is_refused_and_another_is_not() {
    let (mut f, fi) = routed("claude");
    let token = take(&mut f, &fi, "review_a");
    assert_eq!(finish(&mut f, &token, Some(&child("w", Harness::ClaudeCode))), "routing_violated");
    assert_eq!(finish(&mut f, &token, Some(&child("x", Harness::Codex))), "completed");
}

#[test]
fn a_performer_whose_provider_did_a_node_it_differs_from_is_refused() {
    let (mut f, fi) = routed("claude");
    assert_eq!(perform(&mut f, &fi, "review_a", Some(&child("x", Harness::Codex))), "completed");
    let token = take(&mut f, &fi, "review_b");
    assert_eq!(finish(&mut f, &token, Some(&child("y", Harness::Codex))), "routing_violated");
    assert_eq!(finish(&mut f, &token, Some(&child("z", Harness::Gemini))), "completed");
}

#[test]
fn a_performer_with_no_attested_provider_is_refused() {
    let (mut f, fi) = routed("claude");
    assert_eq!(provider_of("agy"), None);
    assert_eq!(perform(&mut f, &fi, "review_a", Some(&child("g", Harness::Agy))), "routing_violated");
}

#[test]
fn the_providers_are_attested_ones() {
    assert_eq!(provider_of("claude-code"), Some("claude"));
    assert_eq!(provider_of("codex"), Some("codex"));
    assert_eq!(provider_of("gemini"), Some("gemini"));
    assert_eq!(provider_of("claude"), None, "a provider name is not a harness");
    assert_eq!(PROVIDERS, ["claude", "codex", "gemini", "grok"]);
}

#[test]
fn a_param_that_names_no_provider_refuses_the_claim() {
    let (mut f, fi) = routed("gpt");
    let r = claim_as(&mut f.ledger, &f.me, &fi, "review_a", None);
    assert_eq!(code(r), "routing_violated");
}

#[test]
fn a_claim_naming_an_excluded_performer_is_refused_before_anything_is_written() {
    let (mut f, fi) = routed("claude");
    let before = f.kinds(&fi).len();
    let r = claim_as(&mut f.ledger, &f.me, &fi, "review_a", Some(&child("w", Harness::ClaudeCode)));
    assert_eq!(code(r), "routing_violated");
    assert_eq!(f.kinds(&fi).len(), before);
}

#[test]
fn the_performer_named_at_claim_does_the_work_when_complete_names_none() {
    let (mut f, fi) = routed("claude");
    let c = claim_as(&mut f.ledger, &f.me, &fi, "review_a", Some(&child("x", Harness::Gemini))).unwrap();
    let done = complete_as(&mut f.ledger, &f.me, &c.token, Value::Null, &quick(), None).unwrap();
    assert_eq!(outcome(&done), "completed");
    assert_eq!(f.last_body(&fi)["performer"]["harness"], "gemini");
    // The owner (codex) is not who did it: review_b may then be codex's.
    assert_eq!(perform(&mut f, &fi, "review_b", Some(&child("y", Harness::Codex))), "completed");
}

#[test]
fn at_version_9_the_verify_and_the_guards_see_the_performer() {
    let (mut f, fi) = routed("claude");
    assert_eq!(perform(&mut f, &fi, "review_a", Some(&child("x", Harness::Codex))), "completed");
    assert_eq!(perform(&mut f, &fi, "review_b", Some(&child("z", Harness::Gemini))), "completed");
    let env: Value = serde_json::from_slice(&std::fs::read(f.dir.join("env_b")).unwrap()).unwrap();
    assert_eq!(env["performer"]["provider"], "gemini");
    assert_eq!(env["inputs"]["nodes"]["review_a"]["performer"]["harness"], "codex");
    let c = claim_as(&mut f.ledger, &f.me, &fi, "gate", None).unwrap();
    match choose(&mut f.ledger, &f.me, &c.token, "go").unwrap() {
        Choosing::Chosen { .. } => {}
        other => panic!("the guard on review_b's performer did not hold: {other:?}"),
    }
}

#[test]
fn below_version_9_the_verify_sees_no_performer() {
    let yaml = "flow: f\nversion: 1\nparams: [dir]\nworkdir: \"{{params.dir}}\"\nreceipt_stdin: true\nnodes:\n  a: {class: work, verify: ['true']}\n  r: {class: work, needs: [a], verify: [sh, -c, 'cat > env']}\n";
    let mut f = fx(yaml);
    assert!(f.flow.ir().ir_version < 9);
    let fi = f.begin();
    f.done(&fi, "a");
    f.done(&fi, "r");
    let env: Value = serde_json::from_slice(&std::fs::read(f.dir.join("env")).unwrap()).unwrap();
    assert!(env.get("performer").is_none(), "{env}");
    assert!(env["inputs"]["nodes"]["a"].get("performer").is_none(), "{env}");
}

#[test]
fn a_node_without_different_from_takes_any_performer() {
    let mut f = fx(CHAIN);
    let fi = f.begin();
    f.done(&fi, "head");
    f.done(&fi, "build");
    std::fs::write(f.dir.join("ok"), "").unwrap();
    assert_eq!(perform(&mut f, &fi, "review", Some(&child("g", Harness::Agy))), "completed");
}

#[test]
fn the_completed_receipt_records_the_named_performer_not_the_owner() {
    let (mut f, fi) = routed("claude");
    assert_eq!(perform(&mut f, &fi, "review_a", Some(&child("x", Harness::Gemini))), "completed");
    let body = f.last_body(&fi);
    assert_eq!(body["performer"]["harness"], "gemini");
    assert_eq!(body["performer"]["session"], "t3:-/x");
}
