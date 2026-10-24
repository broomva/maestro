//! The validator places what the catalog names and records every drop, once, with its reason.

use maestro_view::catalog::{self, Block, MAX_BLOCKS, MAX_DEPTH, MAX_HANDLE, validate};
use serde_json::{Value, json};

fn reasons(v: &catalog::Validated) -> Vec<(String, String)> {
    v.drops.iter().map(|d| (d.block.clone(), d.reason.clone())).collect()
}

fn ids(n: &catalog::Node) -> Vec<String> {
    let mut out = vec![n.id.clone()];
    for c in &n.children {
        out.extend(ids(c));
    }
    out
}

#[test]
fn a_valid_spec_places_every_block_and_drops_none() {
    let v = validate(&json!({
        "root": "main",
        "blocks": {
            "main": { "type": "stack", "children": ["s", "n", "a"] },
            "s": { "type": "snapshot" },
            "n": { "type": "needs", "props": { "project": "demo", "limit": 3 } },
            "a": { "type": "ask", "props": { "ledger": "demo-arc.yaml", "id": "merge-now" } },
        }
    }))
    .unwrap();
    assert!(v.drops.is_empty(), "{:?}", v.drops);
    assert_eq!(ids(&v.root), ["main", "s", "n", "a"]);
    assert_eq!(v.kept["blocks"]["n"]["props"], json!({ "project": "demo", "limit": 3 }));
}

