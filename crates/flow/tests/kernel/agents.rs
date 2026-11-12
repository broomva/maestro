//! BRO-3636: specialist agents. The source parses strictly, the kernel's built-in agents are the
//! repository's sources, the committed renders are a fresh render of them, and drift between a
//! source and its renders is named file by file.

use maestro_flow::agents::{self, Agent, AgentError, Drift, DriftKind, LaunchSpec, Posture};
use maestro_flow::kernel::ir::Launch;
use std::path::{Path, PathBuf};

const REPO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const VERIFIER: &str = include_str!("../../../../agents/verifier.agent.yaml");

fn repo() -> PathBuf {
    std::fs::canonicalize(REPO).unwrap()
}

fn refused(file: &str, text: &str) -> String {
    match agents::parse(file, text) {
        Ok(a) => panic!("{file} parsed: {a:?}"),
        Err(e) => {
            assert_eq!(e.code(), "agent_invalid", "{e}");
            e.to_string()
        }
    }
}

#[test]
fn the_verifier_source_parses_to_its_fields() {
    let a = agents::parse("verifier.agent.yaml", VERIFIER).unwrap();
    assert_eq!(a.name, "verifier");
    assert_eq!(a.posture, Posture::Execute);
    assert_eq!(a.skills, ["dogfood"]);
    assert_eq!(a.model.codex, "gpt-6.1-terra");
    assert_eq!(a.source, "agents/verifier.agent.yaml");
    assert_eq!(a.outputs["required"], serde_json::json!(["receipt", "receipt_sha256"]));
    assert!(a.source_sha256.starts_with("sha256:") && a.source_sha256.len() == 71, "{}", a.source_sha256);
}

#[test]
fn a_source_is_parsed_strictly() {
    let f = "verifier.agent.yaml";
    let with = |old: &str, new: &str| {
        assert_eq!(VERIFIER.matches(old).count(), 1, "{old}");
        VERIFIER.replace(old, new)
    };
    let cases: &[(&str, String, &str)] = &[
        ("an unknown field", format!("{VERIFIER}extra: 1\n"), "unknown field `extra`"),
        (
            "an unknown provider",
            with("  grok: grok-4.7\n", "  grok: grok-4.7\n  openai: x\n"),
            "unknown field `openai`",
        ),
        ("a repeated field", format!("{VERIFIER}posture: read_only\n"), "duplicate field `posture`"),
        ("a missing field", with("posture: execute\n", ""), "missing field `posture`"),
        ("an unknown posture", with("posture: execute", "posture: root"), "unknown variant `root`"),
        ("another schema", with("schema: agent@1", "schema: agent@2"), "not \"agent@1\""),
        ("an upper-case name", with("name: verifier", "name: Verifier"), "must be lower-case"),
        ("a doubled dash", with("name: verifier", "name: veri--fier"), "must be lower-case"),
        ("a leading dash", with("name: verifier", "name: -verifier"), "must be lower-case"),
        ("a name with a colon", with("name: verifier", "name: 'agent:verifier'"), "must be lower-case"),
        ("a name its file does not have", with("name: verifier", "name: checker"), "lives in checker.agent.yaml"),
        ("a model with a space", with("codex: gpt-6.1-terra", "codex: gpt 6"), "the codex model"),
        ("an empty model", with("claude: sonnet", "claude: ''"), "the claude model"),
        ("a skill that is no skill name", with("skills: [dogfood]", "skills: [Dog_food]"), "not a skill name"),
        ("a skill named twice", with("skills: [dogfood]", "skills: [dogfood, dogfood]"), "named twice"),
        ("outputs of another type", with("  type: object\n  required", "  type: array\n  required"), "of type object"),
        (
            "outputs that are no schema",
            format!("{}outputs: 3\n", VERIFIER.split("# What the driver").next().unwrap()),
            "of type object",
        ),
    ];
    for (name, text, want) in cases {
        let reason = refused(f, text);
        assert!(reason.contains(want), "case {name}: {reason}");
    }

    // The fields the strict parse cannot express by serde alone, each on its own.
    let blank = VERIFIER.split("description:").next().unwrap().to_string()
        + "description: ' '\n"
        + &VERIFIER[VERIFIER.find("role:").unwrap()..];
    assert!(refused(f, &blank).contains("the description must be"), "a blank description");
    let long = VERIFIER.split("description:").next().unwrap().to_string()
        + &format!("description: {}\n", "x".repeat(agents::MAX_DESCRIPTION + 1))
        + &VERIFIER[VERIFIER.find("role:").unwrap()..];
    assert!(refused(f, &long).contains("the description must be"), "a long description");
    let role = VERIFIER[..VERIFIER.find("role:").unwrap()].to_string()
        + "role: ' '\n"
        + &VERIFIER[VERIFIER.find("posture:").unwrap()..];
    assert!(refused(f, &role).contains("the role is empty"), "a blank role");
    let big = format!("{VERIFIER}#{}\n", "x".repeat(agents::MAX_SOURCE));
    assert!(refused(f, &big).contains("more than"), "an oversized source");
    // Read only, and with no skill, is a source too.
    let read_only =
        VERIFIER.replace("posture: execute", "posture: read_only").replace("skills: [dogfood]", "skills: []");
    let a = agents::parse(f, &read_only).unwrap();
    assert_eq!((a.posture, a.skills.len()), (Posture::ReadOnly, 0));
}

