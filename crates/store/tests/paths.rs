//! The path registry in Maestro (BRO-3266): `.broomva/<name>` when the tree has it,
//! `.control/<name>` otherwise, both an error, on disk and in a git revision; the
//! `$BROOMVA_HOME` layout by kind; and the stale-reader self-check.

use maestro_store::broomva_paths::{self as registry, REGISTRY_SHA256};
use maestro_store::doctor::{self, Status, VENDORED_READER};
use maestro_store::{Paths, Store, TreeSource, UNFILED, WorkLayout};
use std::path::{Path, PathBuf};
use std::process::Command;

const LEDGER: &str = "asks:\n  - id: a\n";
const INITIATIVE: &str =
    "id: demo\ntitle: Demo\nintent: x\nstated: 2026-10-01\nstatus: active\nnext_action: x\ndriver: owner\n";

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@localhost", "-c", "core.hooksPath=/dev/null"])
        .args(args)
        .output()
        .unwrap();
    assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
}

/// A machine under `root` whose workspace holds `files` (path, contents) and is a git repo with them committed.
fn machine(root: &Path, files: &[(&str, &str)]) -> Paths {
    let paths = Paths::under(root);
    std::fs::create_dir_all(&paths.workspace).unwrap();
    for (rel, text) in files {
        let file = paths.workspace.join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    git(&paths.workspace, &["init", "-q", "-b", "main"]);
    git(&paths.workspace, &["add", "-A"]);
    git(&paths.workspace, &["commit", "-q", "--allow-empty", "-m", "fixture"]);
    paths
}

fn both_sources(paths: &Paths) -> [Store; 2] {
    [Store::new(paths.clone(), TreeSource::WorkingTree), Store::new(paths.clone(), TreeSource::GitRev("main".into()))]
}

fn ask_ids(store: &Store) -> Vec<String> {
    let section = store.asks();
    section.data.unwrap_or_default().iter().flat_map(|l| l.ledger.asks.iter().map(|a| a.id.clone())).collect()
}

#[test]
fn control_only_reads_control() {
    let tmp = tempfile::tempdir().unwrap();
    let paths =
        machine(tmp.path(), &[(".control/asks/a.yaml", LEDGER), (".control/initiatives/demo.yaml", INITIATIVE)]);
    for store in both_sources(&paths) {
        assert_eq!(store.rel("asks").unwrap(), ".control/asks", "{}", store.tree.source);
        let asks = store.asks();
        assert!(asks.errors.is_empty(), "{:?}", asks.errors);
        assert!(asks.source.ends_with(":.control/asks"), "{}", asks.source);
        assert_eq!(ask_ids(&store), ["a"]);
        assert_eq!(store.initiatives().data.unwrap().len(), 1);
    }
}

#[test]
fn broomva_only_reads_broomva() {
    let tmp = tempfile::tempdir().unwrap();
    let paths =
        machine(tmp.path(), &[(".broomva/asks/a.yaml", LEDGER), (".broomva/initiatives/demo.yaml", INITIATIVE)]);
    for store in both_sources(&paths) {
        assert_eq!(store.rel("asks").unwrap(), ".broomva/asks", "{}", store.tree.source);
        assert!(store.asks().source.ends_with(":.broomva/asks"));
        assert_eq!(ask_ids(&store), ["a"]);
        assert_eq!(store.initiatives().data.unwrap().len(), 1);
    }
}

#[test]
fn both_is_a_read_error_never_a_preference() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = machine(
        tmp.path(),
        &[
            (".broomva/asks/a.yaml", LEDGER),
            (".control/asks/b.yaml", "asks:\n  - id: b\n"),
            (".control/initiatives/demo.yaml", INITIATIVE),
        ],
    );
    for store in both_sources(&paths) {
        let asks = store.asks();
        assert!(asks.data.is_none(), "unread is not empty: {}", store.tree.source);
        assert!(asks.errors[0].reason.contains("both .broomva/asks and .control/asks"), "{:?}", asks.errors);
        // Only the name in both places is refused; the rest of the tree still reads.
        assert_eq!(store.initiatives().data.unwrap().len(), 1);
    }
    // Answers waiting to land are unknown when the working tree holds both: not zero.
    let at_rev = Store::new(paths.clone(), TreeSource::GitRev("main".into()));
    let unlanded = maestro_store::views::unlanded(&at_rev, &[]);
    assert!(unlanded.data.is_none() && !unlanded.errors.is_empty());
}

#[test]
fn neither_is_unread_at_the_new_location() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = machine(tmp.path(), &[("README.md", "x")]);
    for store in both_sources(&paths) {
        let asks = store.asks();
        assert!(asks.data.is_none(), "an absent folder is unread, not empty: {}", store.tree.source);
        assert_eq!(store.rel("asks").unwrap(), ".broomva/asks");
        assert!(store.preauth().data.is_none());
    }
}

