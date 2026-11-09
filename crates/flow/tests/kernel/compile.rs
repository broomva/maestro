//! BRO-3633 PR 3b1: flow definitions and the compiler. Each rejection is its own test, and the
//! tables name the case that failed.

use maestro_flow::kernel::compile;
use maestro_flow::kernel::ir::{Class, MAX_FAN_IN, MAX_NODES, Need};

const VERIFY: &str = include_str!("../../flows/verify.yaml");

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
fn an_ir_is_written_in_the_lowest_version_that_expresses_it() {
    // Version 2 added `workdir`: the pilot declares one; a flow without one is a version 1 IR,
    // as it was before the field existed.
    assert_eq!(compile(VERIFY).unwrap().ir().ir_version, 2);
    let bare = compile("flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\n").unwrap();
    assert_eq!(bare.ir().ir_version, 1);
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
            (
                "an outside node needing a body node whose sibling needs it back",
                "flow: f\nversion: 1\nnodes:\n  o: {class: pure, run: [x], needs: [m]}\n  m: {class: pure, run: [x]}\n  b: {class: pure, run: [x], needs: [o]}\nrepeats:\n  r: {body: [m, b], max: 2}\n",
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

#[test]
fn a_definition_holds_at_most_max_nodes() {
    let wide = |n: usize| {
        let nodes: String = (0..n).map(|i| format!("  n{i}: {{class: pure, run: [x]}}\n")).collect();
        format!("flow: f\nversion: 1\nnodes:\n{nodes}")
    };
    assert_eq!(code(&wide(MAX_NODES)), "ok");
    assert_eq!(code(&wide(MAX_NODES + 1)), "definition_invalid");
}

#[test]
fn a_node_reads_at_most_max_fan_in_nodes() {
    let join = |width: usize, repeat_body: usize| {
        let mut y = String::from("flow: f\nversion: 1\nnodes:\n");
        for i in 0..width + repeat_body {
            y.push_str(&format!("  n{i}: {{class: pure, run: [x]}}\n"));
        }
        let mut needs: Vec<String> = (0..width).map(|i| format!("n{i}")).collect();
        if repeat_body > 0 {
            needs.push("r".into());
            let body: Vec<String> = (width..width + repeat_body).map(|i| format!("n{i}")).collect();
            y.push_str(&format!(
                "  j: {{class: pure, run: [x], needs: [{}]}}\nrepeats:\n  r: {{max: 2, body: [{}]}}\n",
                needs.join(", "),
                body.join(", ")
            ));
        } else {
            y.push_str(&format!("  j: {{class: pure, run: [x], needs: [{}]}}\n", needs.join(", ")));
        }
        y
    };
    assert_eq!(code(&join(MAX_FAN_IN, 0)), "ok");
    assert_eq!(code(&join(MAX_FAN_IN + 1, 0)), "definition_invalid");
    // Needing a repeat reads its whole body.
    assert_eq!(code(&join(MAX_FAN_IN - 10, 10)), "ok");
    assert_eq!(code(&join(MAX_FAN_IN - 10, 11)), "definition_invalid");
    // So does needing a body node from outside its repeat.
    let outside = |body: usize| {
        let mut y = String::from("flow: f\nversion: 1\nnodes:\n");
        for i in 0..body {
            y.push_str(&format!("  n{i}: {{class: pure, run: [x]}}\n"));
        }
        let names: Vec<String> = (0..body).map(|i| format!("n{i}")).collect();
        y.push_str(&format!(
            "  j: {{class: pure, run: [x], needs: [n0]}}\nrepeats:\n  r: {{max: 2, body: [{}]}}\n",
            names.join(", ")
        ));
        y
    };
    assert_eq!(code(&outside(MAX_FAN_IN)), "ok");
    assert_eq!(code(&outside(MAX_FAN_IN + 1)), "definition_invalid");
    // A body is counted once, however many of its nodes are read, and with the repeat itself.
    let twelve: Vec<String> = (0..12).map(|i| format!("b{i}")).collect();
    let mut y = String::from("flow: f\nversion: 1\nnodes:\n");
    for b in &twelve {
        y.push_str(&format!("  {b}: {{class: pure, run: [x]}}\n"));
    }
    y.push_str("  j: {class: pure, run: [x], needs: [b0, b1, b2, b3, b4, b5, r]}\n");
    y.push_str(&format!("repeats:\n  r: {{max: 2, until: 'true', body: [{}]}}\n", twelve.join(", ")));
    assert_eq!(code(&y), "ok", "12 reads, not 78");
}

#[test]
fn a_definition_and_its_ir_are_small_enough_to_start() {
    use maestro_flow::kernel::MAX_SOURCE;
    use maestro_flow::kernel::ir::MAX_IR;
    let one = |arg: usize| format!("flow: f\nversion: 1\nnodes:\n  a: {{class: pure, run: [{}]}}\n", "x".repeat(arg));
    let ir_len = |yaml: &str| serde_json::to_vec(compile(yaml).unwrap().ir()).unwrap().len();
    // The IR grows a byte per byte of the argument: find the argument that makes it exactly MAX_IR.
    let base = ir_len(&one(1)) - 1;
    let at_cap = one(MAX_IR - base);
    assert_eq!(ir_len(&at_cap), MAX_IR);
    assert!(at_cap.len() <= MAX_SOURCE, "the source is within its own bound");
    assert_eq!(code(&one(MAX_IR - base + 1)), "definition_invalid", "a byte over the IR bound");
    // The source bound, padded with a comment that the IR never sees.
    let pad = |len: usize| {
        let head = "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\n#";
        format!("{head}{}\n", "x".repeat(len - head.len() - 1))
    };
    assert_eq!(pad(MAX_SOURCE).len(), MAX_SOURCE);
    assert_eq!(code(&pad(MAX_SOURCE)), "ok");
    assert_eq!(code(&pad(MAX_SOURCE + 1)), "definition_invalid");
}

#[test]
fn a_node_needed_twice_is_needed_once() {
    let flow = compile("flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\n  b: {class: pure, run: [x]}\n  c: {class: pure, run: [x], needs: [a, a, both]}\nforks:\n  both: {all: [a, b]}\n").unwrap();
    let needs = &flow.ir().nodes["c"].needs;
    assert_eq!(needs, &vec![Need::Node { node: "a".into() }, Need::Node { node: "b".into() }]);
}
