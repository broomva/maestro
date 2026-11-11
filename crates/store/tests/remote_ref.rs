//! `tree::remote_ref` and its parser (BRO-3635): where a remote says a ref is, bounded in time and
//! output, taken only for exactly that ref, and only as a well-formed object id.

use maestro_store::tree::{
    MAX_LS_REMOTE, check_root_owned, git, git_for, git_path, ls_remote_command, parse_ls_remote, remote_ref,
};
use std::time::{Duration, Instant};

const A: &str = "1111111111111111111111111111111111111111";
const B: &str = "2222222222222222222222222222222222222222";

#[test]
fn only_the_exact_ref_counts() {
    // ls-remote's patterns match by suffix, so it prints refs/heads/x/refs/heads/main too.
    let out = format!("{A}\trefs/heads/x/refs/heads/main\n{B}\trefs/heads/main\n");
    assert_eq!(parse_ls_remote(out.as_bytes(), "refs/heads/main").unwrap().as_deref(), Some(B));
    assert_eq!(parse_ls_remote(format!("{A}\tHEAD\n").as_bytes(), "refs/heads/main").unwrap(), None);
    let sha256 = "3".repeat(64);
    assert_eq!(
        parse_ls_remote(format!("{sha256}\trefs/heads/main\n").as_bytes(), "refs/heads/main").unwrap(),
        Some(sha256)
    );
}

#[test]
fn a_second_line_for_the_ref_or_a_malformed_id_is_refused() {
    let twice = format!("{A}\trefs/heads/main\n{B}\trefs/heads/main\n");
    assert!(parse_ls_remote(twice.as_bytes(), "refs/heads/main").is_err());
    for bad in ["123", &"g".repeat(40), &"A".repeat(40), &"1".repeat(41)] {
        let out = format!("{bad}\trefs/heads/main\n");
        assert!(parse_ls_remote(out.as_bytes(), "refs/heads/main").is_err(), "{bad}");
    }
}

#[test]
fn output_over_the_bound_is_refused() {
    let mut out = format!("{A}\trefs/heads/main\n").into_bytes();
    out.extend(vec![b'#'; MAX_LS_REMOTE]);
    assert!(parse_ls_remote(&out, "refs/heads/main").is_err());
}