#[test]
fn the_kernels_agents_are_the_sources_in_the_agents_folder() {
    let built = agents::builtin().unwrap();
    let on_disk = agents::load(&repo()).unwrap();
    assert_eq!(built, &on_disk[..], "a source added or changed in agents/ is registered in agents::BUILTIN");
    assert!(agents::find("verifier").is_some());
    assert!(agents::find("ghost").is_none());
}

#[test]
fn the_committed_renders_are_a_fresh_render_of_their_sources() {
    let root = repo();
    for r in agents::render_all(&agents::load(&root).unwrap()) {
        let on_disk = std::fs::read_to_string(root.join(&r.path)).unwrap_or_else(|e| panic!("{}: {e}", r.path));
        assert_eq!(on_disk, r.text, "{} is stale: run maestro agents render", r.path);
    }
    assert_eq!(agents::check(&root).unwrap(), None);
}

fn verifier() -> &'static Agent {
    agents::find("verifier").unwrap()
}

#[test]
fn each_render_carries_its_harness_fields_and_the_shared_body() {
    let a = verifier();
    let [claude, gemini, grok]: [agents::Render; 3] = a.renders().try_into().unwrap();
    assert_eq!(claude.path, ".claude/agents/verifier.md");
    assert_eq!(gemini.path, ".gemini/agents/verifier.md");
    assert_eq!(grok.path, ".grok/agents/verifier.md");
    let head = |text: &str| text.split("\n---\n").next().unwrap().to_string();
    assert!(
        head(&claude.text).contains("\ntools: \"Read, Grep, Glob, Bash\"\nmodel: \"sonnet\"\nskills: [\"dogfood\"]")
    );
    assert!(head(&gemini.text).contains("\nkind: \"local\"\n"));
    assert!(head(&gemini.text).contains("\"run_shell_command\", \"activate_skill\"]"));
    assert!(head(&gemini.text).contains("\nmodel: \"gemini-3-pro-preview\""));
    assert!(
        head(&grok.text)
            .contains("\ntools: [\"read_file\", \"grep_search\", \"list_dir\", \"bash\"]\nmodel: \"grok-4.7\"")
    );
    for r in [&claude, &gemini, &grok] {
        assert!(r.text.starts_with("---\n# Generated by maestro agents render from agents/verifier.agent.yaml."));
        assert!(r.text.ends_with(&a.body()), "{}", r.path);
    }
    // Read only takes no shell on any harness.
    let ro = agents::parse("verifier.agent.yaml", &VERIFIER.replace("posture: execute", "posture: read_only")).unwrap();
    for r in ro.renders() {
        let head = head(&r.text);
        assert!(!head.contains("Bash") && !head.contains("shell") && !head.contains("\"bash\""), "{}: {head}", r.path);
    }
    let brief = a.codex();
    assert_eq!(
        (brief.tool.as_str(), brief.model.as_str(), brief.task_name.as_str()),
        ("spawn_agent", "gpt-6.1-terra", "verifier")
    );
    assert_eq!(brief.message, a.body());
}

