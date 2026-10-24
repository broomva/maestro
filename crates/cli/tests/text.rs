//! The human text: an unread section prints `unavailable (<reason>)`, never a count (BRO-3322),
//! and `maestro hosts` says its capabilities are the surfaces', not Maestro's.

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
    rest.split_once(char::is_whitespace).map(|(m, r)| (m, r.trim_start())).unwrap()
}

#[test]
fn every_unread_section_prints_unavailable_and_its_reason_not_a_count() {
    let tmp = tempfile::tempdir().unwrap();
    // No git repository, no work root, no fleet state: every section is unread.
    std::fs::create_dir_all(tmp.path().join("ws")).unwrap();
    let text = stdout(maestro(tmp.path()).args(["--tree", "origin/main", "snapshot"]));
    for name in ["needs", "asks", "work", "loops", "initiatives", "fleet"] {
        let (mark, rest) = line(&text, name);
        assert_eq!(mark, "unreadable", "{name}: {text}");
        assert!(rest.starts_with("unavailable ("), "{name}: {rest}");
        for count in ["null", "open (", "registered", " live", " hold", " tick "] {
            assert!(!rest.contains(count), "{name} prints a count ({count}): {rest}");
        }
    }
    let (_, asks) = line(&text, "asks");
    assert!(asks.contains("did not resolve"), "the reason is the read error's: {asks}");
    let (_, work) = line(&text, "work");
    assert_eq!(work, "unavailable (No such file or directory (os error 2))");
}

#[test]
fn a_readable_section_still_prints_its_counts() {
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
fn an_unread_section_with_several_errors_names_the_first_and_counts_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("ws")).unwrap();
    let text = stdout(maestro(tmp.path()).args(["--tree", "origin/main", "snapshot"]));
    let (_, needs) = line(&text, "needs");
    assert!(needs.ends_with("; and 1 more, use --json)"), "{needs}");
}

#[test]
fn hosts_says_the_capabilities_are_not_maestros() {
    let tmp = tempfile::tempdir().unwrap();
    let text = stdout(maestro(tmp.path()).env("MAESTRO_NODE", "n1").arg("hosts"));
    assert!(text.contains("Maestro launches and sends nothing yet (BRO-3276)."), "{text}");
    let help = stdout(maestro(tmp.path()).args(["hosts", "--help"]));
    assert!(help.contains("not what Maestro does with it"), "{help}");
}
