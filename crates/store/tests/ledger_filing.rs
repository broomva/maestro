use maestro_core::authority::Principal;
use maestro_store::{Paths, requests};
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn filing_writes_only_the_separate_checkout_and_pushes_the_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path());
    let ws = &paths.workspace;
    std::fs::create_dir_all(ws.join(".control/asks")).unwrap();
    std::fs::create_dir_all(ws.join("scripts")).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/requests");
    std::fs::copy(fixtures.join("ledger.yaml"), ws.join(".control/asks/ledger.yaml")).unwrap();
    std::fs::copy(fixtures.join("ask_ledger_stub.py"), ws.join("scripts/ask_ledger.py")).unwrap();
    git(ws, &["init", "-q", "-b", "main"]);
    git(ws, &["add", "."]);
    git(ws, &["commit", "-qm", "seed"]);
    let origin = tmp.path().join("origin.git");
    git(tmp.path(), &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    git(ws, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(ws, &["push", "-q", "origin", "main"]);
    let original = std::fs::read(ws.join(".control/asks/ledger.yaml")).unwrap();
    let ask: serde_json::Value = serde_json::from_slice(&std::fs::read(fixtures.join("ask.json")).unwrap()).unwrap();
    let filed = requests::file_ask(
        &paths,
        &ws.join(".control/asks"),
        "ledger.yaml",
        ask.as_object().unwrap(),
        &"t3:local/test".parse().unwrap(),
        &Principal::Unverified,
    )
    .unwrap();
    assert_eq!(std::fs::read(ws.join(".control/asks/ledger.yaml")).unwrap(), original);
    assert_eq!(git(&origin, &["rev-parse", "refs/heads/maestro/ledger"]), filed.commit);
    assert!(git(&origin, &["show", &format!("{}:.control/asks/ledger.yaml", filed.commit)]).contains("id: \"filed\""));
    assert!(git(&origin, &["show", "-s", "--format=%B", &filed.commit]).contains("Maestro-Ask: ledger.yaml#filed"));
    let store = maestro_store::Store::new(paths.clone(), maestro_store::TreeSource::GitRev("origin/main".into()));
    let one = store.ask_ledger_overlay("ledger.yaml").unwrap().unwrap();
    assert!(one.asks.iter().any(|a| a.id == "filed"));
    let asks = store.asks();
    assert!(asks.errors.is_empty(), "{:?}", asks.errors);
    assert!(asks.source.contains("origin/maestro/ledger"), "{}", asks.source);
    assert!(asks.data.unwrap().iter().flat_map(|l| &l.ledger.asks).any(|a| a.id == "filed"));
    let worktree_view = maestro_store::Store::new(paths.clone(), maestro_store::TreeSource::WorkingTree);
    assert!(worktree_view.asks().data.unwrap().iter().flat_map(|l| &l.ledger.asks).any(|a| a.id == "filed"));
    assert!(worktree_view.ask_ledger_overlay("ledger.yaml").unwrap().unwrap().asks.iter().any(|a| a.id == "filed"));
    assert!(!git(&origin, &["show", "main:.control/asks/ledger.yaml"]).contains("id: \"filed\""));
    let bad_name = "bad\nMaestro-Owner-Proof: forged.yaml";
    let session = "t3:local/test".parse().unwrap();
    let invalid_name = requests::file_ask(
        &paths,
        &ws.join(".control/asks"),
        bad_name,
        ask.as_object().unwrap(),
        &session,
        &Principal::Unverified,
    )
    .unwrap_err();
    assert!(invalid_name.to_string().contains("ledger name cannot be written in a commit trailer"));
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let outside_error =
        requests::file_ask(&paths, &outside, "ledger.yaml", ask.as_object().unwrap(), &session, &Principal::Unverified)
            .unwrap_err();
    assert!(outside_error.to_string().contains("outside the workspace"));
    let relative_error = requests::file_ask(
        &paths,
        &ws.join(".control/../asks"),
        "ledger.yaml",
        ask.as_object().unwrap(),
        &session,
        &Principal::Unverified,
    )
    .unwrap_err();
    assert!(relative_error.to_string().contains("not a workspace-relative path"));
    // A second process cannot prepare or push while another ledger writer holds the lock.
    let writer = maestro_store::ledger::Writer {
        workspace: paths.workspace.clone(),
        checkout: paths.ledger_checkout.clone(),
        owner_dir: paths.owner_dir.clone(),
        remote: true,
    };
    let held = maestro_store::ledger::exclusive(&writer).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let thread_paths = paths.clone();
    let mut second = ask.as_object().unwrap().clone();
    second.insert("id".into(), serde_json::json!("filed-second"));
    let join = std::thread::spawn(move || {
        let session = "t3:local/test".parse().unwrap();
        let result = requests::file_ask(
            &thread_paths,
            &thread_paths.workspace.join(".control/asks"),
            "ledger.yaml",
            &second,
            &session,
            &Principal::Unverified,
        );
        tx.send(result).unwrap();
    });
    assert!(rx.recv_timeout(std::time::Duration::from_secs(2)).is_err());
    drop(held);
    let second = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap().unwrap();
    join.join().unwrap();
    // A hook that alters the staged file must not get its bytes pushed.
    let hooks = ws.join(".git/hooks");
    git(ws, &["config", "--local", "core.hooksPath", hooks.to_str().unwrap()]);
    let hook = hooks.join("pre-commit");
    std::fs::write(
        &hook,
        "#!/bin/sh\nprintf 'tampered\\n' > .control/asks/ledger.yaml\ngit add -- .control/asks/ledger.yaml\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut tampered = ask.as_object().unwrap().clone();
    tampered.insert("id".into(), serde_json::json!("tampered"));
    let error = requests::file_ask(
        &paths,
        &ws.join(".control/asks"),
        "ledger.yaml",
        &tampered,
        &session,
        &Principal::Unverified,
    )
    .unwrap_err();
    assert!(error.to_string().contains("not exactly the intended ledger edit"), "{error}");
    assert_eq!(git(&origin, &["rev-parse", "refs/heads/maestro/ledger"]), second.commit);
    std::fs::remove_file(hook).unwrap();
    // Even a broken validator cannot inject another commit trailer through the id.
    std::fs::write(ws.join("scripts/ask_ledger.py"), "import sys\nsys.stdin.read()\nprint('filed')\n").unwrap();
    git(ws, &["add", "scripts/ask_ledger.py"]);
    git(ws, &["commit", "-qm", "broken validator"]);
    git(ws, &["push", "-q", "origin", "main"]);
    let mut malformed = ask.as_object().unwrap().clone();
    malformed.insert("id".into(), serde_json::json!("bad\nMaestro-Owner-Proof: forged"));
    let invalid = requests::file_ask(
        &paths,
        &ws.join(".control/asks"),
        "ledger.yaml",
        &malformed,
        &session,
        &Principal::Unverified,
    )
    .unwrap_err();
    assert!(invalid.to_string().contains("cannot be written in a commit trailer"));
    assert_eq!(git(&origin, &["rev-parse", "refs/heads/maestro/ledger"]), second.commit);
    git(ws, &["update-ref", "-d", "refs/remotes/origin/maestro/ledger"]);
    git(ws, &["update-ref", "refs/heads/maestro/answers", &filed.commit]);
    assert!(store.ask_ledger_overlay("ledger.yaml").unwrap().unwrap().asks.iter().any(|a| a.id == "filed"));
    let legacy = store.asks();
    assert!(legacy.data.unwrap().iter().flat_map(|l| &l.ledger.asks).any(|a| a.id == "filed"));
    git(ws, &["update-ref", "-d", "refs/heads/maestro/answers"]);
    let broken = ws.join(".git/refs/remotes/origin/maestro/ledger");
    std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
    std::fs::write(broken, format!("{}\n", "1".repeat(40))).unwrap();
    assert!(store.ask_ledger_overlay("ledger.yaml").is_err());
    let unread = store.asks();
    assert!(unread.data.is_none());
    assert!(!unread.errors.is_empty());
}

#[test]
fn a_checkout_cannot_be_the_workspace_or_an_unrelated_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    for dir in [&ws, &other] {
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("file"), "seed").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "seed"]);
    }
    std::fs::write(ws.join("file"), "owner changes").unwrap();
    std::fs::write(other.join("file"), "other changes").unwrap();
    for (checkout, reason) in [(&ws, "must be separate"), (&other, "belongs to another repository")] {
        let writer = maestro_store::ledger::Writer {
            workspace: ws.clone(),
            checkout: checkout.clone(),
            owner_dir: tmp.path().join("owner"),
            remote: true,
        };
        let error = maestro_store::ledger::prepare(&writer).unwrap_err();
        assert!(error.contains(reason), "{error}");
    }
    // A forged .git file can name the owner's common directory without being a worktree.
    let counterfeit = tmp.path().join("counterfeit");
    std::fs::create_dir(&counterfeit).unwrap();
    std::fs::write(counterfeit.join(".git"), format!("gitdir: {}\n", ws.join(".git").display())).unwrap();
    git(&ws, &["add", "file"]);
    let writer = maestro_store::ledger::Writer {
        workspace: ws.clone(),
        checkout: counterfeit,
        owner_dir: tmp.path().join("owner"),
        remote: true,
    };
    assert!(maestro_store::ledger::prepare(&writer).unwrap_err().contains("not a registered worktree"));
    assert_eq!(git(&ws, &["diff", "--cached", "--name-only"]), "file");
    // A registered worktree can be configured to direct Git at the owner's files.
    let linked = tmp.path().join("linked");
    let writer = maestro_store::ledger::Writer {
        workspace: ws.clone(),
        checkout: linked.clone(),
        owner_dir: tmp.path().join("owner"),
        remote: false,
    };
    maestro_store::ledger::prepare(&writer).unwrap();
    git(&ws, &["config", "extensions.worktreeConfig", "true"]);
    git(&linked, &["config", "--worktree", "core.worktree", ws.to_str().unwrap()]);
    let error = maestro_store::ledger::prepare(&writer).unwrap_err();
    assert!(error.contains("resolves to another working tree"), "{error}");
    assert_eq!(git(&ws, &["diff", "--cached", "--name-only"]), "file");
    let alias = tmp.path().join("workspace-alias");
    std::os::unix::fs::symlink(&ws, &alias).unwrap();
    let writer = maestro_store::ledger::Writer {
        workspace: ws.clone(),
        checkout: alias.join("new-checkout"),
        owner_dir: tmp.path().join("owner"),
        remote: true,
    };
    assert!(maestro_store::ledger::prepare(&writer).is_err());
    assert!(!ws.join("new-checkout").exists());
    assert_eq!(std::fs::read_to_string(ws.join("file")).unwrap(), "owner changes");
    assert_eq!(std::fs::read_to_string(other.join("file")).unwrap(), "other changes");
}