#[test]
fn a_launch_spec_is_given_only_for_an_agent_performer() {
    let launch = |performer: Option<&str>| Launch {
        performer: performer.map(str::to_string),
        skill: Some("dogfood".into()),
        routing: Default::default(),
    };
    let Some(LaunchSpec::Agent(spec)) = agents::launch_spec(&launch(Some("agent:verifier"))) else { panic!() };
    assert_eq!(spec.agent, "verifier");
    assert_eq!(spec.skill.as_deref(), Some("dogfood"));
    assert_eq!(spec.claude.subagent_type.as_deref(), Some("verifier"));
    assert_eq!(spec.codex, verifier().codex());
    assert_eq!(agents::launch_spec(&launch(None)), None);
    assert_eq!(agents::launch_spec(&launch(Some("human:owner"))), None);
    assert_eq!(
        agents::launch_spec(&launch(Some("agent:ghost"))),
        Some(LaunchSpec::UnknownAgent {
            agent: "ghost".into(),
            reason: "this maestro has no agent source agents/ghost.agent.yaml".into()
        })
    );
}

/// A root holding the verifier's source, rendered.
fn rendered() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::write(root.join("agents/verifier.agent.yaml"), VERIFIER).unwrap();
    let w = agents::write(&root).unwrap();
    assert_eq!(w.written, [".claude/agents/verifier.md", ".gemini/agents/verifier.md", ".grok/agents/verifier.md"]);
    assert_eq!(agents::check(&root).unwrap(), None);
    (tmp, root)
}

fn drift(root: &Path) -> Drift {
    agents::check(root).unwrap().expect("a drift")
}

#[test]
fn check_names_the_first_render_that_drifts() {
    let (_tmp, root) = rendered();
    let gemini = root.join(".gemini/agents/verifier.md");

    // A render edited by hand.
    std::fs::write(&gemini, std::fs::read_to_string(&gemini).unwrap().replace("gemini-3", "gemini-2")).unwrap();
    assert_eq!(drift(&root), Drift { file: ".gemini/agents/verifier.md".into(), kind: DriftKind::Differs });
    // Longer than the render, by one byte at the end.
    agents::write(&root).unwrap();
    std::fs::write(&gemini, std::fs::read_to_string(&gemini).unwrap() + "\n").unwrap();
    assert_eq!(drift(&root).kind, DriftKind::Differs);
    // Longer by a character the cut at the render's length splits: drift, never unreadable.
    agents::write(&root).unwrap();
    std::fs::write(&gemini, std::fs::read_to_string(&gemini).unwrap() + "\u{e9}").unwrap();
    assert_eq!(drift(&root).kind, DriftKind::Differs);

    // A render that is gone.
    agents::write(&root).unwrap();
    std::fs::remove_file(root.join(".grok/agents/verifier.md")).unwrap();
    assert_eq!(drift(&root), Drift { file: ".grok/agents/verifier.md".into(), kind: DriftKind::Missing });

    // A source changed after its renders: the first render is named.
    agents::write(&root).unwrap();
    let source = root.join("agents/verifier.agent.yaml");
    std::fs::write(&source, VERIFIER.replace("claude: sonnet", "claude: opus")).unwrap();
    let d = drift(&root);
    assert_eq!(d, Drift { file: ".claude/agents/verifier.md".into(), kind: DriftKind::Differs });
    assert!(d.to_string().contains(".claude/agents/verifier.md"), "{d}");
    agents::write(&root).unwrap();
    assert_eq!(agents::check(&root).unwrap(), None);
}

#[test]
fn a_render_whose_source_is_gone_is_drift_and_write_removes_only_renders() {
    let (_tmp, root) = rendered();
    // A second agent, rendered, then its source removed.
    let checker = VERIFIER.replace("name: verifier", "name: checker");
    std::fs::write(root.join("agents/checker.agent.yaml"), &checker).unwrap();
    agents::write(&root).unwrap();
    std::fs::remove_file(root.join("agents/checker.agent.yaml")).unwrap();
    // A hand-written agent beside the renders is no render.
    let mine = root.join(".claude/agents/mine.md");
    std::fs::write(&mine, "---\nname: mine\ndescription: by hand\n---\n\nHi.\n").unwrap();
    assert_eq!(drift(&root), Drift { file: ".claude/agents/checker.md".into(), kind: DriftKind::Orphan });

    let w = agents::write(&root).unwrap();
    assert_eq!(w.removed, [".claude/agents/checker.md", ".gemini/agents/checker.md", ".grok/agents/checker.md"]);
    assert!(mine.exists(), "a file that is not a render is left alone");
    assert_eq!(agents::check(&root).unwrap(), None);
}

