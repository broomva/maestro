//! The human text: an unread section prints `unavailable (<where>: <why>)`, never a count, and a
//! partial one says its counts come from a partial read, claiming no bound (BRO-3322);
//! `maestro hosts` says its capabilities are the surfaces', not Maestro's.

use std::path::Path;
use std::process::Command;

/// The binary with a clean, scratch environment rooted at `dir`.
fn maestro(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_maestro"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.join("home"))
        .env("MAESTRO_WORK_ROOT", dir.join("work"))
        .env("MAESTRO_FLEET_STATE", dir.join("fleet"))
        .arg("--workspace")
        .arg(dir.join("ws"));
    cmd
}

fn stdout(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// The snapshot line of one section: its status and the rest.
fn line<'a>(text: &'a str, name: &str) -> (&'a str, &'a str) {
    let l = text.lines().find(|l| l.split_whitespace().next() == Some(name)).unwrap_or_else(|| panic!("{text}"));
    let rest = l.trim_start()[name.len()..].trim_start();
    rest.split_once(char::is_whitespace).map(|(m, r)| (m, r.trim_start())).unwrap_or((rest, ""))
}

/// No git repository, no work root, no fleet state: every section is unread.
fn unread(dir: &Path) -> Command {
    std::fs::create_dir_all(dir.join("ws")).unwrap();
    let mut cmd = maestro(dir);
    cmd.args(["--tree", "origin/main"]);
    cmd
}

/// One ledger that reads and one that does not, a fleet state and no work root.
fn partial(dir: &Path) -> Command {
    let asks = dir.join("ws/.control/asks");
    std::fs::create_dir_all(&asks).unwrap();
    std::fs::write(asks.join("a.yaml"), "asks:\n  - id: one\n").unwrap();
    std::fs::write(asks.join("b.yaml"), "asks: [unclosed\n").unwrap();
    std::fs::create_dir_all(dir.join("fleet")).unwrap();
    let mut cmd = maestro(dir);
    cmd.args(["--tree", "worktree"]);
    cmd
}

#[test]
fn every_unread_section_prints_unavailable_and_its_reason_not_a_count() {
    let tmp = tempfile::tempdir().unwrap();
    let text = stdout(unread(tmp.path()).arg("snapshot"));
    for name in ["needs", "asks", "work", "loops", "initiatives", "fleet"] {
        let (mark, rest) = line(&text, name);
        assert_eq!(mark, "unreadable", "{name}: {text}");
        assert!(rest.starts_with("unavailable ("), "{name}: {rest}");
        for count in ["null", "open (", "registered", " live", " hold", " tick "] {
            assert!(!rest.contains(count), "{name} prints a count ({count}): {rest}");
        }
    }
    let (_, asks) = line(&text, "asks");
    assert!(asks.starts_with("unavailable (git:origin/main@unresolved:asks: revision"), "where, then why: {asks}");
    let (_, work) = line(&text, "work");
    let root = tmp.path().join("work");
    assert!(work.starts_with(&format!("unavailable (fs:{}: ", root.display())), "{work}");
}

#[test]
fn an_unread_section_with_several_errors_names_the_first_and_counts_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let text = stdout(unread(tmp.path()).arg("snapshot"));
    let (_, needs) = line(&text, "needs");
    assert!(needs.ends_with("; and 1 more, use --json)"), "{needs}");
}

#[test]
fn a_readable_section_prints_its_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let asks = tmp.path().join("ws/.control/asks");
    std::fs::create_dir_all(&asks).unwrap();
    std::fs::write(asks.join("a.yaml"), "asks:\n  - id: one\n").unwrap();
    std::fs::create_dir_all(tmp.path().join("fleet")).unwrap();
    let text = stdout(maestro(tmp.path()).args(["--tree", "worktree", "snapshot"]));
    let (mark, rest) = line(&text, "asks");
    assert_eq!(mark, "ok", "{text}");
    assert!(rest.starts_with("1 open ("), "{rest}");
    assert_eq!(line(&text, "fleet"), ("ok", ""));
}

#[test]
fn a_partial_section_says_its_counts_come_from_a_partial_read() {
    let tmp = tempfile::tempdir().unwrap();
    let text = stdout(partial(tmp.path()).arg("snapshot"));
    let (mark, rest) = line(&text, "asks");
    assert_eq!(mark, "partial", "{text}");
    assert!(rest.starts_with("1 open ("), "{rest}");
    assert!(rest.contains(" ledgers, from a partial read (.control/asks/b.yaml: outside the YAML subset"), "{rest}");
    let (mark, rest) = line(&text, "needs");
    assert_eq!(mark, "partial", "{text}");
    assert!(rest.starts_with("0 holds on the owner, from a partial read ("), "{rest}");
    assert!(!text.contains("at least"), "a partial read claims no bound: {text}");
}

#[test]
fn the_asks_and_work_commands_print_unavailable_or_their_partial_read() {
    let tmp = tempfile::tempdir().unwrap();
    let asks = stdout(partial(tmp.path()).arg("asks"));
    assert!(asks.starts_with("1 ask, from a partial read (.control/asks/b.yaml: "), "{asks}");
    let work = stdout(partial(tmp.path()).arg("work"));
    assert!(work.starts_with("work: unavailable (fs:"), "{work}");
    let other = tempfile::tempdir().unwrap();
    let asks = stdout(unread(other.path()).arg("asks"));
    assert!(asks.starts_with("asks: unavailable (git:origin/main@unresolved:asks: revision"), "{asks}");
    let needs = stdout(unread(other.path()).arg("needs"));
    assert!(needs.contains("Needs you: unavailable (git:origin/main@unresolved:asks: revision"), "{needs}");
}

#[test]
fn hosts_says_the_capabilities_are_not_maestros() {
    let tmp = tempfile::tempdir().unwrap();
    let text = stdout(maestro(tmp.path()).env("MAESTRO_NODE", "n1").arg("hosts"));
    assert!(text.contains("Maestro launches and sends nothing yet (BRO-3276)."), "{text}");
    let help = stdout(maestro(tmp.path()).args(["hosts", "--help"]));
    assert!(
        help.contains("not what Maestro does with it: Maestro launches and sends nothing until its host"),
        "{help}"
    );
}