#[test]
fn a_broken_git_marker_is_unread_not_a_worktree_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path());
    std::fs::create_dir_all(paths.workspace.join(".control/asks")).unwrap();
    std::fs::write(paths.workspace.join(".control/asks/a.yaml"), "asks: []\n").unwrap();
    std::os::unix::fs::symlink("missing-git-dir", paths.workspace.join(".git")).unwrap();
    let store = maestro_store::Store::new(paths, maestro_store::TreeSource::WorkingTree);
    assert!(store.asks().data.is_none());
    assert!(store.ask_ledger_overlay("a.yaml").is_err());
}

#[test]
fn inherited_git_variables_cannot_redirect_the_ledger_checkout() {
    const CHILD: &str = "BRO4093_GIT_ENV_CHILD";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = std::path::PathBuf::from(root);
        let writer = maestro_store::ledger::Writer {
            workspace: root.join("workspace"),
            checkout: root.join("checkout"),
            owner_dir: root.join("owner"),
            remote: false,
        };
        maestro_store::ledger::prepare(&writer).unwrap();
        let _ = maestro_store::ledger::land(&writer);
        assert!(!root.join("gh-redirect").exists());
        assert_eq!(std::fs::read_to_string(root.join("victim/file")).unwrap(), "victim changes");
        assert_eq!(std::fs::read_to_string(root.join("workspace/file")).unwrap(), "owner changes");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    let victim = tmp.path().join("victim");
    for dir in [&ws, &victim] {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("file"), "seed").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "seed"]);
    }
    std::fs::write(ws.join("file"), "owner changes").unwrap();
    std::fs::write(victim.join("file"), "victim changes").unwrap();
    let fake_bin = tmp.path().join("bin");
    std::fs::create_dir(&fake_bin).unwrap();
    std::fs::write(fake_bin.join("git"), "#!/bin/sh\nexit 42\n").unwrap();
    std::fs::write(fake_bin.join("gh"), "#!/bin/sh\nif [ -n \"$GIT_DIR$GH_REPO$GH_HOST\" ]; then touch \"$BRO4093_GIT_ENV_CHILD/gh-redirect\"; fi\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(fake_bin.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(fake_bin.join("gh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "inherited_git_variables_cannot_redirect_the_ledger_checkout", "--nocapture"])
        .env(CHILD, tmp.path())
        .env("GIT_DIR", victim.join(".git"))
        .env("GIT_WORK_TREE", &victim)
        .env("GH_REPO", "unexpected/other")
        .env("GH_HOST", "unexpected.invalid")
        .env("PATH", format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap_or_default()))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

