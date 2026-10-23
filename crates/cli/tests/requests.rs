//! Agent requests end to end (BRO-3267): `maestro ask file` files through the validator under the
//! ledger's lock, other requests are journalled and held, and the agent CLI carries no owner verb.
//! The validator here is the stub in fixtures/requests (the workspace repository is private); the
//! live check against the real ask_ledger.py is pasted in the PR.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/requests")
}

/// A scratch machine: a workspace with the seeded ledger and the stub at scripts/ask_ledger.py.
fn machine() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join(".control/asks")).unwrap();
    std::fs::create_dir_all(ws.join("scripts")).unwrap();
    std::fs::copy(fixtures().join("ledger.yaml"), ws.join(".control/asks/ledger.yaml")).unwrap();
    std::fs::copy(fixtures().join("ask_ledger_stub.py"), ws.join("scripts/ask_ledger.py")).unwrap();
    tmp
}

/// Where this machine's Python finds user-installed packages (PyYAML), read with the real HOME:
/// the scratch HOME below would hide them from the validator.
fn python_user_base() -> String {
    let out = Command::new("python3").args(["-m", "site", "--user-base"]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn maestro(root: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_maestro"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("PYTHONUSERBASE", python_user_base())
        .env("HOME", root.join("home"))
        .env("BROOMVA_WORKSPACE", root.join("ws"))
        .env("BROOMVA_HOME", root.join("broomva-home"))
        .env("MAESTRO_PASEO_ASK_LOCKS", root.join("locks"))
        .env("MAESTRO_FLEET_STATE", root.join("fleet"))
        .env("MAESTRO_TREE", "worktree");
    cmd
}

fn ask(id: &str) -> String {
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(fixtures().join("ask.json")).unwrap()).unwrap();
    v["id"] = Value::String(id.into());
    v.to_string()
}

fn file(root: &Path, id: &str) -> Output {
    maestro(root)
        .args(["--json", "ask", "file", "--session", "t3:local/test", "--ledger", "ledger.yaml", "--ask", &ask(id)])
        .output()
        .unwrap()
}

fn ledger(root: &Path) -> String {
    std::fs::read_to_string(root.join("ws/.control/asks/ledger.yaml")).unwrap()
}

fn listed(root: &Path) -> Vec<String> {
    let out = maestro(root).args(["--json", "asks", "--state", "all", "--limit", "500"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["errors"].as_array().map(Vec::len), Some(0), "{v}");
    v["asks"].as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn a_filed_ask_is_appended_validated_and_listed() {
    let m = machine();
    let out = file(m.path(), "filed-1");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["request"]["request"], "ask.file");
    assert_eq!(v["request"]["disposition"], "applied");
    assert_eq!(v["request"]["verified"], false);
    assert_eq!(v["validator"], "ask_ledger.py: 6 ask(s), no new error");
    let ids = listed(m.path());
    assert_eq!(ids.len(), 6);
    assert_eq!(ids.last().map(String::as_str), Some("filed-1"));
    let text = ledger(m.path());
    // Appended to the list, before the key that follows it; the origin is Maestro's and routes nowhere.
    assert!(text.find("id: \"filed-1\"").unwrap() < text.find("decisions: []").unwrap());
    assert!(
        text.contains("verified: false") && text.contains("session: \"t3:local/test\"") && !text.contains("agent_id")
    );
    let journal = std::fs::read_to_string(m.path().join("broomva-home/ledger/requests/requests.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 1);
    assert!(std::fs::read_dir(m.path().join("locks")).unwrap().next().is_none(), "no lock left behind");
}

#[test]
fn a_refused_ask_writes_nothing() {
    let m = machine();
    let before = ledger(m.path());
    let mut bad: Value = serde_json::from_str(&ask("bad")).unwrap();
    bad["stakes"] = Value::String("STUB-INVALID".into());
    let cases: Vec<(String, &str)> = vec![
        (bad.to_string(), "refused the ask"),
        (ask("seed-1"), "already has an ask `seed-1`"),
        (
            {
                let mut v: Value = serde_json::from_str(&ask("shadow")).unwrap();
                v["uid"] = Value::String("seed-1".into());
                v.to_string()
            },
            "already has an ask `seed-1`",
        ),
        (r#"{"id":"x","answer":"yes"}"#.to_string(), "`answer` is not the filer's"),
        (r#"{"id":"x","defaulted_at":"2026-10-23T00:00:00Z"}"#.to_string(), "`defaulted_at` is not the filer's"),
        (r#"{"id":"x","answered_by":"owner"}"#.to_string(), "`answered_by` is not the filer's"),
        (r#"{"id":"x","status":"resolved"}"#.to_string(), "`status` is not the filer's"),
        (r#"{"id":"x","origin":{"agent_id":"someone"}}"#.to_string(), "`origin` is not the filer's"),
        (r#"{"ask":"no id"}"#.to_string(), "needs an `id`"),
    ];
    for (body, why) in cases {
        let out = maestro(m.path())
            .args(["ask", "file", "--session", "t3:-/x", "--ledger", "ledger.yaml", "--ask", &body])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{why}");
        assert!(String::from_utf8_lossy(&out.stderr).contains(why), "{why}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(ledger(m.path()), before, "{why}");
    }
    for (ledger_name, why) in [("../x.yaml", "not a ledger name"), ("none.yaml", "no ledger none.yaml")] {
        let out = maestro(m.path())
            .args(["ask", "file", "--session", "t3:-/x", "--ledger", ledger_name, "--ask", &ask("y")])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stderr).contains(why), "{why}");
    }
    let out =
        maestro(m.path()).args(["ask", "file", "--session", "nosurface", "--ledger", "ledger.yaml"]).output().unwrap();
    assert!(!out.status.success());
    let names: Vec<_> =
        std::fs::read_dir(m.path().join("ws/.control/asks")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names.len(), 1, "no temp left behind: {names:?}");
}

#[test]
fn an_error_the_ledger_already_had_does_not_refuse_a_new_ask() {
    let m = machine();
    let path = m.path().join("ws/.control/asks/ledger.yaml");
    std::fs::write(&path, ledger(m.path()).replace("ask: \"Seeded ask 2", "ask: \"STUB-INVALID Seeded ask 2")).unwrap();
    let out = file(m.path(), "after-legacy");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(ledger(m.path()).contains("id: \"after-legacy\""));
}

/// A ledger whose asks sit under `open_asks`: appending an `asks` list would hide them all.
#[test]
fn an_open_asks_ledger_is_refused() {
    let m = machine();
    let path = m.path().join("ws/.control/asks/ledger.yaml");
    std::fs::write(&path, ledger(m.path()).replace("\nasks:\n", "\nopen_asks:\n")).unwrap();
    let before = ledger(m.path());
    let out = file(m.path(), "hidden");
    assert!(!out.status.success());
    assert_eq!(ledger(m.path()), before);
    assert_eq!(listed(m.path()).len(), 5);
}

/// The validator judges the result as the ledger itself (its name rules), never as the temp.
#[test]
fn the_validator_judges_the_ledgers_own_name() {
    let m = machine();
    let asks = m.path().join("ws/.control/asks");
    std::fs::rename(asks.join("ledger.yaml"), asks.join("ideas-foo.yaml")).unwrap();
    let file_into = |body: String| {
        maestro(m.path())
            .args(["ask", "file", "--session", "t3:-/x", "--ledger", "ideas-foo.yaml", "--ask", &body])
            .output()
            .unwrap()
    };
    let out = file_into(ask("not-an-idea"));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("holds ideas only"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut idea: Value = serde_json::from_str(&ask("an-idea")).unwrap();
    idea["class"] = Value::String("idea".into());
    let out = file_into(idea.to_string());
    assert!(out.status.success(), "the seeded asks' errors are old ones: {}", String::from_utf8_lossy(&out.stderr));
}

/// Strings YAML 1.1 reads as booleans or timestamps are written so every reader reads strings.
#[test]
fn every_reader_reads_the_same_strings() {
    let m = machine();
    let mut v: Value = serde_json::from_str(&ask("words")).unwrap();
    v["default"] = Value::String("no".into());
    v["options"][0]["id"] = Value::String("yes".into());
    v["stakes"] = Value::String("2026-10-23 on: \"quoted\" # not a comment\nsecond line".into());
    let out = maestro(m.path())
        .args(["ask", "file", "--session", "t3:-/x", "--ledger", "ledger.yaml", "--ask", &v.to_string()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let shown = maestro(m.path()).args(["--json", "asks", "show", "ledger.yaml", "words"]).output().unwrap();
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["ask"]["default"], "no");
    assert_eq!(shown["ask"]["stakes"], v["stakes"]);
}

/// The plugin addresses an ask by `text(uid)`: trimmed, a number as text. No spelling of an
/// existing uid may be filed beside it, or the owner's ask can no longer be answered.
#[test]
fn no_spelling_of_a_taken_uid_is_filed() {
    let m = machine();
    let path = m.path().join("ws/.control/asks/ledger.yaml");
    std::fs::write(&path, ledger(m.path()).replacen("  - id: seed-1\n", "  - id: seed-1\n    uid: \"7\"\n", 1))
        .unwrap();
    let before = ledger(m.path());
    for uid in [json!("7"), json!(" 7 "), json!(7), json!("seed-2")] {
        let mut v: Value = serde_json::from_str(&ask("n1")).unwrap();
        v["uid"] = uid.clone();
        let out = maestro(m.path())
            .args(["ask", "file", "--session", "t3:-/x", "--ledger", "ledger.yaml", "--ask", &v.to_string()])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("already has an ask"),
            "{uid}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(ledger(m.path()), before, "{uid}");
    }
}

/// Over the readers' 1 MiB limit no reader reads any ask in a ledger: such a filing is refused.
#[test]
fn a_filing_never_pushes_a_ledger_past_the_read_limit() {
    let m = machine();
    let before = ledger(m.path());
    let mut v: Value = serde_json::from_str(&ask("huge")).unwrap();
    v["notes"] = Value::String("x".repeat(1_100_000));
    let mut child = maestro(m.path())
        .args(["ask", "file", "--session", "t3:-/x", "--ledger", "ledger.yaml"])
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write as _;
        child.stdin.take().unwrap().write_all(v.to_string().as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("would be over 1048576 bytes"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(ledger(m.path()), before);
    assert_eq!(listed(m.path()).len(), 5);
}

#[test]
fn twenty_parallel_filers_lose_nothing() {
    let m = machine();
    let children: Vec<_> = (0..20)
        .map(|i| {
            maestro(m.path())
                .args([
                    "ask",
                    "file",
                    "--session",
                    &format!("t3:-/w{i}"),
                    "--ledger",
                    "ledger.yaml",
                    "--ask",
                    &ask(&format!("par-{i}")),
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let ids = listed(m.path());
    assert_eq!(ids.len(), 25, "{ids:?}");
    for i in 0..20 {
        assert_eq!(ids.iter().filter(|id| **id == format!("par-{i}")).count(), 1, "par-{i}");
    }
    let journal = std::fs::read_to_string(m.path().join("broomva-home/ledger/requests/requests.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 20);
    assert!(journal.lines().all(|l| serde_json::from_str::<Value>(l).is_ok()), "no torn journal line");
}

#[test]
fn other_requests_are_journalled_and_held_or_recorded() {
    let m = machine();
    let before = ledger(m.path());
    let runs: [&[&str]; 5] = [
        &["queue", "--title", "t", "--brief", "b"],
        &["action-request", "--action", "gh pr merge 1", "--reason", "r"],
        &["close", "--receipt", "PR 1 merged at abc"],
        &["handback", "--summary", "s", "--next", "n"],
        &["escalate", "--reason", "r"],
    ];
    for args in runs {
        let out = maestro(m.path()).env("MAESTRO_SESSION", "paseo:-/a1").arg("--json").args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        // Every caller is unverified until BRO-3268, so every one of these is held.
        assert_eq!(v["request"]["disposition"], "held", "{args:?}");
        assert_eq!(v["request"]["session"]["surface"], "paseo");
    }
    let journal = std::fs::read_to_string(m.path().join("broomva-home/ledger/requests/requests.jsonl")).unwrap();
    let kinds: Vec<String> = journal
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["request"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, ["work.queue", "action.request", "session.close", "handback", "escalate"]);
    assert_eq!(ledger(m.path()), before, "a request changes no ledger");
    let out = maestro(m.path()).args(["escalate", "--reason", "r"]).output().unwrap();
    assert!(!out.status.success(), "a request with no session is refused");
}

/// The agent CLI's whole command tree, from its own help: no subcommand is an owner verb.
#[test]
fn the_agent_cli_has_no_owner_verb() {
    fn subcommands(path: &[String]) -> Vec<String> {
        let out = Command::new(env!("CARGO_BIN_EXE_maestro")).args(path).arg("--help").output().unwrap();
        let help = String::from_utf8_lossy(&out.stdout).to_string();
        let Some(section) = help.split("Commands:\n").nth(1) else { return Vec::new() };
        section
            .lines()
            .take_while(|l| l.starts_with("  "))
            .filter_map(|l| l.split_whitespace().next())
            .filter(|c| *c != "help")
            .map(str::to_string)
            .collect()
    }
    let mut seen = Vec::new();
    let mut queue = vec![Vec::<String>::new()];
    while let Some(path) = queue.pop() {
        for sub in subcommands(&path) {
            let mut next = path.clone();
            next.push(sub.clone());
            seen.push(next.join(" "));
            queue.push(next);
        }
    }
    assert!(seen.iter().any(|c| c == "ask file"), "{seen:?}");
    let owner_words = [
        "answer",
        "approve",
        "allow",
        "deny",
        "dispatch",
        "verdict",
        "undo",
        "review",
        "decide",
        "send-back",
        "resolve",
        "withdraw",
        "archive",
        "default",
    ];
    for command in &seen {
        for word in owner_words {
            assert!(!command.split([' ', '-']).any(|w| w == word), "`maestro {command}` reads as an owner verb");
        }
    }
}
