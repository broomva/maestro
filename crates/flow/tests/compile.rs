//! BRO-3633 PR 3b1: flow definitions and the compiler. Each rejection is its own test, and the
//! tables name the case that failed.

use maestro_flow::kernel::compile;
use maestro_flow::kernel::ir::{Class, Need};

const VERIFY: &str = include_str!("../flows/verify.yaml");

fn code(yaml: &str) -> String {
    match compile(yaml) {
        Ok(_) => "ok".to_string(),
        Err(e) => e.code().to_string(),
    }
}

/// Every case must give `want`; the message names the first that did not.
fn all_refused(want: &str, cases: &[(&str, &str)]) {
    for (name, yaml) in cases {
        assert_eq!(code(yaml), want, "case {name}: {:?}", compile(yaml).err());
    }
}

#[test]
fn the_verifier_pilot_compiles_to_a_hashed_ir() {
    let flow = compile(VERIFY).unwrap();
    assert_eq!(flow.name(), "verify@1");
    let ir = flow.ir();
    assert!(matches!(ir.nodes["pin"].class, Class::Observe { fresh_for: Some(600), .. }));
    assert!(matches!(ir.nodes["check"].class, Class::Pure { .. }));
    assert!(matches!(ir.nodes["exercise"].class, Class::Work { .. }));
    assert_eq!(ir.nodes["check"].needs, vec![Need::Node { node: "pin".into() }]);
    assert_eq!(ir.params.get("surface"), Some(&false));
    assert_eq!(ir.params.get("head"), Some(&true));
    // The hash is a function of the content: the same text gives the same hash, a change another.
    assert!(flow.hash().starts_with("sha256:"));
    assert_eq!(compile(VERIFY).unwrap().hash(), flow.hash());
    let changed = VERIFY.replace("fresh_for: 10m", "fresh_for: 11m");
    assert_ne!(compile(&changed).unwrap().hash(), flow.hash());
}

#[test]
fn an_unknown_reference_is_refused() {
    all_refused(
        "unknown_reference",
        &[
            ("needs", "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x], needs: [ghost]}\n"),
            ("switch on", "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x], switch: {on: ghost, case: y}}\n"),
            (
                "fork branch",
                "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\nforks:\n  f: {all: [a, ghost]}\n",
            ),
            (
                "repeat body",
                "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\nrepeats:\n  r: {body: [a, ghost], max: 2}\n",
            ),
            (
                "on_exhausted",
                "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\nrepeats:\n  r: {body: [a], max: 2, on_exhausted: ghost}\n",
            ),
            (
                "observe input param",
                "flow: f\nversion: 1\nnodes:\n  a: {class: observe, run: [x], inputs: [params.ghost], exit_map: {0: ok}}\n",
            ),
            (
                "observe input node",
                "flow: f\nversion: 1\nnodes:\n  a: {class: observe, run: [x], inputs: [ghost], exit_map: {0: ok}}\n",
            ),
        ],
    );
}

#[test]
fn a_loop_without_a_max_is_refused() {
    let repeat = |max: &str| {
        format!("flow: f\nversion: 1\nnodes:\n  a: {{class: pure, run: [x]}}\nrepeats:\n  r: {{body: [a]{max}}}\n")
    };
    all_refused(
        "unbounded_loop",
        &[("no max", &repeat("")), ("max 0", &repeat(", max: 0")), ("max past the bound", &repeat(", max: 100000"))],
    );
    assert_eq!(code(&repeat(", max: 1000")), "ok");
}

#[test]
fn a_cycle_is_refused_because_only_a_repeat_may_loop() {
    all_refused(
        "unbounded_loop",
        &[
            (
                "two nodes",
                "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x], needs: [b]}\n  b: {class: pure, run: [x], needs: [a]}\n",
            ),
            ("itself", "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x], needs: [a]}\n"),
            (
                "through a switch",
                "flow: f\nversion: 1\nnodes:\n  c: {class: choice, edges: {y: {guard: 'true'}}, needs: [a]}\n  a: {class: pure, run: [x], switch: {on: c, case: y}}\n",
            ),
            (
                "a body node needing its own repeat",
                "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x], needs: [r]}\nrepeats:\n  r: {body: [a], max: 2}\n",
            ),
        ],
    );
}