#[test]
fn a_moved_file_in_git_is_read_from_the_new_place() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = machine(tmp.path(), &[(".control/preauth.yaml", "grants: []\n")]);
    git(&paths.workspace, &["mv", ".control", ".broomva"]);
    git(&paths.workspace, &["commit", "-q", "-m", "move"]);
    let at_main = Store::new(paths.clone(), TreeSource::GitRev("main".into()));
    let at_before = Store::new(paths, TreeSource::GitRev("main~1".into()));
    assert_eq!(at_main.rel("preauth").unwrap(), ".broomva/preauth.yaml");
    assert_eq!(at_before.rel("preauth").unwrap(), ".control/preauth.yaml");
    assert!(at_main.preauth().data.is_some() && at_before.preauth().data.is_some());
}

/// Maestro reads a git revision with one `ls-tree` instead of the registry's three to four git
/// processes per name. On every tree shape below, each name's answer at the commit is the
/// registry's `tree_path` + `shadowed` answer, and the on-disk answer for the same checkout
/// (the registry's `repo_path` + `shadowed`, which Maestro calls directly).
#[test]
fn the_git_rule_is_the_registrys_on_every_tree_shape() {
    use std::os::unix::fs::symlink;
    type Build = fn(&Path);
    let shapes: &[(&str, Build)] = &[
        ("legacy", |w| fill(w, ".control")),
        ("new", |w| fill(w, ".broomva")),
        ("both", |w| {
            fill(w, ".control");
            fill(w, ".broomva");
        }),
        ("split", |w| {
            write(w, ".broomva/asks/a.yaml");
            write(w, ".control/initiatives/i.yaml");
            write(w, ".control/preauth.yaml");
        }),
        ("symlinked entry", |w| {
            write(w, ".control/real/a.yaml");
            symlink("real", w.join(".control/asks")).unwrap();
        }),
        ("control is a file", |w| write(w, ".control")),
        ("broomva is a symlink", |w| {
            write(w, ".control/asks/a.yaml");
            symlink(".control", w.join(".broomva")).unwrap();
        }),
        ("empty", |w| write(w, "README.md")),
        ("legacy, one name moved", |w| {
            fill(w, ".control");
            std::fs::remove_dir_all(w.join(".control/bands")).unwrap();
            write(w, ".broomva/bands/b.yaml");
        }),
    ];
    // The names Maestro reads, and one of each other kind: an owner file, a folder and a file
    // without a legacy spelling, a plain file. Every name goes through the same few lines.
    let names = ["asks", "initiatives", "preauth", "loops", "policy", "hooks", "checks", "arcs", "bands"];
    for name in names {
        assert!(registry::REPO.iter().any(|r| r.0 == name), "{name} left the registry");
    }
    for (shape, build) in shapes {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        build(&ws);
        git(&ws, &["init", "-q", "-b", "main"]);
        git(&ws, &["add", "-A"]);
        git(&ws, &["commit", "-q", "-m", "shape"]);
        let store = Store::new(Paths::under(tmp.path()), TreeSource::GitRev("main".into()));
        let disk = Store::new(Paths::under(tmp.path()), TreeSource::WorkingTree);
        let commit = store.tree.commit.clone().unwrap();
        let shadowed = registry::shadowed(&ws, Some(&commit));
        for name in &names {
            let ours = store.rel(name);
            let on_disk = disk.rel(name).map_err(|_| ());
            let theirs: Result<String, ()> = match &shadowed {
                Err(_) => Err(()),
                Ok(both) if both.iter().any(|n| n == name) => Err(()),
                Ok(_) => match registry::tree_path(name, &ws, &commit) {
                    Ok(Some(rel)) => Ok(rel),
                    // In neither: the git reader has no rule of its own; the disk one decides.
                    Ok(None) => on_disk.clone(),
                    Err(_) => Err(()),
                },
            };
            assert_eq!(
                ours.as_ref().map_err(|_| ()),
                theirs.as_ref().map_err(|_| ()),
                "{shape}: {name}: ours {ours:?}"
            );
            assert_eq!(
                ours.as_ref().map_err(|_| ()),
                on_disk.as_ref().map_err(|_| ()),
                "{shape}: {name}: disk and git disagree"
            );
        }
    }
}

fn write(ws: &Path, rel: &str) {
    let file = ws.join(rel);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, "x: 1\n").unwrap();
}

fn fill(ws: &Path, root: &str) {
    for rel in ["asks/a.yaml", "initiatives/i.yaml", "preauth.yaml", "loops/l.yaml", "policy.yaml", "bands/b.yaml"] {
        write(ws, &format!("{root}/{rel}"));
    }
}

// --- $BROOMVA_HOME by kind ---

fn item(dir: &Path, slug: &str) {
    let dir = dir.join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("_work.md"),
        format!("---\nid: {slug}\nkind: task\nstate: proposed\nupdated: 2026-10-01\n---\n\n# {slug}\n"),
    )
    .unwrap();
}