#[test]
fn a_root_without_sources_is_unread_never_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let e = agents::check(tmp.path()).unwrap_err();
    assert!(matches!(e, AgentError::Io { .. }), "{e}");
    assert_eq!(e.code(), "agent_io");
    // A source that does not parse fails the check too, naming the file.
    std::fs::create_dir_all(tmp.path().join("agents")).unwrap();
    std::fs::write(tmp.path().join("agents/x.agent.yaml"), "schema: agent@1\n").unwrap();
    let e = agents::check(tmp.path()).unwrap_err();
    assert!(e.to_string().starts_with("x.agent.yaml is not a valid agent source"), "{e}");
}

#[test]
fn a_repeated_key_in_outputs_is_refused_at_any_level() {
    let f = "verifier.agent.yaml";
    let top = VERIFIER.replace("  type: object\n  required", "  type: object\n  type: object\n  required");
    assert!(refused(f, &top).contains("outputs repeats the key \"type\""), "{}", refused(f, &top));
    let nested = VERIFIER.replace(
        "      type: string\n      description: The receipt's path",
        "      type: string\n      type: number\n      description: The receipt's path",
    );
    assert!(refused(f, &nested).contains("outputs repeats the key \"type\""), "{}", refused(f, &nested));
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}

/// A root with the verifier's source and nothing rendered, and a folder outside it.
#[cfg(unix)]
fn bare_root() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("agents/verifier.agent.yaml"), VERIFIER).unwrap();
    (tmp, root, outside)
}

#[cfg(unix)]
#[test]
fn a_render_that_is_a_link_is_drift_and_write_replaces_the_link_not_its_target() {
    let (_tmp, root, outside) = bare_root();
    std::fs::create_dir_all(root.join(".claude/agents")).unwrap();
    // The link's target holds exactly the render: still drift, since it is a link.
    let render = verifier().renders().remove(0);
    let target = outside.join("victim.md");
    std::fs::write(&target, &render.text).unwrap();
    let at = root.join(".claude/agents/verifier.md");
    link(&target, &at);
    assert_eq!(drift(&root), Drift { file: ".claude/agents/verifier.md".into(), kind: DriftKind::Link });
    assert!(drift(&root).to_string().contains("it is a symbolic link"));

    std::fs::write(&target, "keep me\n").unwrap();
    agents::write(&root).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me\n", "the link's target is untouched");
    let m = std::fs::symlink_metadata(&at).unwrap();
    assert!(m.is_file() && !m.file_type().is_symlink(), "the link was replaced by the render");
    assert_eq!(std::fs::read_to_string(&at).unwrap(), render.text);
    assert_eq!(agents::check(&root).unwrap(), None);
}

#[cfg(unix)]
#[test]
fn a_render_folder_that_is_a_link_is_refused_and_nothing_is_written_through_it() {
    // The render folder itself, and a folder above it.
    for (made, linked) in [(".gemini", ".gemini/agents"), ("", ".grok")] {
        let (_tmp, root, outside) = bare_root();
        if !made.is_empty() {
            std::fs::create_dir_all(root.join(made)).unwrap();
        }
        std::fs::create_dir_all(outside.join("agents")).unwrap();
        let target = if linked.ends_with("agents") { outside.join("agents") } else { outside.clone() };
        link(&target, &root.join(linked));
        let e = agents::write(&root).unwrap_err();
        assert_eq!(e.code(), "render_path_unsafe", "{linked}: {e}");
        assert!(e.to_string().contains(linked) && e.to_string().contains("is a symbolic link"), "{e}");
        assert!(!outside.join("agents/verifier.md").exists(), "{linked}: nothing written outside the root");
        let e = agents::check(&root).unwrap_err();
        assert_eq!(e.code(), "render_path_unsafe", "{linked}: {e}");
    }
}

#[cfg(unix)]
#[test]
fn a_link_is_never_taken_for_an_orphan_or_removed() {
    let (_tmp, root, outside) = bare_root();
    agents::write(&root).unwrap();
    // A file outside the root that reads as a render whose source is gone, linked into a folder.
    let checker = agents::parse("checker.agent.yaml", &VERIFIER.replace("name: verifier", "name: checker")).unwrap();
    let target = outside.join("checker.md");
    std::fs::write(&target, &checker.renders()[0].text).unwrap();
    let at = root.join(".claude/agents/checker.md");
    link(&target, &at);
    assert_eq!(agents::check(&root).unwrap(), None, "a link is not a render of ours");
    let w = agents::write(&root).unwrap();
    assert!(w.removed.is_empty(), "{w:?}");
    assert!(std::fs::symlink_metadata(&at).unwrap().file_type().is_symlink(), "the link stays");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), checker.renders()[0].text, "its target stays");
}