#[test]
fn a_switch_case_its_subject_does_not_declare_is_refused() {
    all_refused(
        "undeclared_edge",
        &[
            (
                "choice edge",
                "flow: f\nversion: 1\nnodes:\n  c: {class: choice, edges: {quick: {guard: 'true'}}}\n  a: {class: pure, run: [x], switch: {on: c, case: deep}}\n",
            ),
            (
                "observed value",
                "flow: f\nversion: 1\nnodes:\n  o: {class: observe, run: [x], fresh_for: 1m, exit_map: {0: green}}\n  a: {class: pure, run: [x], switch: {on: o, case: red}}\n",
            ),
            (
                "a class with no values",
                "flow: f\nversion: 1\nnodes:\n  w: {class: work, verify: [x]}\n  a: {class: pure, run: [x], switch: {on: w, case: done}}\n",
            ),
        ],
    );
    let declared = "flow: f\nversion: 1\nnodes:\n  c: {class: choice, edges: {deep: {guard: 'true'}}}\n  o: {class: observe, run: [x], fresh_for: 1m, exit_map: {0: green}}\n  a: {class: pure, run: [x], switch: {on: c, case: deep}}\n  b: {class: pure, run: [x], switch: {on: o, case: green}}\n";
    assert_eq!(code(declared), "ok");
}

#[test]
fn an_effect_without_reconcile_is_refused() {
    let effect = |reconcile: &str| {
        format!(
            "flow: f\nversion: 1\nnodes:\n  push: {{class: effect, preconditions: ['.params != null']{reconcile}}}\n"
        )
    };
    assert_eq!(code(&effect("")), "effect_without_reconcile");
    assert_eq!(code(&effect(", reconcile: []")), "effect_without_reconcile");
    assert_eq!(code(&effect(", reconcile: [git, ls-remote]")), "ok");
}

#[test]
fn a_repeated_id_is_refused_wherever_it_is_declared() {
    all_refused(
        "duplicate_id",
        &[
            (
                "node key twice",
                "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\n  a: {class: pure, run: [y]}\n",
            ),
            (
                "a fork named as a node",
                "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\nforks:\n  a: {all: [a]}\n",
            ),
            (
                "a repeat named as a node",
                "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\nrepeats:\n  a: {body: [a], max: 2}\n",
            ),
            ("a param twice", "flow: f\nversion: 1\nparams: [p, p?]\nnodes:\n  a: {class: pure, run: [x]}\n"),
            (
                "an edge twice",
                "flow: f\nversion: 1\nnodes:\n  c: {class: choice, edges: {y: {guard: 'true'}, y: {guard: 'false'}}}\n",
            ),
            (
                "an exit status twice",
                "flow: f\nversion: 1\nnodes:\n  o: {class: observe, run: [x], fresh_for: 1m, exit_map: {0: a, 0: b}}\n",
            ),
        ],
    );
}

#[test]
fn subflows_traits_and_templates_are_refused_as_phase_two() {
    all_refused(
        "unsupported",
        &[
            ("trait", "flow: f\nversion: 1\nuses: [verified-finish@1]\nnodes:\n  a: {class: pure, run: [x]}\n"),
            ("template", "flow: f\nversion: 1\ntemplates: {t: {}}\nnodes:\n  a: {class: pure, run: [x]}\n"),
            ("subflow", "flow: f\nversion: 1\nnodes:\n  a: {class: work, verify: [x], uses: pr-head@1}\n"),
        ],
    );
    let err =
        compile("flow: f\nversion: 1\nuses: [verified-finish@1]\nnodes:\n  a: {class: pure, run: [x]}\n").unwrap_err();
    assert!(err.to_string().contains("not supported in phase 1"), "{err}");
}