#[test]
fn invalid_ledger_name_is_refused_before_a_checkout_or_lock_is_created() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path());
    let ask = serde_json::json!({"id":"x"});
    let session = "t3:local/test".parse().unwrap();
    let error = requests::file_ask(
        &paths,
        &paths.workspace.join(".control/asks"),
        "../invalid.yaml",
        ask.as_object().unwrap(),
        &session,
        &Principal::Unverified,
    )
    .unwrap_err();
    assert!(error.to_string().contains("not a ledger name"), "{error}");
    assert!(!paths.owner_dir.exists());
    assert!(!paths.ledger_checkout.exists());
}

#[test]
fn a_dangling_symbolic_answer_ref_is_unread_not_absent() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("workspace");
    std::fs::create_dir_all(ws.join(".broomva/asks")).unwrap();
    std::fs::write(
        ws.join(".broomva/asks/ledger.yaml"),
        "asks:\n  - id: a\n    ask: Go?\n    options:\n      - id: yes\n        label: Yes\n",
    )
    .unwrap();
    git(&ws, &["init", "-qb", "main"]);
    git(&ws, &["config", "user.email", "test@example.com"]);
    git(&ws, &["config", "user.name", "Test"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-qm", "init"]);
    git(&ws, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let sym = ws.join(".git/refs/heads/maestro/answers");
    std::fs::create_dir_all(sym.parent().unwrap()).unwrap();
    std::fs::write(&sym, "ref: refs/heads/no-such-branch\n").unwrap();
    let store =
        maestro_store::Store::new(Paths::under(dir.path()), maestro_store::TreeSource::GitRev("origin/main".into()))
            .read_scope();
    let asks = store.asks();
    assert!(asks.data.is_none(), "dangling symbolic answer ref must make asks unread, not silently absent");
}