#[test]
fn a_remote_that_hangs_is_given_up_at_the_timeout() {
    // A git:// server that accepts and never says a word.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let held = listener.accept();
        std::thread::sleep(Duration::from_secs(30));
        drop(held);
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let cmd = ls_remote_command(&format!("git://127.0.0.1:{port}/x"), "refs/heads/main", &[]);
        let _ = tx.send(remote_ref(cmd, "refs/heads/main", Duration::from_secs(1)));
    });
    let got = rx.recv_timeout(Duration::from_secs(15)).expect("remote_ref never gave up");
    let e = got.unwrap_err();
    assert!(e.contains("did not answer"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn the_ls_remote_command_runs_outside_any_repository_with_only_its_own_configuration() {
    let cmd = ls_remote_command("https://example.com/r.git", "refs/heads/main", &["a.b=c", "d.e="]);
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(args, ["-c", "a.b=c", "-c", "d.e=", "ls-remote", "--", "https://example.com/r.git", "refs/heads/main"]);
    assert_eq!(cmd.get_current_dir(), Some(std::path::Path::new("/")));
    // The environment is cleared (`env_clear`) and holds exactly these: PATH and HOME from the
    // caller's (both are set in a test run), and the three that keep git's own configuration out.
    let mut env: Vec<(String, Option<String>)> = cmd
        .get_envs()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
        .collect();
    env.sort();
    let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        ["GIT_CONFIG_GLOBAL", "GIT_CONFIG_NOSYSTEM", "GIT_TERMINAL_PROMPT", "HOME", "PATH", "xcrun_nocache"]
    );
    let value = |k: &str| env.iter().find(|(key, _)| key == k).and_then(|(_, v)| v.clone());
    // The platform's git, by absolute path, on a fixed PATH: none earlier in the caller's stands in.
    assert_eq!(cmd.get_program(), git());
    assert_eq!(value("PATH").as_deref(), Some(git_path()));
    assert_eq!(value("xcrun_nocache").as_deref(), Some("1"));
    assert_eq!(value("GIT_CONFIG_GLOBAL").as_deref(), Some("/dev/null"));
    assert_eq!(value("GIT_CONFIG_NOSYSTEM").as_deref(), Some("1"));
    assert_eq!(value("GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
    assert_eq!(value("HOME"), std::env::var("HOME").ok());
}

#[test]
fn output_past_the_bound_is_refused_and_the_reader_stops_at_it() {
    // A bare repository whose refs ending in refs/heads/main (ls-remote's patterns match by
    // suffix) say far more than MAX_LS_REMOTE.
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("r.git");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@localhost"])
            .arg("-C")
            .arg(&bare)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    std::fs::create_dir(&bare).unwrap();
    git(&["init", "-q", "--bare"]);
    let tree = git(&["hash-object", "-t", "tree", "-w", "/dev/null"]);
    let commit = git(&["commit-tree", &tree, "-m", "x"]);
    let refs: String = (0..200).map(|i| format!("{commit} refs/heads/x{i:03}/refs/heads/main\n")).collect();
    std::fs::write(bare.join("packed-refs"), format!("{refs}{commit} refs/heads/main\n")).unwrap();
    let cmd = ls_remote_command(bare.to_str().unwrap(), "refs/heads/main", &[]);
    let e = remote_ref(cmd, "refs/heads/main", Duration::from_secs(20)).unwrap_err();
    assert!(e.contains(&format!("read {}, then stopped", MAX_LS_REMOTE + 1)), "{e}");
}

#[test]
fn macos_runs_the_command_line_tools_git_and_linux_the_systems() {
    // On macOS /usr/bin/git is an xcrun launcher that finds git through a user-writable cache.
    assert_eq!(git_for("macos"), "/Library/Developer/CommandLineTools/usr/bin/git");
    assert_eq!(git_for("linux"), "/usr/bin/git");
    assert_eq!(git(), git_for(std::env::consts::OS));
}

#[test]
fn only_a_binary_root_owns_alone_up_to_the_root_passes() {
    // The platform's git does (the hosts this runs on have it, root-owned).
    check_root_owned(std::path::Path::new(git())).unwrap();
    // A file this user owns does not.
    let mine = tempfile::NamedTempFile::new().unwrap();
    let e = check_root_owned(mine.path()).unwrap_err();
    assert!(e.contains("not root"), "{e}");
    // A root-owned folder others may write (/tmp, mode 1777, where it really is: /private/tmp on
    // macOS, whose /tmp is a link) does not.
    let e = check_root_owned(&std::fs::canonicalize("/tmp").unwrap()).unwrap_err();
    assert!(e.contains("writable by its group or others"), "{e}");
}

#[test]
fn every_folder_above_the_binary_is_checked_not_only_the_binary() {
    // A hard link to the platform's git (root-owned) in a folder this user owns: the file itself
    // passes, so only checking the folders above it refuses it.
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("git");
    if let Err(e) = std::fs::hard_link(git(), &link) {
        eprintln!("skipped: {} cannot be hard-linked into {} ({e})", git(), dir.path().display());
        return;
    }
    let e = check_root_owned(&link).unwrap_err();
    assert!(e.contains(&format!("{} is owned by uid", dir.path().display())), "{e}");
}

#[test]
fn a_symbolic_link_anywhere_on_the_path_is_refused() {
    // A link this user makes to the platform's git: refused as a link, never followed.
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("git");
    std::os::unix::fs::symlink(git(), &link).unwrap();
    let e = check_root_owned(&link).unwrap_err();
    assert!(e.contains("is a symbolic link"), "{e}");
    assert!(check_root_owned(std::path::Path::new("git")).is_err(), "a relative path is refused");
}

#[test]
fn a_folder_its_group_may_write_is_refused() {
    // A root-owned folder whose group (not others) may write, under root-owned folders: macOS's
    // /Library/Fonts (root:admin 1775), Debian's /var/mail or /var/local (2775). Skipped where none is.
    use std::os::unix::fs::MetadataExt as _;
    let found = ["/Library/Fonts", "/var/mail", "/var/local"].into_iter().map(std::path::Path::new).find(|p| {
        std::fs::symlink_metadata(p)
            .is_ok_and(|m| m.is_dir() && m.uid() == 0 && m.mode() & 0o020 != 0 && m.mode() & 0o002 == 0)
            && p.parent().is_some_and(|up| check_root_owned(up).is_ok())
    });
    let Some(dir) = found else {
        eprintln!("skipped: no root-owned, group-writable folder under root-owned folders here");
        return;
    };
    let e = check_root_owned(dir).unwrap_err();
    assert!(e.contains(&format!("{} is writable by its group or others", dir.display())), "{e}");
}

const LS_REMOTE_CHILD: &str = "MAESTRO_STORE_TEST_LS_REMOTE_CHILD";

/// Not a test: in a child process run from another folder (`ls_remote_ignores_the_repository_it_is_run_from`),
/// asks `$MAESTRO_STORE_TEST_LS_REMOTE_CHILD` where its main is. A no-op in a normal test run.
#[test]
fn ls_remote_child_entry() {
    let Ok(url) = std::env::var(LS_REMOTE_CHILD) else { return };
    let got = remote_ref(ls_remote_command(&url, "refs/heads/main", &[]), "refs/heads/main", Duration::from_secs(20));
    println!("LS_REMOTE_RESULT {got:?}");
}

#[test]
fn ls_remote_ignores_the_repository_it_is_run_from() {
    // A repository whose own configuration sends the answers remote elsewhere (insteadOf to an
    // evil remote, and origin's uploadpack), as the folder the process runs in: git, asked from
    // there, would ask the evil remote. ls_remote_command runs it in `/`, so the genuine main
    // comes back.
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let run = |dir: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@localhost", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let (genuine, evil, work) = (base.join("genuine.git"), base.join("evil.git"), base.join("work"));
    for bare in [&genuine, &evil] {
        run(base, &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
    }
    std::fs::create_dir(&work).unwrap();
    run(&work, &["init", "-q", "-b", "main"]);
    run(&work, &["commit", "-q", "--allow-empty", "-m", "genuine"]);
    let genuine_main = run(&work, &["rev-parse", "HEAD"]);
    run(&work, &["push", "-q", genuine.to_str().unwrap(), "main"]);
    run(&work, &["commit", "-q", "--allow-empty", "-m", "evil"]);
    let evil_main = run(&work, &["rev-parse", "HEAD"]);
    run(&work, &["push", "-q", evil.to_str().unwrap(), "main"]);
    let (g, e) = (genuine.to_str().unwrap(), evil.to_str().unwrap());
    run(&work, &["remote", "add", "origin", g]);
    run(&work, &["config", &format!("url.{e}.insteadOf"), g]);
    run(&work, &["config", "remote.origin.uploadpack", &format!("git-upload-pack {e}")]);
    let control = run(&work, &["ls-remote", g, "refs/heads/main"]);
    assert!(control.starts_with(&evil_main), "the control: from there, plain git asks the evil remote: {control}");
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["ls_remote_child_entry", "--exact", "--nocapture", "--test-threads=1"])
        .env(LS_REMOTE_CHILD, g)
        .current_dir(&work)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let said = text
        .lines()
        .find_map(|l| l.split_once("LS_REMOTE_RESULT ").map(|(_, r)| r.trim().to_string()))
        .unwrap_or_default();
    assert_eq!(said, format!("Ok(Some({genuine_main:?}))"), "{text}");
}