#[test]
fn a_guard_must_parse_and_read_only_its_input() {
    let edge = |guard: &str| {
        format!("flow: f\nversion: 1\nnodes:\n  c: {{class: choice, edges: {{y: {{guard: '{guard}'}}}}}}\n")
    };
    all_refused(
        "invalid_guard",
        &[
            ("syntax", &edge(".a ==")),
            ("the clock", &edge("now > 0")),
            ("the environment", &edge("env.HOME != null")),
            ("the time zone", &edge(".t | localtime | length > 0")),
            ("an input stream", &edge("input")),
            ("a print", &edge(". | debug | true")),
            ("a halt", &edge("halt")),
            ("a module file", &edge("include \"guards\"; true")),
            ("recursion", &edge("def f: f; f")),
            ("an unbounded generator", &edge("last(range(1e15)) > 0")),
            ("work the log sizes", &edge("[range(0; .nodes.a.outputs.n)] | length > 0")),
            ("a string the log repeats", &edge("(\"x\" * .n | length) > 0")),
            ("a banned word inside an interpolation", &edge("\"\\(now)\" != \"\"")),
            ("a banned word inside a block", &edge("[.[] | repeat(.)] | length > 0")),
            (
                "an until",
                "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\nrepeats:\n  r: {body: [a], max: 2, until: 'now > 0'}\n",
            ),
            (
                "a precondition",
                "flow: f\nversion: 1\nnodes:\n  e: {class: effect, preconditions: ['.a =='], reconcile: [x]}\n",
            ),
        ],
    );
    assert_eq!(code(&edge(".params.depth == \"deep\" and (.nodes | length) > 0")), "ok");
}

#[test]
fn class_rules_are_held() {
    all_refused(
        "definition_invalid",
        &[
            ("a misspelt key", "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x], need: [b]}\n"),
            (
                "a field of another class",
                "flow: f\nversion: 1\nnodes:\n  a: {class: work, verify: [x], edges: {y: {guard: 'true'}}}\n",
            ),
            ("an empty command", "flow: f\nversion: 1\nnodes:\n  a: {class: pure}\n"),
            (
                "an observation with no freshness rule",
                "flow: f\nversion: 1\nnodes:\n  o: {class: observe, run: [x], exit_map: {0: ok}}\n",
            ),
            (
                "an observation with no exit_map",
                "flow: f\nversion: 1\nnodes:\n  o: {class: observe, run: [x], fresh_for: 1m}\n",
            ),
            (
                "an observation reading a node it does not need",
                "flow: f\nversion: 1\nnodes:\n  b: {class: pure, run: [x]}\n  o: {class: observe, run: [x], inputs: [b], exit_map: {0: ok}}\n",
            ),
            ("a choice with no edges", "flow: f\nversion: 1\nnodes:\n  c: {class: choice}\n"),
            ("a human node with no ask", "flow: f\nversion: 1\nnodes:\n  h: {class: human, ask: ' '}\n"),
            ("zero attempts", "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x], retry: {attempts: 0}}\n"),
            (
                "a node in two repeats",
                "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\nrepeats:\n  r: {body: [a], max: 2}\n  s: {body: [a], max: 2}\n",
            ),
            (
                "exhaustion routed into the body",
                "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\nrepeats:\n  r: {body: [a], max: 2, on_exhausted: a}\n",
            ),
            (
                "a duration that does not parse",
                "flow: f\nversion: 1\nnodes:\n  o: {class: observe, run: [x], fresh_for: soon, exit_map: {0: ok}}\n",
            ),
            (
                "an empty fork",
                "flow: f\nversion: 1\nnodes:\n  j: {class: pure, run: [x], needs: [s]}\nforks:\n  s: {all: []}\n",
            ),
            (
                "an empty repeat body",
                "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\nrepeats:\n  r: {body: [], max: 3}\n",
            ),
            ("a node id that reads as a param", "flow: f\nversion: 1\nnodes:\n  'params.x': {class: pure, run: [x]}\n"),
            (
                "a param name that is not plain",
                "flow: f\nversion: 1\nparams: ['a.b']\nnodes:\n  a: {class: pure, run: [x]}\n",
            ),
            (
                "a zero duration",
                "flow: f\nversion: 1\nnodes:\n  o: {class: observe, run: [x], fresh_for: 0s, exit_map: {0: ok}}\n",
            ),
        ],
    );
    let ok = "flow: f\nversion: 1\nnodes:\n  b: {class: pure, run: [x]}\n  o: {class: observe, run: [x], needs: [b], inputs: [b], exit_map: {0: ok}}\n  h: {class: human, ask: Approve the budget?, retry: {attempts: 1}, cancel: {after: 1h}}\n";
    assert_eq!(code(ok), "ok", "{:?}", compile(ok).err());
}