#[test]
fn the_broomva_home_work_folder_is_read_by_initiative() {
    let tmp = tempfile::tempdir().unwrap();
    let mut paths = Paths::under(tmp.path());
    paths.work_root = paths.broomva_home.join("work");
    assert_eq!(paths.work_layout(), WorkLayout::ByInitiative);
    item(&paths.work_root.join("alpha"), "one");
    item(&paths.work_root.join(UNFILED), "two");
    item(&paths.work_root.join(".hidden"), "three");
    let work = Store::new(paths, TreeSource::WorkingTree).work();
    assert!(work.errors.is_empty(), "{:?}", work.errors);
    let mut got: Vec<_> = work.data.unwrap().into_iter().map(|w| (w.slug, w.filed_under)).collect();
    got.sort();
    assert_eq!(got, [("one".to_string(), Some("alpha".to_string())), ("two".to_string(), None)]);
}

#[test]
fn an_item_at_the_top_of_the_broomva_home_work_folder_is_read_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let mut paths = Paths::under(tmp.path());
    paths.work_root = paths.broomva_home.join("work");
    item(&paths.work_root, "flat-item");
    item(&paths.work_root.join("alpha"), "one");
    let work = Store::new(paths, TreeSource::WorkingTree).work();
    let mut got: Vec<_> = work.data.unwrap().into_iter().map(|w| (w.slug, w.filed_under)).collect();
    got.sort();
    assert_eq!(got, [("flat-item".to_string(), None), ("one".to_string(), Some("alpha".to_string()))]);
    assert_eq!(work.errors.len(), 1, "{:?}", work.errors);
    assert!(work.errors[0].at.ends_with("/flat-item") && work.errors[0].reason.contains("top of the work root"));
}

#[test]
fn any_other_work_root_is_flat() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path());
    assert_eq!(paths.work_layout(), WorkLayout::Flat);
    item(&paths.work_root, "one");
    // A folder of items in a flat root is not an initiative: `~/maestro` holds such folders.
    item(&paths.work_root.join("nested"), "two");
    let work = Store::new(paths, TreeSource::WorkingTree).work();
    let got: Vec<_> = work.data.unwrap().into_iter().map(|w| (w.slug, w.filed_under)).collect();
    assert_eq!(got, [("one".to_string(), None)]);
}

#[test]
fn an_unlistable_initiative_bucket_is_reported() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let mut paths = Paths::under(tmp.path());
    paths.work_root = paths.broomva_home.join("work");
    item(&paths.work_root.join("alpha"), "one");
    let locked = paths.work_root.join("beta");
    item(&locked, "two");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let work = Store::new(paths, TreeSource::WorkingTree).work();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(work.data.unwrap().len(), 1);
    assert!(work.errors.iter().any(|e| e.at.ends_with("/beta")), "{:?}", work.errors);
}

#[test]
fn machine_names_come_from_the_registry_table() {
    let paths = Paths::under("/m");
    assert_eq!(paths.ticks_ledger(), PathBuf::from("/m/broomva-home/ledger/loops/ticks.jsonl"));
    assert_eq!(paths.config_dir(), PathBuf::from("/m/broomva-home/config/maestro"));
    assert_eq!(paths.ledger("maestro"), Some(PathBuf::from("/m/broomva-home/ledger/maestro")));
    for bad in ["", "..", ".hidden", "a/b", "a\\b", "a\0b"] {
        assert_eq!(paths.ledger(bad), None, "{bad:?}");
    }
}

// --- the stale-reader check ---

#[test]
fn the_vendored_pair_is_coherent() {
    let toml = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/registry/paths.toml")).unwrap();
    assert_eq!(doctor::sha256(&toml), REGISTRY_SHA256, "re-vendor with scripts/vendor-paths.sh");
}

fn bstack(root: &Path, toml: &[u8], reader: &str) -> PathBuf {
    let dir = root.join("bstack");
    std::fs::create_dir_all(dir.join("references")).unwrap();
    std::fs::create_dir_all(dir.join("scripts/lib")).unwrap();
    std::fs::write(dir.join("references/paths.toml"), toml).unwrap();
    std::fs::write(dir.join("scripts/lib/broomva_paths.rs"), reader).unwrap();
    dir
}

fn statuses(dir: &Path, source: TreeSource) -> Vec<Status> {
    doctor::registry(dir, source).into_iter().map(|c| c.status).collect()
}

#[test]
fn doctor_tells_a_stale_reader() {
    let toml = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/registry/paths.toml")).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = bstack(tmp.path(), &toml, VENDORED_READER);
    assert_eq!(statuses(&dir, TreeSource::WorkingTree), [Status::Ok, Status::Ok]);
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "bstack"]);
    assert_eq!(statuses(&dir, TreeSource::GitRev("main".into())), [Status::Ok, Status::Ok]);

    let mut edited = toml.clone();
    edited.extend_from_slice(b"\n# a new name\n");
    let dir = bstack(&tmp.path().join("registry-moved"), &edited, VENDORED_READER);
    assert_eq!(statuses(&dir, TreeSource::WorkingTree), [Status::Stale, Status::Ok]);

    let reader = format!("{VENDORED_READER}// regenerated\n");
    let dir = bstack(&tmp.path().join("template-moved"), &toml, &reader);
    assert_eq!(statuses(&dir, TreeSource::WorkingTree), [Status::Ok, Status::Stale]);

    assert_eq!(statuses(&tmp.path().join("absent"), TreeSource::WorkingTree), [Status::Unread, Status::Unread]);
}