#[test]
fn an_unknown_type_is_dropped_and_recorded() {
    let v = validate(&json!({
        "root": "main",
        "blocks": {
            "main": { "type": "stack", "children": ["x", "s"] },
            "x": { "type": "markdown", "props": { "text": "Anything the agent likes" } },
            "s": { "type": "snapshot" },
        }
    }))
    .unwrap();
    assert_eq!(ids(&v.root), ["main", "s"]);
    assert_eq!(v.drops.len(), 1);
    assert_eq!(v.drops[0].block, "x");
    assert_eq!(v.drops[0].r#type.as_deref(), Some("markdown"));
    assert!(v.drops[0].reason.contains("unknown block type"), "{}", v.drops[0].reason);
    // What the App polls with holds no trace of the dropped block.
    assert!(v.kept["blocks"].get("x").is_none());
    assert_eq!(v.kept["blocks"]["main"]["children"], json!(["s"]));
}

#[test]
fn a_literal_value_in_props_is_dropped_not_rendered() {
    // The catalog takes handles; a block that tries to carry its own text is refused.
    let v = validate(&json!({
        "root": "main",
        "blocks": {
            "main": { "type": "stack", "children": ["n"] },
            "n": { "type": "needs", "props": { "title": "Everything is fine", "count": 0 } },
        }
    }))
    .unwrap();
    assert_eq!(ids(&v.root), ["main"]);
    assert_eq!(v.drops.len(), 1);
    assert!(reasons(&v)[0].1.starts_with("invalid props: unknown field"), "{:?}", v.drops);
}

#[test]
fn extra_block_keys_are_refused() {
    let v = validate(&json!({
        "root": "main",
        "blocks": {
            "main": { "type": "stack", "children": ["s"] },
            "s": { "type": "snapshot", "html": "<b>hi</b>" },
        }
    }))
    .unwrap();
    assert_eq!(reasons(&v), [("s".into(), "a block takes only type, props and children, not `html`".into())]);
}

#[test]
fn handles_and_limits_are_bounded() {
    let long = "x".repeat(MAX_HANDLE + 1);
    let v = validate(&json!({
        "root": "main",
        "blocks": {
            "main": { "type": "stack", "children": ["a", "b", "c", "d", "e", "f"] },
            "a": { "type": "ask", "props": { "ledger": long, "id": "x" } },
            "b": { "type": "ask", "props": { "ledger": "l.yaml", "id": "" } },
            "c": { "type": "asks", "props": { "limit": 0 } },
            "d": { "type": "work", "props": { "limit": 51 } },
            "e": { "type": "loops", "props": { "id": "a\nb" } },
            "f": { "type": "ask", "props": { "ledger": "a\u{202e}b\u{E0041}.yaml", "id": "x" } },
        }
    }))
    .unwrap();
    assert_eq!(ids(&v.root), ["main"]);
    let r = reasons(&v);
    assert_eq!(r.len(), 6, "{r:?}");
    assert!(r[0].1.contains("longer than 200"), "{r:?}");
    assert!(r[1].1.contains("`id` is empty"), "{r:?}");
    assert!(r[2].1.contains("`limit` must be 1"), "{r:?}");
    assert!(r[3].1.contains("`limit` must be 1"), "{r:?}");
    assert!(r[4].1.contains("control character"), "{r:?}");
    assert!(r[5].1.contains("invisible formatting character"), "{r:?}");
    // The edges are inside.
    let ok = validate(&json!({
        "root": "main",
        "blocks": {
            "main": { "type": "stack", "children": ["a", "c", "d"] },
            "a": { "type": "ask", "props": { "ledger": "x".repeat(MAX_HANDLE), "id": "x" } },
            "c": { "type": "asks", "props": { "limit": 1 } },
            "d": { "type": "work", "props": { "limit": 50 } },
        }
    }))
    .unwrap();
    assert!(ok.drops.is_empty(), "{:?}", ok.drops);
}

#[test]
fn a_block_is_placed_once_and_a_cycle_ends() {
    let v = validate(&json!({
        "root": "main",
        "blocks": {
            "main": { "type": "stack", "children": ["inner", "s"] },
            "inner": { "type": "stack", "children": ["s", "main"] },
            "s": { "type": "snapshot" },
        }
    }))
    .unwrap();
    assert_eq!(ids(&v.root), ["main", "inner", "s"]);
    let r = reasons(&v);
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(r[0].0, "main");
    assert!(r[0].1.starts_with("already placed"));
    assert_eq!(r[1].0, "s");
    assert!(r[1].1.starts_with("already placed"));
}

#[test]
fn missing_and_unreachable_blocks_are_recorded() {
    let v = validate(&json!({
        "root": "main",
        "blocks": {
            "main": { "type": "stack", "children": ["ghost", 7] },
            "orphan": { "type": "fleet" },
        }
    }))
    .unwrap();
    let r = reasons(&v);
    assert_eq!(
        r,
        [
            ("7".into(), "a child of `main` must be a block id string".into()),
            ("ghost".into(), "no block has this id".into()),
            ("orphan".into(), "not reachable from the root".into()),
        ]
    );
}

#[test]
fn a_leaf_keeps_its_place_and_drops_its_children() {
    let v = validate(&json!({
        "root": "main",
        "blocks": {
            "main": { "type": "stack", "children": ["n"] },
            "n": { "type": "needs", "children": ["s"] },
            "s": { "type": "snapshot" },
        }
    }))
    .unwrap();
    assert_eq!(ids(&v.root), ["main", "n"]);
    assert_eq!(reasons(&v), [("s".into(), "`n` is a needs, which takes no children".into())]);
}

#[test]
fn depth_and_count_are_capped() {
    let mut blocks = serde_json::Map::new();
    for d in 0..=MAX_DEPTH + 1 {
        let next = format!("d{}", d + 1);
        blocks.insert(format!("d{d}"), json!({ "type": "stack", "children": [next] }));
    }
    blocks.insert(format!("d{}", MAX_DEPTH + 2), json!({ "type": "snapshot" }));
    let v = validate(&json!({ "root": "d0", "blocks": blocks })).unwrap();
    assert_eq!(ids(&v.root).len(), MAX_DEPTH + 1);
    let deep = format!("d{}", MAX_DEPTH + 1);
    assert!(v.drops.iter().any(|d| d.block == deep && d.reason.contains("deeper than")), "{:?}", v.drops);

    // Two stacks of 40 and 30 snapshots: main, a, s0..s39, b and s40..s60 make 64; s61..s69 are over.
    let mut blocks = serde_json::Map::new();
    blocks.insert("main".into(), json!({ "type": "stack", "children": ["a", "b"] }));
    let kids = |r: std::ops::Range<usize>| r.map(|i| json!(format!("s{i}"))).collect::<Vec<Value>>();
    blocks.insert("a".into(), json!({ "type": "stack", "children": kids(0..40) }));
    blocks.insert("b".into(), json!({ "type": "stack", "children": kids(40..70) }));
    for i in 0..70 {
        blocks.insert(format!("s{i}"), json!({ "type": "snapshot" }));
    }
    let v = validate(&json!({ "root": "main", "blocks": blocks })).unwrap();
    assert_eq!(ids(&v.root).len(), MAX_BLOCKS);
    assert_eq!(v.drops.iter().filter(|d| d.reason.contains("over the limit")).count(), 9);
}

#[test]
fn a_spec_with_no_valid_root_is_an_error() {
    assert!(validate(&json!({ "root": "x", "blocks": {} })).unwrap_err().contains("not in `blocks`"));
    assert!(validate(&json!({ "root": "x", "blocks": { "x": { "type": "nope" } } })).unwrap_err().contains("unknown"));
    assert!(validate(&json!({ "blocks": {} })).is_err());
    assert!(validate(&json!({ "root": "x", "blocks": { "x": { "type": "fleet" } }, "theme": "x" })).is_err());
    assert!(validate(&json!([])).is_err());
}

#[test]
fn the_catalog_lists_every_type_with_a_props_schema() {
    let c = catalog::catalog();
    let listed: Vec<&str> = c["blocks"].as_array().unwrap().iter().map(|b| b["type"].as_str().unwrap()).collect();
    assert_eq!(listed, catalog::type_names());
    for (name, schema) in catalog::schemas() {
        assert_eq!(schema["type"], "object", "{name}");
    }
    // Only stack takes children.
    let parents: Vec<&str> = c["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["children"] == true)
        .map(|b| b["type"].as_str().unwrap())
        .collect();
    assert_eq!(parents, ["stack"]);
    // Every listed type validates with empty props, except the ask, which needs its handle.
    for name in catalog::type_names() {
        let r = validate(&json!({ "root": "b", "blocks": { "b": { "type": name } } }));
        assert_eq!(r.is_ok(), name != "ask", "{name}");
    }
    let v = validate(&json!({ "root": "b", "blocks": { "b": { "type": "work", "props": { "state": "Needs you" } } } }))
        .unwrap();
    assert!(matches!(v.root.block, Block::Work(_)));
}

#[test]
fn an_oversized_spec_is_refused_whole() {
    let mut blocks = serde_json::Map::new();
    blocks.insert("r".into(), json!({ "type": "snapshot" }));
    for i in 0..maestro_view::catalog::MAX_SPEC_BLOCKS {
        blocks.insert(format!("u{i}"), json!({ "type": "snapshot" }));
    }
    let e = validate(&json!({ "root": "r", "blocks": blocks })).unwrap_err();
    assert!(e.contains("at most 256"), "{e}");
    blocks.retain(|k, _| k == "r" || k.len() < 4); // r, u0..u99
    assert!(validate(&json!({ "root": "r", "blocks": blocks.clone() })).is_ok());

    let kids: Vec<String> = (0..=maestro_view::catalog::MAX_BLOCKS).map(|i| format!("u{i}")).collect();
    blocks.insert("r".into(), json!({ "type": "stack", "children": kids }));
    let e = validate(&json!({ "root": "r", "blocks": blocks.clone() })).unwrap_err();
    assert!(e.contains("lists 65 children; at most 64"), "{e}");
    blocks.insert("r".into(), json!({ "type": "stack", "children": kids[..64] }));
    assert!(validate(&json!({ "root": "r", "blocks": blocks })).is_ok());
}

#[test]
fn the_published_schema_admits_what_the_validator_admits() {
    let cat = maestro_view::catalog();
    let props = |t: &str| cat["blocks"].as_array().unwrap().iter().find(|b| b["type"] == t).unwrap()["props"].clone();
    for t in ["needs", "asks", "work"] {
        let limit = &props(t)["properties"]["limit"];
        assert_eq!((limit["minimum"].as_u64(), limit["maximum"].as_u64()), (Some(1), Some(50)), "{t}: {limit}");
    }
    let states = props("work")["properties"]["state"]["enum"].clone();
    let published: Vec<Option<String>> = serde_json::from_value(states).unwrap();
    assert_eq!(published.len(), 7);
    let labels: Vec<String> = published.into_iter().flatten().collect();
    for l in &labels {
        let s: maestro_core::work::WorkState = serde_json::from_value(json!(l)).unwrap();
        assert_eq!(s.label(), l);
    }
    assert_eq!(labels.len(), 6);
    // `null` is published and accepted alike.
    assert!(
        validate(&json!({ "root": "w", "blocks": { "w": { "type": "work", "props": { "state": null } } } })).is_ok()
    );
    // Handles publish the bounds the validator enforces.
    for (t, field) in [("needs", "project"), ("asks", "project"), ("ask", "ledger"), ("ask", "id"), ("loops", "id")] {
        let p = &props(t)["properties"][field];
        assert_eq!(
            (p["minLength"].as_u64(), p["maxLength"].as_u64()),
            (Some(1), Some(MAX_HANDLE as u64)),
            "{t}.{field}: {p}"
        );
    }
}

#[test]
fn every_class_of_format_character_is_refused_in_a_handle() {
    // One from each run of Unicode 16's Cf set.
    for c in [
        '\u{00AD}',
        '\u{0600}',
        '\u{061C}',
        '\u{06DD}',
        '\u{070F}',
        '\u{0890}',
        '\u{08E2}',
        '\u{180E}',
        '\u{200B}',
        '\u{202A}',
        '\u{2060}',
        '\u{2066}',
        '\u{FEFF}',
        '\u{FFF9}',
        '\u{110BD}',
        '\u{110CD}',
        '\u{13430}',
        '\u{1BCA0}',
        '\u{1D173}',
        '\u{E0001}',
        '\u{E0041}',
    ] {
        let v = validate(&json!({
            "root": "m",
            "blocks": { "m": { "type": "stack", "children": ["a"] }, "a": { "type": "loops", "props": { "id": format!("a{c}b") } } }
        }))
        .unwrap();
        assert!(
            reasons(&v).iter().any(|(_, r)| r.contains("invisible formatting")),
            "U+{:04X}: {:?}",
            c as u32,
            v.drops
        );
    }
}

#[test]
fn the_drop_report_is_bounded() {
    let mut blocks = serde_json::Map::new();
    blocks.insert("r".into(), json!({ "type": "snapshot" }));
    for i in 0..201 {
        blocks.insert(format!("u{i}"), json!({ "type": "snapshot" }));
    }
    let v = validate(&json!({ "root": "r", "blocks": blocks })).unwrap();
    assert_eq!(v.drops.len(), catalog::MAX_DROPS + 1, "{:?}", v.drops.last());
    assert_eq!(v.drops.last().unwrap().reason, "137 more drops not listed");
}

#[test]
fn a_child_reference_drop_echoes_a_bounded_parent_id() {
    // 63 leaf blocks with the longest id each list 64 children: 4,032 drops, each naming
    // its parent. Only MAX_DROPS are built and every reason stays short.
    let long = |i: usize| format!("{i:0>width$}", width = MAX_HANDLE);
    let mut blocks = serde_json::Map::new();
    blocks.insert("r".into(), json!({ "type": "stack", "children": (0..63).map(long).collect::<Vec<_>>() }));
    for i in 0..63 {
        blocks.insert(long(i), json!({ "type": "fleet", "children": vec!["x"; MAX_BLOCKS] }));
    }
    let v = validate(&json!({ "root": "r", "blocks": blocks })).unwrap();
    assert_eq!(v.drops.len(), catalog::MAX_DROPS + 1);
    assert_eq!(v.drops.last().unwrap().reason, format!("{} more drops not listed", 63 * 64 - catalog::MAX_DROPS));
    assert!(v.drops.iter().all(|d| d.reason.chars().count() < MAX_HANDLE + 60), "{:?}", v.drops[0]);
}

fn refused(spec: serde_json::Value, why: &str) {
    let e = validate(&spec).unwrap_err();
    assert!(e.contains(why), "{e}");
    assert!(e.len() < 120, "{} bytes: an error never echoes the name it refuses", e.len());
}

#[test]
fn every_name_a_spec_sends_is_a_handle() {
    let long = "x".repeat(MAX_HANDLE + 1);
    let snap = json!({ "type": "snapshot" });
    refused(json!({ "root": long, "blocks": { "r": snap } }), "`root` is longer");
    refused(json!({ "root": "r\u{202e}", "blocks": { "r": snap } }), "`root` holds an invisible");
    refused(json!({ "root": "r", "blocks": { "r": snap, long.clone(): snap } }), "`block id` is longer");
    refused(json!({ "root": "r", "blocks": { "r": snap, "a\u{0}": snap } }), "`block id` holds a control");
    refused(json!({ "root": "r", "blocks": { "r": { "type": long } } }), "`type` is longer");
    refused(
        json!({ "root": "r", "blocks": { "r": { "type": "snapshot", long.clone(): 1 } } }),
        "`block key` is longer",
    );
    refused(json!({ "root": "r", "blocks": { "r": { "type": "stack", "children": [long] } } }), "`child id` is longer");
    refused(json!({ "root": "r", "blocks": { "r": snap }, long.clone(): 1 }), "takes only `root` and `blocks`");
}

#[test]
fn a_serde_message_is_clipped() {
    let long = "Z".repeat(10_000);
    let v = validate(&json!({
        "root": "r",
        "blocks": { "r": { "type": "stack", "children": ["w"] }, "w": { "type": "work", "props": { "limit": long } } }
    }))
    .unwrap();
    assert_eq!(v.drops.len(), 1);
    assert!(v.drops[0].reason.starts_with("invalid props: "), "{}", v.drops[0].reason);
    assert!(
        v.drops[0].reason.chars().count() <= "invalid props: ".len() + MAX_HANDLE + 1,
        "{}",
        v.drops[0].reason.len()
    );
}
