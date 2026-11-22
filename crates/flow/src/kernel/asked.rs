//! The human-step tests (BRO-3635; BRO-3624 items 1 and 7), each guard mutation-checked in
//! `scripts/mutation-check.sh`. They live in the crate under `cfg(test)` because two of them need
//! seams that exist in no other build: the test clock (`clock`), and the mint and crash seams
//! (`asks::seams`). The writer of record is the stub in `fixtures/requests` (the workspace
//! repository is private), run as `ask_ledger.py file` is, sometimes behind a wrapper that checks
//! something while it runs. The workspace is a git repository whose `origin/main` holds the
//! seeded ledger, so the answers ledger can be read at its revisions as in production. The
//! adoption and retirement tests (`adoption`'s) share this fixture.

use super::*;
use crate::Options;
use crate::clock::TestClock;
use crate::hooked::common::{agent, host, root_in};
use crate::kernel::{CompiledFlow, State, compile, start, status};
use crate::{Reader, ledger::Ledger};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `owner` (human) gates `ship`.
const FLOW: &str = "flow: gate\nversion: 1\nnodes:\n  owner: {class: human, ask: Ship the release?}\n  ship: {class: pure, run: ['true'], needs: [owner]}\n";

pub(crate) fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/requests")
}

/// A python preamble the wrapper runs before the stub writer, in the writer's own process.
pub(crate) fn wrapper(pre: &str) -> String {
    format!(
        "import fcntl, io, json, os, runpy, sys\nHERE = os.path.dirname(os.path.abspath(__file__))\n{pre}\nrunpy.run_path(os.path.join(HERE, 'stub.py'), run_name='__main__')\n"
    )
}

pub(crate) struct Fx {
    pub(crate) tmp: tempfile::TempDir,
    pub(crate) paths: Paths,
    pub(crate) ws: PathBuf,
    pub(crate) flows: PathBuf,
    pub(crate) ledger: Ledger,
    pub(crate) flow: CompiledFlow,
    pub(crate) me: VerifiedSession,
}

pub(crate) fn git(ws: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@localhost", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(ws)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A workspace whose writer of record is `writer` (python; the stub itself when `None`), with the
/// seeded ledger committed and pushed to `origin`, a local bare repository (never the network),
/// and fetched, and an empty flows ledger.
pub(crate) fn fx(writer: Option<&str>, extra_asks: &str) -> Fx {
    crate::kernel::guard_entry();
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let paths = Paths::under(&base);
    let ws = paths.workspace.clone();
    std::fs::create_dir_all(ws.join(".control/asks")).unwrap();
    std::fs::create_dir_all(ws.join("scripts")).unwrap();
    let seeded = std::fs::read_to_string(fixtures().join("ledger.yaml")).unwrap();
    let seeded = seeded.replace("\ndecisions: []", &format!("{extra_asks}\ndecisions: []"));
    std::fs::write(ws.join(".control/asks/ledger.yaml"), seeded).unwrap();
    std::fs::copy(fixtures().join("ask_ledger_stub.py"), ws.join("scripts/stub.py")).unwrap();
    match writer {
        None => std::fs::copy(fixtures().join("ask_ledger_stub.py"), ws.join("scripts/ask_ledger.py")).map(|_| ()),
        Some(w) => std::fs::write(ws.join("scripts/ask_ledger.py"), w),
    }
    .unwrap();
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "-A"]);
    git(&ws, &["commit", "-qm", "seed"]);
    let origin = base.join("origin.git");
    git(&base, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    git(&ws, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&ws, &["push", "-q", "origin", "main"]);
    git(&ws, &["fetch", "-q", "origin"]);
    // The answers remote, for this test thread: the local bare origin, never the network.
    seams::REMOTE.with(|r| *r.borrow_mut() = Some(origin.to_str().unwrap().to_string()));
    let flows = root_in(&base.join("fl"));
    let ledger = Ledger::open(&flows, &host("mac"), Options::default()).unwrap();
    Fx { tmp, paths, ws, flows, ledger, flow: compile(FLOW).unwrap(), me: agent("owner", "marker-1") }
}

/// The body an agent gives: what the stub's ledger fixture holds for every ask.
pub(crate) fn body() -> Map<String, Value> {
    serde_json::json!({
        "headline": "Ship the release?",
        "stakes": "Nothing is blocked; a fixture.",
        "why_you": "Only the owner answers asks.",
        "options": [{ "id": "ship", "label": "Ship" }, { "id": "hold", "label": "Hold", "recommended": true }],
        "blocking": false,
        "default": "hold",
        "preanswerable": false,
        "class": "decision",
        "gates": ["fixture"],
        "exhausted": ["read: a fixture", "default: a fixture"],
    })
    .as_object()
    .unwrap()
    .clone()
}

pub(crate) fn filing() -> Filing {
    Filing { ledger: "ledger.yaml".into(), body: body() }
}

pub(crate) fn code<T>(r: Result<T, KernelError>) -> String {
    r.map_or_else(|e| e.code().to_string(), |_| "ok".to_string())
}

impl Fx {
    pub(crate) fn begin(&mut self) -> InstanceId {
        start(&mut self.ledger, &self.me, &self.flow, serde_json::json!({})).unwrap().instance
    }

    pub(crate) fn ask(&mut self, fi: &InstanceId, filing: Option<&Filing>) -> Result<Asked, KernelError> {
        let result = ask(&mut self.ledger, &self.paths, &self.me, fi, "owner", filing);
        // Older tests use this scratch working tree to simulate a subsequent main landing.
        // Mirror the just-filed branch only in the fixture, never in the kernel.
        if matches!(&result, Ok(Asked::Waiting { appended: Some(_), .. })) {
            self.mirror_filed();
        }
        result
    }

    pub(crate) fn mirror_filed(&self) {
        let branch = git(&self.ws, &["show", "refs/remotes/origin/maestro/ledger:.control/asks/ledger.yaml"]);
        std::fs::write(self.ws.join(".control/asks/ledger.yaml"), branch).unwrap();
    }

    /// Edit and push the scratch ledger branch to model a concurrent ledger writer in a test.
    pub(crate) fn edit_branch(&self, rel: &str, edit: impl FnOnce(String) -> String) {
        let checkout = &self.paths.ledger_checkout;
        let path = checkout.join(rel);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, edit(text)).unwrap();
        git(checkout, &["add", "--", rel]);
        git(checkout, &["commit", "-qm", "fixture branch change"]);
        git(checkout, &["push", "-q", "origin", "HEAD:refs/heads/maestro/ledger"]);
    }

    pub(crate) fn events(&self, fi: &InstanceId) -> Vec<Value> {
        let text =
            std::fs::read_to_string(self.flows.join("instances").join(fi.as_str()).join("events.jsonl")).unwrap();
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    pub(crate) fn kinds(&self, fi: &InstanceId) -> Vec<String> {
        self.events(fi).iter().map(|e| e["kind"].as_str().unwrap().to_string()).collect()
    }

    pub(crate) fn pinned(&self, fi: &InstanceId) -> Value {
        self.events(fi).into_iter().find(|e| e["kind"] == "AskPinned").expect("a pin")["body"]["body"].clone()
    }

    pub(crate) fn ledger_text(&self) -> String {
        std::fs::read_to_string(self.ws.join(".control/asks/ledger.yaml")).unwrap()
    }

    /// The working tree's row with `id`, as the store reads it.
    pub(crate) fn row(&self, id: &str) -> Option<Ask> {
        let tree = Store::new(self.paths.clone(), TreeSource::WorkingTree);
        let row = tree.ask_ledger("ledger.yaml").unwrap().unwrap().asks.into_iter().find(|a| a.id == id);
        row.or_else(|| {
            let branch =
                Store::new(self.paths.clone(), TreeSource::GitRev("refs/remotes/origin/maestro/ledger".into()));
            branch.ask_ledger("ledger.yaml").ok().flatten()?.asks.into_iter().find(|a| a.id == id)
        })
    }

    pub(crate) fn status(&self, fi: &InstanceId) -> crate::kernel::Status {
        let reader = Reader::new(&self.flows, Options::default()).unwrap();
        status(&reader.lock().unwrap(), fi, clock::now()).unwrap()
    }

    /// Adds the owner's answer to the row `id` in the working tree, as a writer of answers would.
    pub(crate) fn answer_in_worktree(&self, id: &str) {
        self.add_to_row(id, "    answer: \"Ship it\"\n    answer_option: \"ship\"\n    answered_at: \"2026-11-09T00:00:00Z\"\n    answered_by: \"owner:maestro-web\"\n");
    }

    /// Adds `lines` (YAML keys at the row's indent) to the working tree's row `id`.
    pub(crate) fn add_to_row(&self, id: &str, lines: &str) {
        let mut text = self.ledger_text();
        let line = format!("  - id: \"{id}\"\n");
        if !text.contains(&line) {
            text = git(&self.ws, &["show", "refs/remotes/origin/maestro/ledger:.control/asks/ledger.yaml"]);
        }
        assert!(text.contains(&line), "{text}");
        std::fs::write(self.ws.join(".control/asks/ledger.yaml"), text.replace(&line, &format!("{line}{lines}")))
            .unwrap();
    }

    /// Adds a whole entry at the end of the working tree's asks.
    pub(crate) fn add_entry(&self, entry: &str) {
        let text = self.ledger_text().replace("\ndecisions: []", &format!("{entry}\ndecisions: []"));
        std::fs::write(self.ws.join(".control/asks/ledger.yaml"), text).unwrap();
    }

    /// Commits the working tree here (on the local main only).
    pub(crate) fn commit(&self) {
        git(&self.ws, &["add", "-A"]);
        git(&self.ws, &["commit", "-qm", "here"]);
    }

    /// Commits the working tree and points `maestro/answers` at it: an answer waiting to land.
    pub(crate) fn put_on_answers_branch(&self) {
        self.commit();
        git(&self.ws, &["branch", "-f", "maestro/answers"]);
    }

    /// Commits the working tree, pushes it to origin's main and fetches: the answer has landed.
    pub(crate) fn land_on_main(&self) {
        let file = self.ws.join(".control/asks/ledger.yaml");
        let text = std::fs::read(&file).unwrap();
        let branch = Command::new("git")
            .arg("-C")
            .arg(&self.ws)
            .args(["show-ref", "--verify", "--quiet", "refs/remotes/origin/maestro/ledger"])
            .status()
            .unwrap()
            .success();
        if branch {
            git(&self.ws, &["checkout", "-q", "HEAD", "--", ".control/asks/ledger.yaml"]);
            git(&self.ws, &["merge", "-q", "--no-edit", "refs/remotes/origin/maestro/ledger"]);
            std::fs::write(&file, text).unwrap();
        }
        git(&self.ws, &["add", "-A"]);
        git(&self.ws, &["commit", "-q", "--allow-empty", "-m", "land"]);
        git(&self.ws, &["push", "-q", "origin", "main"]);
        git(&self.ws, &["fetch", "-q", "origin"]);
    }

    /// A commit holding the working tree's ledger, made without moving main (`commit-tree`), and
    /// the working tree and index put back: what an agent forging an answer could make.
    pub(crate) fn forge(&self) -> String {
        git(&self.ws, &["add", "-A"]);
        let tree = git(&self.ws, &["write-tree"]).trim().to_string();
        let forged = git(&self.ws, &["commit-tree", &tree, "-p", "HEAD", "-m", "answers: owner"]).trim().to_string();
        git(&self.ws, &["reset", "-q", "--hard", "HEAD"]);
        forged
    }
}

fn waiting(a: &Asked) -> (&str, bool) {
    match a {
        Asked::Waiting { ask_id, appended, .. } => (ask_id, appended.is_some()),
        other => panic!("not waiting: {other:?}"),
    }
}

#[test]
fn a_minted_id_collides_with_no_id_or_uid_in_the_ledger() {
    // BRO-3624 item 7: the first two ids drawn are taken, one as an `id`, one as a `uid`.
    let taken = "\n  - id: flow-owner-aaaaaaaaaaaa\n    ask: \"taken as an id\"\n\n  - id: other-1\n    uid: flow-owner-bbbbbbbbbbbb\n    ask: \"taken as a uid\"\n";
    let mut f = fx(None, taken);
    let fi = f.begin();
    seams::SUFFIXES.with(|q| q.borrow_mut().extend(["aaaaaaaaaaaa", "bbbbbbbbbbbb", "cccccccccccc"].map(String::from)));
    let asked = f.ask(&fi, Some(&filing())).unwrap();
    assert_eq!(waiting(&asked), ("flow-owner-cccccccccccc", true));
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
    // Every draw taken: nothing is pinned or filed.
    let fi = f.begin();
    let ids = ["aaaaaaaaaaaa", "bbbbbbbbbbbb", "cccccccccccc"];
    seams::SUFFIXES.with(|q| q.borrow_mut().extend((0..MINT_TRIES).map(|i| ids[i as usize % 3].to_string())));
    assert_eq!(code(f.ask(&fi, Some(&filing()))), "ask_id_exhausted");
    assert_eq!(f.kinds(&fi), ["Started"]);
}

#[test]
fn asked_at_is_pinned_before_hashing_and_the_comparable_body_excludes_origin() {
    // BRO-3624 item 7: `asked_at` comes from the kernel's clock and is in the pinned body (and so
    // its hash), and the row the writer stamps with an `origin` still links.
    let mut f = fx(None, "");
    let clock = TestClock::install();
    clock.set("2026-01-02T03:04:05Z".parse().unwrap());
    let fi = f.begin();
    // The agent never gives asked_at (nor id, uid or ask): the kernel's own goes in.
    for key in ["id", "uid", "ask", "asked_at"] {
        let mut early = filing();
        early.body.insert(key.into(), Value::String("2020-01-01T00:00:00Z".into()));
        assert_eq!(code(f.ask(&fi, Some(&early))), "ask_body_invalid", "{key}");
    }
    let asked = f.ask(&fi, Some(&filing())).unwrap();
    let (id, linked) = waiting(&asked);
    assert!(linked);
    let pinned = f.pinned(&fi);
    assert_eq!(pinned["asked_at"], "2026-01-02T03:04:05Z");
    let row = f.row(id).unwrap();
    assert_eq!(row.asked_at.as_deref(), Some("2026-01-02T03:04:05Z"));
    assert!(row.origin.is_some(), "the writer of record stamps an origin");
    // The comparable hash ignores origin and the owner's keys, and nothing else.
    let mut answered = row.row.clone();
    let map = answered.as_object_mut().unwrap();
    map.insert("answer".into(), JsValue::Str("yes".into()));
    map.remove("origin");
    map.insert("superseded_by".into(), JsValue::Str("new-id".into()));
    assert_eq!(comparable_hash(&answered), comparable_hash(&row.row));
    answered.as_object_mut().unwrap().insert("headline".into(), JsValue::Str("edited".into()));
    assert_ne!(comparable_hash(&answered), comparable_hash(&row.row));
}

#[test]
fn a_row_is_linked_after_a_filing_and_after_a_duplicate_from_a_crash() {
    // BRO-3624 item 7: the working tree's row with the pinned id and hash links, whether the
    // writer filed it now or refuses it as a duplicate of the filing a crash left unlinked.
    let mut f = fx(None, "");
    let fi = f.begin();
    seams::CRASH_AFTER_FILE.with(|c| c.set(true));
    assert_eq!(code(f.ask(&fi, Some(&filing()))), "encode", "the simulated crash");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned"]);
    let id = f.pinned(&fi)["id"].as_str().unwrap().to_string();
    assert!(f.row(&id).is_some(), "filed before the crash");
    // Filed again with the same pin: the writer refuses a duplicate, and the row links.
    let asked = f.ask(&fi, None).unwrap();
    assert_eq!(waiting(&asked), (id.as_str(), true));
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
    assert_eq!(f.ledger_text().matches(&format!("id: \"{id}\"")).count(), 1, "filed once");
    // A plain filing links on the first call.
    let fi = f.begin();
    let asked = f.ask(&fi, Some(&filing())).unwrap();
    assert!(waiting(&asked).1);
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
    // Linked, the node waits on the owner with its ask named.
    let s = f.status(&fi);
    assert_eq!(s.nodes["owner"].state, State::Waiting);
    let crate::kernel::Reason::Owner { ask_id, .. } = &s.nodes["owner"].unmet[0] else { panic!() };
    assert!(ask_id.starts_with("flow-owner-"));
}

#[test]
fn a_hash_miss_is_ask_refused() {
    // BRO-3624 item 7: a row with the pinned id but another body is never linked.
    let edits = "body = json.loads(sys.stdin.read())\nbody['headline'] = 'Edited on the way in'\nsys.stdin = io.StringIO(json.dumps(body))";
    let mut f = fx(Some(&wrapper(edits)), "");
    let fi = f.begin();
    assert_eq!(code(f.ask(&fi, Some(&filing()))), "ask_refused");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned"]);
    // Again: the writer refuses the duplicate, and the row still misses.
    assert_eq!(code(f.ask(&fi, None)), "ask_refused");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned"]);
    // Pinned and never linked: the node waits on its agent, not on the owner.
    let s = f.status(&fi);
    let crate::kernel::Reason::Unfiled { ask_id, .. } = &s.nodes["owner"].unmet[0] else { panic!() };
    assert!(ask_id.is_some());
}

#[test]
fn confirmation_reads_the_pushed_branch_not_the_working_tree_or_main() {
    let mut f = fx(None, "");
    let fi = f.begin();
    let asked = ask(&mut f.ledger, &f.paths, &f.me, &fi, "owner", Some(&filing())).unwrap();
    let (id, linked) = waiting(&asked);
    assert!(linked);
    let at_main = git(&f.ws, &["show", "refs/remotes/origin/main:.control/asks/ledger.yaml"]);
    let branch = git(&f.ws, &["show", "refs/remotes/origin/maestro/ledger:.control/asks/ledger.yaml"]);
    assert!(!at_main.contains(id) && !f.ledger_text().contains(id) && branch.contains(id));
}

#[test]
fn a_busy_ask_ledger_is_retried_until_the_kernels_deadline_then_refused() {
    // BRO-3624 item 7: the ask ledger's lock busy is the kernel's timed retry, against its own
    // clock, then `ask_refused_busy`; never the owner's wait. The writer is busy for five tries.
    let count = "C = os.path.join(os.path.dirname(os.path.dirname(HERE)), 'tries')\nn = int(open(C).read()) + 1 if os.path.exists(C) else 1\nopen(C, 'w').write(str(n))\nif n < 6:\n    sys.stdin.read()\n    print('error: held by pid 1', file=sys.stderr)\n    sys.exit(75)";
    let mut f = fx(Some(&wrapper(count)), "");
    let fi = f.begin();
    let tries = f.tmp.path().join("tries");
    let read = |p: &Path| std::fs::read_to_string(p).ok().and_then(|t| t.parse::<u32>().ok()).unwrap_or(0);
    let clock = TestClock::install();
    let mover = {
        let (clock, tries) = (clock.clone(), tries.clone());
        std::thread::spawn(move || {
            let until = std::time::Instant::now() + Duration::from_secs(20);
            while read(&tries) < 2 && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(1));
            }
            clock.shift(ASK_BUSY_DEADLINE + SignedDuration::from_secs(1));
        })
    };
    let refused = f.ask(&fi, Some(&filing()));
    mover.join().unwrap();
    assert_eq!(code(refused), "ask_refused_busy");
    let n = read(&tries);
    assert!((2..6).contains(&n), "{n} tries: retried, and stopped at the deadline");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned"], "pinned, not filed, not withdrawn");
    // The agent calls again: the same pin is filed once the lock frees, and linked.
    std::fs::write(&tries, "5").unwrap();
    let asked = f.ask(&fi, None).unwrap();
    assert!(waiting(&asked).1);
}

#[test]
fn the_flow_finds_its_ask_by_its_id_never_by_searching() {
    // BRO-3624 item 7: a decoy ask with the node's own question, answered on the answers branch,
    // is never taken for the node's ask. The kernel addresses its ask by the id it minted.
    let decoy = "\n  - id: decoy-1\n    ask: \"Ship the release?\"\n    headline: \"Ship the release?\"\n    answer: \"Ship it\"\n    answer_option: \"ship\"\n    answered_at: \"2026-11-09T00:00:00Z\"\n";
    let mut f = fx(None, decoy);
    git(&f.ws, &["branch", "maestro/answers"]);
    let fi = f.begin();
    assert!(waiting(&f.ask(&fi, Some(&filing())).unwrap()).1);
    assert!(!waiting(&f.ask(&fi, None).unwrap()).1, "still waiting, nothing appended");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
    // Nor does the module read the asks as a list.
    let src = include_str!("asks.rs");
    assert!(!src.contains(".asks()") && !src.contains("asks_list"));
}

#[test]
fn the_flows_lock_is_free_while_an_ask_is_filed() {
    // BRO-3624 item 1: the writer of record itself checks, while it runs, that it can take both
    // OS locks a flows writer holds (the lock file and the root directory). Held, it refuses, and
    // the ask is not filed.
    let mut f = fx(None, "");
    let lock = f.flows.parent().unwrap().join("flows.lock");
    let check = format!(
        "for p in [{lock:?}, {root:?}]:\n    fd = os.open(p, os.O_RDONLY)\n    try:\n        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)\n        fcntl.flock(fd, fcntl.LOCK_UN)\n    except BlockingIOError:\n        sys.stdin.read()\n        print('error: a flows lock is held while filing', file=sys.stderr)\n        sys.exit(2)\n    finally:\n        os.close(fd)\nopen(os.path.join(HERE, 'checked'), 'w').write('ok')",
        root = f.flows,
    );
    std::fs::write(f.ws.join("scripts/ask_ledger.py"), wrapper(&check)).unwrap();
    f.commit();
    git(&f.ws, &["push", "-q", "origin", "main"]);
    let fi = f.begin();
    let asked = f.ask(&fi, Some(&filing())).unwrap();
    assert!(waiting(&asked).1, "{asked:?}");
    assert!(f.paths.ledger_checkout.join("scripts/checked").exists(), "the writer checked the locks");
    // The check is live: with the flows lock held by this process, the writer refuses.
    let held = f.ledger.lock().unwrap();
    let out = Command::new("python3")
        .arg(f.ws.join("scripts/ask_ledger.py"))
        .args(["file", "x.yaml", "--json", "-"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    drop(held);
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    let _ = &f.tmp;
}

#[test]
fn only_the_owner_path_answers_and_the_answer_resumes_the_flow() {
    // BRO-3624 item 7 and BRO-3635: an unanswered ask leaves the node waiting; an answer an agent
    // writes into the working tree is not read; the answer on the answers branch is, and appends
    // `Answered` with the owner's choice as outputs, and the node's successor is ready.
    let mut f = fx(None, "");
    let fi = f.begin();
    // An agent's body carrying an answer is refused before anything is pinned.
    let mut smuggled = filing();
    smuggled.body.insert("answer".into(), Value::String("yes".into()));
    assert_eq!(code(f.ask(&fi, Some(&smuggled))), "ask_body_invalid");
    assert_eq!(f.kinds(&fi), ["Started"]);
    // Another session is refused.
    let other = agent("other", "marker-9");
    assert_eq!(code(ask(&mut f.ledger, &f.paths, &other, &fi, "owner", Some(&filing()))), "not_owner");
    assert_eq!(code(ask(&mut f.ledger, &f.paths, &f.me, &fi, "ship", Some(&filing()))), "not_human");

    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    // On main, unanswered: waiting, nothing appended.
    f.land_on_main();
    assert_eq!(waiting(&f.ask(&fi, None).unwrap()), (id.as_str(), false));
    // Answered in the working tree only, by anyone who can write it: still waiting.
    f.answer_in_worktree(&id);
    assert_eq!(waiting(&f.ask(&fi, None).unwrap()), (id.as_str(), false));
    assert_eq!(f.status(&fi).nodes["owner"].state, State::Waiting);
    // (f) Answered on Maestro's answers branch and not on main: waiting, with the waiting-to-land
    // reason the reads give, and no receipt.
    f.put_on_answers_branch();
    match f.ask(&fi, None).unwrap() {
        Asked::Waiting { landing, appended, .. } => {
            assert_eq!((landing.as_deref(), appended), (Some("maestro/answers"), None))
        }
        other => panic!("{other:?}"),
    }
    let mut unmet = f.status(&fi).nodes["owner"].unmet.clone();
    landing(&f.paths, &mut unmet);
    assert!(
        matches!(&unmet[0], crate::kernel::Reason::AnswerWaitingToLand { at, .. } if at == "maestro/answers"),
        "{unmet:?}"
    );
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
    // (c) The answer lands on origin's main, at the commit the remote names: the flow resumes.
    git(&f.ws, &["push", "-q", "origin", "main"]);
    let main = git(&f.ws, &["rev-parse", "HEAD"]).trim().to_string();
    match f.ask(&fi, None).unwrap() {
        Asked::Answered { ask_id, outputs, source, .. } => {
            assert_eq!(ask_id, id);
            assert_eq!(outputs["answer_option"], "ship");
            assert_eq!(outputs["answer"], "Ship it");
            assert_eq!(source, format!("main@{main}"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked", "Answered"]);
    let s = f.status(&fi);
    assert_eq!((s.nodes["owner"].state, s.nodes["ship"].state), (State::Done, State::Ready));
    assert_eq!(code(f.ask(&fi, None)), "not_enabled", "answered once");
}

#[test]
fn a_writer_refusal_with_no_row_withdraws_the_pin() {
    // A refusal by the writer (here its validator) with no row in the working tree: nothing was
    // filed, the pin is withdrawn with its own code, and the next call mints anew.
    let mut f = fx(None, "");
    let fi = f.begin();
    let mut bad = filing();
    bad.body.insert("stakes".into(), Value::String("STUB-INVALID".into()));
    match f.ask(&fi, Some(&bad)).unwrap() {
        Asked::Withdrawn { code, .. } => assert_eq!(code, ASK_NOT_FILED),
        other => panic!("{other:?}"),
    }
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskWithdrawn"]);
    assert!(waiting(&f.ask(&fi, Some(&filing())).unwrap()).1);
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskWithdrawn", "AskPinned", "AskLinked"]);
}

#[test]
fn only_an_answer_to_the_linked_ask_is_a_human_nodes_receipt() {
    // Replay's half of "nothing an agent passes in stands in for the answer": a `Completed` for a
    // human node, or an `Answered` naming an ask that is not the one linked, is set aside.
    let mut f = fx(None, "");
    let fi = f.begin();
    let append = |f: &mut Fx, kind: &str, body: Value| {
        let mut guard = f.ledger.lock().unwrap();
        let seq = guard.log(&fi).unwrap().seq();
        guard.append(&fi, seq, crate::NewEvent::new(kind, body)).unwrap();
    };
    append(&mut f, "Completed", serde_json::json!({ "node": "owner", "iteration": {}, "attempt": 1 }));
    let answered = |id: &str| {
        serde_json::json!({
            "node": "owner", "iteration": {}, "ask_id": id, "ledger": "ledger.yaml",
            "outputs": { "answer_option": "ship" }, "source": "maestro/answers", "row_hash": "x",
        })
    };
    append(&mut f, "Answered", answered("flow-owner-unlinked"));
    let s = f.status(&fi);
    assert_eq!(s.nodes["owner"].state, State::Waiting);
    let why: Vec<&str> = s.rejected.iter().map(|r| r.reason.as_str()).collect();
    assert_eq!(why, ["wrong_class", "not_linked"]);
    // Linked, an answer naming another ask is still set aside; one naming the linked ask is not.
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    append(&mut f, "Answered", answered("flow-owner-other"));
    assert_eq!(f.status(&fi).nodes["owner"].state, State::Waiting);
    append(&mut f, "Answered", answered(&id));
    assert_eq!(f.status(&fi).nodes["owner"].state, State::Done);
}

// The sweep of every code `ask` adds (BRO-3635, coordinator's pre-P20 ask): each guard has a test
// that fails when the guard is removed, and a line in scripts/mutation-check.sh.

#[test]
fn an_ask_body_the_kernel_does_not_file_is_ask_body_invalid() {
    // `ask_body_invalid`, for each of its guards: no first filing, a ledger name that is not one,
    // a body over its bound, and one nested too deep. Nothing is pinned or filed.
    let mut f = fx(None, "");
    let fi = f.begin();
    assert_eq!(code(f.ask(&fi, None)), "ask_body_invalid", "a first ask needs a filing");
    let bad_name = Filing { ledger: "../ledger.yaml".into(), body: body() };
    assert_eq!(code(f.ask(&fi, Some(&bad_name))), "ask_body_invalid", "not a ledger name");
    let mut big = filing();
    big.body.insert("stakes".into(), Value::String("x".repeat(MAX_ASK_BODY)));
    // The agent's own body is refused by its own bound, before the composed ask is checked
    // (the composed bound refuses it too, with another message: maestro#62 mutation shard 15).
    let refused = f.ask(&fi, Some(&big)).unwrap_err();
    assert_eq!(refused.code(), "ask_body_invalid", "over MAX_ASK_BODY");
    assert!(refused.to_string().contains("the body is"), "{refused}");
    let mut deep = Value::String("x".into());
    for _ in 0..MAX_NESTING {
        deep = Value::Array(vec![deep]);
    }
    let mut nested = filing();
    nested.body.insert("stakes".into(), deep);
    assert_eq!(code(f.ask(&fi, Some(&nested))), "ask_body_invalid", "nested too deep");
    assert_eq!(f.kinds(&fi), ["Started"]);
    assert!(!f.ledger_text().contains("flow-owner-"), "nothing filed");
}

#[test]
fn a_ledger_that_is_not_there_is_ask_ledger_unread() {
    // `ask_ledger_unread`: the id is minted against the ledger's working tree, so a ledger that is
    // not there (or does not read) pins and files nothing.
    let mut f = fx(None, "");
    let fi = f.begin();
    let missing = Filing { ledger: "absent.yaml".into(), body: body() };
    assert_eq!(code(f.ask(&fi, Some(&missing))), "ask_ledger_unread");
    assert_eq!(f.kinds(&fi), ["Started"]);
}

#[test]
fn a_writer_that_says_filed_with_no_row_is_ask_unconfirmed_and_withdraws_nothing() {
    // `ask_unconfirmed`: the writer exits 0 and the working tree has no row. Only a refusal by the
    // writer proves nothing was filed, so the pin stands and the next call files it again.
    let lies = "sys.stdin.read()\nprint('filed it')\nsys.exit(0)";
    let mut f = fx(Some(&wrapper(lies)), "");
    let fi = f.begin();
    assert_eq!(code(f.ask(&fi, Some(&filing()))), "ask_unconfirmed");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned"], "the pin stands, not withdrawn");
    // The same pin, filed by the real writer, links.
    std::fs::copy(fixtures().join("ask_ledger_stub.py"), f.ws.join("scripts/ask_ledger.py")).unwrap();
    f.commit();
    git(&f.ws, &["push", "-q", "origin", "main"]);
    assert!(waiting(&f.ask(&fi, None).unwrap()).1);
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
}

#[test]
fn two_rows_addressed_by_the_pinned_id_are_ask_ambiguous() {
    // `ask_ambiguous`: a filing a crash left unlinked, and then another row whose uid is the
    // pinned id. Neither row is taken for the ask.
    let mut f = fx(None, "");
    let fi = f.begin();
    seams::CRASH_AFTER_FILE.with(|c| c.set(true));
    assert_eq!(code(f.ask(&fi, Some(&filing()))), "encode", "the simulated crash");
    let id = f.pinned(&fi)["id"].as_str().unwrap().to_string();
    f.edit_branch(".control/asks/ledger.yaml", |text| {
        text.replace(
            "\ndecisions: []",
            &format!("\n  - id: other-2\n    uid: \"{id}\"\n    ask: \"Addressed as the flow's ask\"\ndecisions: []"),
        )
    });
    assert_eq!(code(f.ask(&fi, None)), "ask_ambiguous");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned"], "nothing linked");
}

#[test]
fn an_answer_to_another_body_is_answer_mismatch() {
    // `answer_mismatch`: main has the ask answered, but its body was edited, so the
    // answer is not to the question the flow asked.
    let mut f = fx(None, "");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.answer_in_worktree(&id);
    let text = f.ledger_text().replace("headline: \"Ship the release?\"", "headline: \"Ship a different release?\"");
    std::fs::write(f.ws.join(".control/asks/ledger.yaml"), text).unwrap();
    f.land_on_main();
    assert_eq!(code(f.ask(&fi, None)), "answer_mismatch");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
    assert_eq!(f.status(&fi).nodes["owner"].state, State::Waiting);
}

#[test]
fn an_answers_source_that_does_not_read_is_answers_unread() {
    // `answers_unread`: the ledger at the commit origin.s main names does not read. Unread is not
    // unanswered: the call refuses rather than saying waiting.
    let mut f = fx(None, "");
    let fi = f.begin();
    assert!(waiting(&f.ask(&fi, Some(&filing())).unwrap()).1);
    std::fs::write(f.ws.join(".control/asks/ledger.yaml"), "asks: [unclosed\n").unwrap();
    f.land_on_main();
    assert_eq!(code(f.ask(&fi, None)), "answers_unread");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
}

#[test]
fn an_answer_over_the_outputs_bound_is_answer_too_large() {
    // `answer_too_large`: the answer's fields become the node's outputs, which are bounded.
    let mut f = fx(None, "");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    let long = "y".repeat(crate::kernel::MAX_OUTPUTS);
    f.add_to_row(&id, &format!("    answer: \"{long}\"\n    answered_at: \"2026-11-09T00:00:00Z\"\n"));
    f.land_on_main();
    assert_eq!(code(f.ask(&fi, None)), "answer_too_large");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
}

#[test]
fn an_ask_that_changed_while_it_was_filed_is_ask_changed() {
    // `ask_changed`: between filing (flows lock released) and linking, another write of the
    // owner's withdrew the pin. Nothing is linked to a pin that no longer stands.
    let hold = "P = os.path.dirname(os.path.dirname(HERE))\nopen(os.path.join(P, 'reached'), 'w').write('1')\nimport time\nwhile not os.path.exists(os.path.join(P, 'go')):\n    time.sleep(0.01)";
    let mut f = fx(Some(&wrapper(hold)), "");
    let fi = f.begin();
    let (paths, me, flows, fi2) = (f.paths.clone(), f.me.clone(), f.flows.clone(), fi.clone());
    let filer = std::thread::spawn(move || {
        let mut ledger = Ledger::open(&flows, &host("mac"), Options::default()).unwrap();
        code(ask(&mut ledger, &paths, &me, &fi2, "owner", Some(&filing())))
    });
    let reached = f.tmp.path().join("reached");
    let until = std::time::Instant::now() + Duration::from_secs(20);
    while !reached.exists() {
        assert!(std::time::Instant::now() < until, "the writer never ran");
        std::thread::sleep(Duration::from_millis(5));
    }
    let id = f.pinned(&fi)["id"].as_str().unwrap().to_string();
    {
        let mut guard = f.ledger.lock().unwrap();
        let seq = guard.log(&fi).unwrap().seq();
        let body = serde_json::json!({ "node": "owner", "iteration": {}, "ask_id": id, "code": "x", "reason": "x" });
        guard.append(&fi, seq, crate::NewEvent::new("AskWithdrawn", body)).unwrap();
    }
    std::fs::write(f.tmp.path().join("go"), "").unwrap();
    assert_eq!(filer.join().unwrap(), "ask_changed");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskWithdrawn"], "nothing linked");
}

// P20 round 1 (BRO-3635): the guards no test killed, and the answer source the owner chose on
// 2026-11-09 ("Landed main, remote-checked (Recommended)").

/// Every file under the flows ledger and the asks folder, by path: what a refused write must leave.
pub(crate) fn machine(f: &Fx) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![f.flows.parent().unwrap().to_path_buf(), f.ws.join(".control/asks")];
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                todo.push(path);
            } else {
                out.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

#[test]
fn another_session_is_refused_and_writes_nothing() {
    let mut f = fx(None, "");
    let fi = f.begin();
    let before = machine(&f);
    let other = agent("other", "marker-9");
    assert_eq!(code(ask(&mut f.ledger, &f.paths, &other, &fi, "owner", Some(&filing()))), "not_owner");
    assert!(machine(&f) == before, "a refused session wrote: log, projection or asks ledger");
    // A replacement of the owner (same session, new marker) is refused too.
    let replacement = agent("owner", "marker-2");
    assert_eq!(code(ask(&mut f.ledger, &f.paths, &replacement, &fi, "owner", Some(&filing()))), "not_owner");
    assert!(machine(&f) == before);
}

#[test]
fn the_busy_deadline_runs_from_the_first_try_not_from_each() {
    // Each try takes 60 s by the kernel's clock (the mover shifts it while the writer runs, as the
    // real writer's 30 s wait would). From the first try, the 90 s deadline passes on the second;
    // a deadline taken afresh per try would never pass.
    let hold = "P = os.path.dirname(os.path.dirname(HERE))\nC = os.path.join(P, 'tries')\nn = int(open(C).read()) + 1 if os.path.exists(C) else 1\nopen(C, 'w').write(str(n))\nimport time\nwhile not os.path.exists(os.path.join(P, 'go-%d' % n)):\n    time.sleep(0.005)\nif n < 6:\n    sys.stdin.read()\n    print('error: held by pid 1', file=sys.stderr)\n    sys.exit(75)";
    let mut f = fx(Some(&wrapper(hold)), "");
    let fi = f.begin();
    let scripts = f.tmp.path().to_path_buf();
    let clock = TestClock::install();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mover = {
        let (clock, scripts, done) = (clock.clone(), scripts.clone(), done.clone());
        std::thread::spawn(move || {
            for k in 1..=6u32 {
                let tries = scripts.join("tries");
                loop {
                    let n = std::fs::read_to_string(&tries).ok().and_then(|t| t.parse::<u32>().ok()).unwrap_or(0);
                    if n >= k {
                        break;
                    }
                    if done.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                clock.shift(SignedDuration::from_secs(60));
                std::fs::write(scripts.join(format!("go-{k}")), "").unwrap();
            }
        })
    };
    let result = f.ask(&fi, Some(&filing()));
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    mover.join().unwrap();
    assert_eq!(code(result), "ask_refused_busy");
    assert_eq!(std::fs::read_to_string(scripts.join("tries")).unwrap(), "2", "refused at the second try");
}

#[test]
fn a_human_node_still_blocked_by_its_needs_files_nothing() {
    let mut f = fx(None, "");
    f.flow = compile("flow: gate\nversion: 1\nnodes:\n  pre: {class: pure, run: ['true']}\n  owner: {class: human, ask: Ship the release?, needs: [pre]}\n").unwrap();
    let fi = f.begin();
    let before = machine(&f);
    assert_eq!(code(f.ask(&fi, Some(&filing()))), "not_enabled");
    assert!(machine(&f) == before);
    assert_eq!(f.status(&fi).nodes["owner"].state, State::Blocked);
}

#[test]
fn a_pin_is_appended_only_at_the_sequence_it_was_minted_at() {
    // Another call of the owner's pins, files and links between this call's mint and its pin: this
    // pin is refused (`stale_seq`), so the node's ask is filed once, not twice.
    let mut f = fx(None, "");
    let fi = f.begin();
    let (paths, me, flows, fi2) = (f.paths.clone(), f.me.clone(), f.flows.clone(), fi.clone());
    seams::BEFORE_PIN.with(|c| {
        *c.borrow_mut() = Some(Box::new(move || {
            let mut ledger = Ledger::open(&flows, &host("mac"), Options::default()).unwrap();
            assert!(waiting(&ask(&mut ledger, &paths, &me, &fi2, "owner", Some(&filing())).unwrap()).1);
        }));
    });
    assert_eq!(code(f.ask(&fi, Some(&filing()))), "stale_seq");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
    let branch = git(&f.ws, &["show", "refs/remotes/origin/maestro/ledger:.control/asks/ledger.yaml"]);
    assert_eq!(branch.matches("id: \"flow-owner-").count(), 1, "filed once");
}

#[test]
fn ambiguity_on_main_is_ask_ambiguous_too() {
    let mut f = fx(None, "");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.answer_in_worktree(&id);
    f.add_entry(&format!("\n  - id: other-3\n    uid: \"{id}\"\n    ask: \"Addressed as the flow's ask\"\n"));
    f.land_on_main();
    assert_eq!(code(f.ask(&fi, None)), "ask_ambiguous");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
}

#[test]
fn replay_keeps_the_first_pin_links_only_its_hash_and_never_withdraws_a_link() {
    let mut f = fx(None, "");
    let append = |f: &mut Fx, fi: &InstanceId, kind: &str, body: Value| {
        let mut guard = f.ledger.lock().unwrap();
        let seq = guard.log(fi).unwrap().seq();
        guard.append(fi, seq, crate::NewEvent::new(kind, body)).unwrap();
    };
    let owner = |f: &Fx, fi: &InstanceId| f.status(fi).nodes["owner"].unmet[0].clone();
    let pin = |id: &str, hash: &str| serde_json::json!({ "node": "owner", "iteration": {}, "ask_id": id, "ledger": "ledger.yaml", "body": {}, "body_hash": hash });
    let link = |id: &str, hash: &str| serde_json::json!({ "node": "owner", "iteration": {}, "ask_id": id, "ledger": "ledger.yaml", "body_hash": hash });
    // A linked ask: a second pin does not replace it, and a withdrawal does not undo it.
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    append(&mut f, &fi, "AskPinned", pin("flow-owner-other", "x"));
    append(
        &mut f,
        &fi,
        "AskWithdrawn",
        serde_json::json!({ "node": "owner", "iteration": {}, "ask_id": id, "code": "x", "reason": "x" }),
    );
    assert!(matches!(owner(&f, &fi), crate::kernel::Reason::Owner { ask_id, .. } if ask_id == id));
    // A link names the pin's hash, or it does not link.
    let fi = f.begin();
    append(&mut f, &fi, "AskPinned", pin("flow-owner-x", "sha256:pinned"));
    append(&mut f, &fi, "AskLinked", link("flow-owner-x", "sha256:other"));
    assert!(matches!(owner(&f, &fi), crate::kernel::Reason::Unfiled { .. }));
    append(&mut f, &fi, "AskLinked", link("flow-owner-x", "sha256:pinned"));
    assert!(matches!(owner(&f, &fi), crate::kernel::Reason::Owner { .. }));
}

#[test]
fn replay_amend_replaces_only_the_named_link_and_withdrawal_restores_it() {
    let mut f = fx(None, "");
    let fi = f.begin();
    let old = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    let append = |f: &mut Fx, kind: &str, body: Value| {
        let mut guard = f.ledger.lock().unwrap();
        let seq = guard.log(&fi).unwrap().seq();
        guard.append(&fi, seq, crate::NewEvent::new(kind, body)).unwrap();
    };
    let pin = |predecessor: &str| {
        serde_json::json!({
            "node": "owner", "iteration": {}, "ask_id": "flow-owner-amend", "ledger": "ledger.yaml",
            "body": {}, "body_hash": "h", "supersedes": predecessor, "amends": true, "reason": "amended: clarify"
        })
    };
    append(&mut f, "AskPinned", pin("wrong"));
    assert!(f.status(&fi).rejected.iter().any(|r| r.reason == "invalid_amend"));
    append(&mut f, "AskPinned", pin(&old));
    assert!(matches!(f.status(&fi).nodes["owner"].unmet[0], crate::kernel::Reason::Unfiled { .. }));
    append(
        &mut f,
        "AskWithdrawn",
        serde_json::json!({
            "node": "owner", "iteration": {}, "ask_id": "flow-owner-amend", "code": "ask_not_filed", "reason": "refused"
        }),
    );
    assert!(
        matches!(&f.status(&fi).nodes["owner"].unmet[0], crate::kernel::Reason::Owner { ask_id, .. } if ask_id == &old)
    );
}

/// The working tree's ledger with the answer added to row `id`, the working tree put back.
fn answered_ledger(f: &Fx, id: &str) -> String {
    let before = f.ledger_text();
    f.answer_in_worktree(id);
    let answered = f.ledger_text();
    std::fs::write(f.ws.join(".control/asks/ledger.yaml"), before).unwrap();
    answered
}

#[test]
fn a_local_answers_branch_forgery_does_not_resume_the_flow() {
    // (a) An agent commits an answer (answered_by owner:maestro-web) with commit-tree and points
    // the local maestro/answers at it. It reads as waiting to land, never as the owner's answer.
    let mut f = fx(None, "");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.land_on_main();
    f.answer_in_worktree(&id);
    let forged = f.forge();
    git(&f.ws, &["update-ref", "refs/heads/maestro/answers", &forged]);
    match f.ask(&fi, None).unwrap() {
        Asked::Waiting { landing, .. } => assert_eq!(landing.as_deref(), Some("maestro/answers")),
        other => panic!("the forgery resumed the flow: {other:?}"),
    }
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
}

#[test]
fn a_moved_origin_main_ref_does_not_resume_the_flow() {
    // (b) `update-ref refs/remotes/origin/main <forged>`: the kernel asks the remote where main is.
    let mut f = fx(None, "");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.land_on_main();
    f.answer_in_worktree(&id);
    let forged = f.forge();
    git(&f.ws, &["update-ref", "refs/remotes/origin/main", &forged]);
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Waiting { landing: None, .. }));
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
}

#[test]
fn a_remote_that_does_not_answer_is_answers_unread_and_writes_nothing() {
    // (d) The answer has landed and been fetched, but the remote cannot be asked: fail closed.
    let mut f = fx(None, "");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.answer_in_worktree(&id);
    f.land_on_main();
    let nowhere = f.tmp.path().join("nowhere.git");
    seams::REMOTE.with(|r| *r.borrow_mut() = Some(nowhere.to_str().unwrap().to_string()));
    let before = machine(&f);
    assert_eq!(code(f.ask(&fi, None)), "answers_unread");
    assert!(machine(&f) == before);
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
}

#[test]
fn a_main_this_clone_does_not_have_is_answers_unread_until_fetched() {
    // (e) The answer lands on origin's main from another clone; this one has not fetched it.
    let mut f = fx(None, "");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.land_on_main();
    let answered = answered_ledger(&f, &id);
    let other = f.tmp.path().join("other");
    let origin = f.tmp.path().join("origin.git");
    git(f.tmp.path(), &["clone", "-q", origin.to_str().unwrap(), other.to_str().unwrap()]);
    std::fs::write(other.join(".control/asks/ledger.yaml"), answered).unwrap();
    git(&other, &["commit", "-qam", "answers: owner"]);
    git(&other, &["push", "-q", "origin", "main"]);
    let e = f.ask(&fi, None).unwrap_err();
    assert_eq!(e.code(), "answers_unread");
    assert!(e.to_string().contains("not in this clone; fetch it"), "{e}");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
    git(&f.ws, &["fetch", "-q", "origin"]);
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Answered { .. }));
}

#[test]
fn text_a_refusal_echoes_is_bounded() {
    let mut f = fx(None, "");
    let fi = f.begin();
    let long = Filing { ledger: format!("{}x.yaml", "a/".repeat(50_000)), body: body() };
    let e = f.ask(&fi, Some(&long)).unwrap_err();
    assert_eq!(e.code(), "ask_body_invalid");
    assert!(e.to_string().len() < 2 * MAX_ECHO, "{} bytes", e.to_string().len());
    // A ledger name that is not printable ASCII is not filed (it would reach a relayed draft).
    let odd = Filing { ledger: "led\u{2028}ger.yaml".into(), body: body() };
    assert_eq!(code(f.ask(&fi, Some(&odd))), "ask_body_invalid");
    assert_eq!(f.kinds(&fi), ["Started"]);
}

#[test]
fn the_writers_words_in_a_busy_refusal_are_bounded() {
    // The first try waits for the clock; after five busy tries the stub files instead of hanging.
    let loud = "import time\nP = os.path.dirname(os.path.dirname(HERE))\nC = os.path.join(P, 'tries')\nn = int(open(C).read()) + 1 if os.path.exists(C) else 1\nopen(C, 'w').write(str(n))\nif n == 1:\n    open(os.path.join(P, 'ready'), 'w').close()\n    until = time.monotonic() + 30\n    while not os.path.exists(os.path.join(P, 'release')) and time.monotonic() < until:\n        time.sleep(0.01)\nif n < 6:\n    sys.stdin.read()\n    print('error: ' + 'w' * 100000, file=sys.stderr)\n    sys.exit(75)";
    let mut f = fx(Some(&wrapper(loud)), "");
    let fi = f.begin();
    let clock = TestClock::install();
    let ready = f.tmp.path().join("ready");
    let release = f.tmp.path().join("release");
    let mover = std::thread::spawn(move || {
        let until = std::time::Instant::now() + Duration::from_secs(30);
        while !ready.exists() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let started = ready.exists();
        clock.shift(ASK_BUSY_DEADLINE + SignedDuration::from_secs(1));
        std::fs::write(release, "").unwrap();
        assert!(started, "writer did not start");
    });
    let e = f.ask(&fi, Some(&filing())).unwrap_err();
    mover.join().unwrap();
    assert_eq!(e.code(), "ask_refused_busy");
    assert!(e.to_string().len() < 3 * MAX_ECHO, "{} bytes", e.to_string().len());
}

// P20 round 2 (BRO-3635): replace objects and an agent-controlled `origin` cannot forge an answer;
// the answers remote is compiled in (owner decision 2026-11-09, "Compiled into maestro"), and a
// test reads a local bare repository in its place (`seams::REMOTE`).

/// A filed ask, landed on main unanswered, with a forged answered commit made here (not on main):
/// the fixture, the instance, the ask id, main's commit and the forgery.
fn landed_unanswered() -> (Fx, InstanceId, String, String, String) {
    let mut f = fx(None, "");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    let filed = git(&f.ws, &["show", "refs/remotes/origin/maestro/ledger:.control/asks/ledger.yaml"]);
    std::fs::write(f.ws.join(".control/asks/ledger.yaml"), filed).unwrap();
    f.land_on_main();
    let main = git(&f.ws, &["rev-parse", "HEAD"]).trim().to_string();
    f.answer_in_worktree(&id);
    let forged = f.forge();
    (f, fi, id, main, forged)
}

#[test]
fn ask_confirms_the_pushed_branch_without_editing_the_working_tree() {
    let mut f = fx(None, "");
    let before = f.ledger_text();
    std::fs::create_dir_all(f.ws.join(".broomva/asks")).unwrap();
    let fi = f.begin();
    let result = ask(&mut f.ledger, &f.paths, &f.me, &fi, "owner", Some(&filing())).unwrap();
    let id = waiting(&result).0.to_string();
    assert_eq!(f.ledger_text(), before);
    let branch = git(&f.ws, &["show", "refs/remotes/origin/maestro/ledger:.control/asks/ledger.yaml"]);
    assert!(branch.contains(&format!("id: \"{id}\"")));
    assert!(f.kinds(&fi).iter().any(|kind| kind == "AskLinked"));
}

/// Build a signed answer against a local bare origin; no network or owner's files are used.
fn branch_answer_fixture(case: &str, branch: &str) -> (Fx, InstanceId) {
    use base64::{
        Engine,
        engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    };
    use p256::{
        ecdsa::{SigningKey, signature::Signer},
        pkcs8::EncodePublicKey,
    };
    use sha2::{Digest, Sha256};

    let (f, fi, id, _, _) = landed_unanswered();
    f.answer_in_worktree(&id);
    f.add_to_row(&id, "    answer_note: \"approved\"\n");
    let owner = f.tmp.path().join("owner");
    std::fs::create_dir_all(&owner).unwrap();
    let party = maestro_store::passkey::Party::parse("https://owner.example").unwrap();
    let key = SigningKey::from_slice(&[7; 32]).unwrap();
    let signing_key = if case == "forged" { SigningKey::from_slice(&[8; 32]).unwrap() } else { key.clone() };
    let credential = maestro_store::passkey::Credential {
        id: "Y3JlZA".into(),
        public_key: STANDARD.encode(
            p256::PublicKey::from_sec1_bytes(key.verifying_key().to_encoded_point(false).as_bytes())
                .unwrap()
                .to_public_key_der()
                .unwrap()
                .as_bytes(),
        ),
        enrolled_at: "t".into(),
        rp_id: party.id.clone(),
    };
    let ring =
        maestro_store::passkey::Keyring { credentials: if case == "unenrolled" { vec![] } else { vec![credential] } };
    ring.save(&owner).unwrap();
    std::fs::write(owner.join("web-origin"), if case == "party" { "https://other.example" } else { &party.origin })
        .unwrap();
    seams::OWNER_DIR.with(|d| *d.borrow_mut() = Some(owner));
    let intent = serde_json::json!({
        "verb":"ask.answer", "ledger":"ledger.yaml", "handle": id, "hash":"original",
        "option": if case == "option" { "hold" } else { "ship" },
        "note": if case == "note" { "wrong" } else { "approved" },
    })
    .to_string();
    let challenge = maestro_store::passkey::challenge(&intent, "nonce");
    let client =
        serde_json::json!({"type":"webauthn.get", "challenge":challenge, "origin":party.origin, "crossOrigin":false})
            .to_string();
    let mut auth = Sha256::digest(party.id.as_bytes()).to_vec();
    auth.push(0x05);
    auth.extend_from_slice(&[0, 0, 0, 1]);
    let mut signed = auth.clone();
    signed.extend_from_slice(&Sha256::digest(client.as_bytes()));
    let signature: p256::ecdsa::Signature = signing_key.sign(&signed);
    let assertion = maestro_store::passkey::Assertion {
        credential: "Y3JlZA".into(),
        client_data: URL_SAFE_NO_PAD.encode(client),
        authenticator_data: URL_SAFE_NO_PAD.encode(auth),
        signature: URL_SAFE_NO_PAD.encode(signature.to_der().as_bytes()),
    };
    let proof = serde_json::json!({"intent":intent, "nonce":"nonce", "assertion":assertion});
    let message =
        if case == "unsigned" { "answer".to_string() } else { format!("answer\n\nMaestro-Owner-Proof: {proof}") };
    git(&f.ws, &["add", "-A"]);
    let tree = git(&f.ws, &["write-tree"]).trim().to_string();
    let parent = if branch == "maestro/ledger" { "refs/remotes/origin/maestro/ledger" } else { "HEAD" };
    let commit = git(&f.ws, &["commit-tree", &tree, "-p", parent, "-m", &message]).trim().to_string();
    git(&f.ws, &["reset", "-q", "--hard", "HEAD"]);
    git(&f.ws, &["push", "-q", "origin", &format!("{commit}:refs/heads/{branch}")]);
    (f, fi)
}

#[test]
fn remote_branch_answer_needs_a_proof_for_its_row_and_owner() {
    for case in ["valid", "unsigned", "forged", "option", "note", "party", "unenrolled"] {
        let (mut f, fi) = branch_answer_fixture(case, "maestro/ledger");
        let result = f.ask(&fi, None);
        if case == "valid" {
            assert!(matches!(result, Ok(Asked::Answered { .. })), "{case}: {result:?}");
        } else {
            assert_eq!(result.unwrap_err().code(), "answer_unproven", "{case}");
        }
    }
    let (mut f, fi) = branch_answer_fixture("valid", "maestro/answers");
    assert!(matches!(f.ask(&fi, None), Ok(Asked::Answered { .. })));
}

#[test]
fn a_branch_answer_with_missing_owner_configuration_is_unread() {
    let (mut f, fi) = branch_answer_fixture("valid", "maestro/ledger");
    std::fs::remove_file(f.tmp.path().join("owner/passkeys.json")).unwrap();
    assert_eq!(f.ask(&fi, None).unwrap_err().code(), "answers_unread");
}

#[test]
fn proof_follows_the_last_row_change_across_later_commits() {
    for change in [false, true] {
        let (mut f, fi) = branch_answer_fixture("valid", "maestro/ledger");
        git(&f.ws, &["fetch", "-q", "origin", "maestro/ledger"]);
        git(&f.ws, &["checkout", "-q", "--detach", "FETCH_HEAD"]);
        if change {
            let path = f.ws.join(".control/asks/ledger.yaml");
            let text = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, text.replace("approved", "modified")).unwrap();
            git(&f.ws, &["commit", "-qam", "unsigned row change"]);
        } else {
            git(&f.ws, &["commit", "-q", "--allow-empty", "-m", "unrelated change"]);
        }
        git(&f.ws, &["push", "-q", "origin", "HEAD:refs/heads/maestro/ledger"]);
        git(&f.ws, &["checkout", "-q", "main"]);
        let result = f.ask(&fi, None);
        if change {
            assert_eq!(result.unwrap_err().code(), "answer_unproven");
        } else {
            assert!(matches!(result, Ok(Asked::Answered { .. })));
        }
    }
}

#[test]
fn ambiguous_ancestor_in_answer_history_is_unread() {
    let (mut f, fi) = branch_answer_fixture("valid", "maestro/ledger");
    let id = f.events(&fi).into_iter().find(|e| e["kind"] == "AskPinned").unwrap()["body"]["ask_id"]
        .as_str()
        .unwrap()
        .to_string();
    git(&f.ws, &["fetch", "-q", "origin", "maestro/ledger"]);
    git(&f.ws, &["checkout", "-q", "--detach", "FETCH_HEAD"]);
    let path = f.ws.join(".control/asks/ledger.yaml");
    let original = std::fs::read_to_string(&path).unwrap();
    let duplicate = original.replace(
        "\ndecisions: []",
        &format!("\n  - id: duplicate\n    uid: \"{id}\"\n    ask: Duplicate\ndecisions: []"),
    );
    std::fs::write(&path, duplicate).unwrap();
    git(&f.ws, &["commit", "-qam", "duplicate"]);
    std::fs::write(&path, original).unwrap();
    git(&f.ws, &["commit", "-qam", "remove duplicate"]);
    git(&f.ws, &["push", "-q", "origin", "HEAD:refs/heads/maestro/ledger"]);
    git(&f.ws, &["checkout", "-q", "main"]);
    assert_eq!(f.ask(&fi, None).unwrap_err().code(), "answers_unread");
}

#[test]
fn the_remote_ledger_tip_wins_over_a_forged_local_branch() {
    let (mut f, fi) = branch_answer_fixture("valid", "maestro/ledger");
    let main = git(&f.ws, &["rev-parse", "HEAD"]).trim().to_string();
    git(&f.ws, &["update-ref", "refs/heads/maestro/ledger", &main]);
    // A shadow asks folder in the owner's working tree cannot redirect the remote read.
    std::fs::create_dir_all(f.ws.join(".broomva/asks")).unwrap();
    assert!(matches!(f.ask(&fi, None), Ok(Asked::Answered { .. })));
}

#[test]
fn a_remote_branch_tip_missing_from_the_clone_is_unread() {
    let (mut f, fi) = branch_answer_fixture("valid", "maestro/ledger");
    let tip = git(&f.ws, &["ls-remote", "origin", "refs/heads/maestro/ledger"]);
    let tip = tip.split_whitespace().next().unwrap();
    let object = f.ws.join(".git/objects").join(&tip[..2]).join(&tip[2..]);
    std::fs::remove_file(object).unwrap();
    assert_eq!(f.ask(&fi, None).unwrap_err().code(), "answers_unread");
}

/// What `git show <rev>:<ledger>` reads with `env` and no other change: the attack's control.
fn plain_show(f: &Fx, rev: &str, env: &[(&str, &str)]) -> String {
    let out = Command::new("git")
        .args(["show", &format!("{rev}:.control/asks/ledger.yaml")])
        .current_dir(&f.ws)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const ASKS_CHILD: &str = "MAESTRO_FLOW_TEST_ASKS_CHILD";

/// Not a test: the entry a child process runs for `in_child`, with the attack's environment set
/// on it as a whole process. A no-op in a normal test run.
#[test]
fn asks_child_entry() {
    let Ok(spec) = std::env::var(ASKS_CHILD) else { return };
    let parts: Vec<&str> = spec.split('\u{1f}').collect();
    let [base, instance, remote] = parts[..] else { panic!("{spec}") };
    seams::REMOTE.with(|r| *r.borrow_mut() = Some(remote.to_string()));
    let base = PathBuf::from(base);
    let mut ledger = Ledger::open(&root_in(&base.join("fl")), &host("mac"), Options::default()).unwrap();
    let me = agent("owner", "marker-1");
    let out = match ask(&mut ledger, &Paths::under(&base), &me, &InstanceId::new(instance).unwrap(), "owner", None) {
        Ok(Asked::Waiting { .. }) => "waiting".to_string(),
        Ok(Asked::Answered { .. }) => "answered".to_string(),
        Ok(other) => format!("{other:?}"),
        Err(e) => e.code().to_string(),
    };
    println!("ASKS_RESULT {out}");
}

/// `ask` on `fi`'s owner node in a child process whose environment adds `env`.
fn in_child(f: &Fx, fi: &InstanceId, env: &[(&str, &str)]) -> String {
    let base = f.paths.workspace.parent().unwrap();
    let remote = seams::REMOTE.with(|r| r.borrow().clone()).unwrap();
    let spec = [base.to_str().unwrap(), fi.as_str(), &remote].join("\u{1f}");
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["kernel::asks::asked::asks_child_entry", "--exact", "--nocapture", "--test-threads=1"])
        .env(ASKS_CHILD, spec)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|l| l.split_once("ASKS_RESULT ").map(|(_, r)| r.trim().to_string()))
        .unwrap_or_else(|| format!("no result: {text}{}", String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn a_replace_ref_for_mains_commit_does_not_forge_the_answer() {
    let (mut f, fi, id, main, forged) = landed_unanswered();
    git(&f.ws, &["replace", &main, &forged]);
    assert!(plain_show(&f, &main, &[]).contains("answer_option"), "the control: plain git reads the forgery");
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Waiting { .. }), "{id}");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
}

#[test]
fn a_replace_ref_for_the_ledgers_blob_does_not_forge_the_answer() {
    let (mut f, fi, _id, main, forged) = landed_unanswered();
    let genuine = git(&f.ws, &["rev-parse", &format!("{main}:.control/asks/ledger.yaml")]).trim().to_string();
    let fake = git(&f.ws, &["rev-parse", &format!("{forged}:.control/asks/ledger.yaml")]).trim().to_string();
    git(&f.ws, &["replace", "-f", &genuine, &fake]);
    assert!(plain_show(&f, &main, &[]).contains("answer_option"), "the control: plain git reads the forgery");
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Waiting { .. }));
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
}

#[test]
fn a_replace_ref_base_in_the_environment_does_not_forge_the_answer() {
    let (f, fi, _id, main, forged) = landed_unanswered();
    git(&f.ws, &["update-ref", &format!("refs/forge/{main}"), &forged]);
    let env = [("GIT_REPLACE_REF_BASE", "refs/forge/")];
    assert!(plain_show(&f, &main, &env).contains("answer_option"), "the control: plain git reads the forgery");
    assert_eq!(in_child(&f, &fi, &env), "waiting");
    assert_eq!(in_child(&f, &fi, &[]), "waiting");
}

/// An evil bare repository whose main is `forged`, which this clone has (an agent fetched it).
fn evil(f: &Fx, forged: &str) -> String {
    let evil = f.tmp.path().join("evil.git");
    git(f.tmp.path(), &["init", "-q", "--bare", "-b", "main", evil.to_str().unwrap()]);
    git(&f.ws, &["push", "-q", evil.to_str().unwrap(), &format!("{forged}:refs/heads/main")]);
    evil.to_str().unwrap().to_string()
}

#[test]
fn an_agent_controlled_origin_does_not_forge_the_answer() {
    let (mut f, fi, _id, _main, forged) = landed_unanswered();
    let evil = evil(&f, &forged);
    let genuine = seams::REMOTE.with(|r| r.borrow().clone()).unwrap();
    // origin's URL, an insteadOf for the answers remote, and origin's uploadpack, all in the repo.
    git(&f.ws, &["remote", "set-url", "origin", &evil]);
    git(&f.ws, &["config", &format!("url.{evil}.insteadOf"), &genuine]);
    git(&f.ws, &["config", "remote.origin.uploadpack", &format!("git-upload-pack {evil}")]);
    let control =
        Command::new("git").args(["ls-remote", &genuine, "refs/heads/main"]).current_dir(&f.ws).output().unwrap();
    assert!(
        String::from_utf8_lossy(&control.stdout).starts_with(&forged),
        "the control: in the repo, git asks the evil remote"
    );
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Waiting { .. }));
    // The same insteadOf, from the process environment.
    git(&f.ws, &["config", "--unset", &format!("url.{evil}.insteadOf")]);
    let key = format!("url.{evil}.insteadOf");
    let env = [("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", key.as_str()), ("GIT_CONFIG_VALUE_0", genuine.as_str())];
    let control = Command::new("git").args(["ls-remote", &genuine, "refs/heads/main"]).envs(env).output().unwrap();
    assert!(String::from_utf8_lossy(&control.stdout).starts_with(&forged), "the control: the env redirects plain git");
    assert_eq!(in_child(&f, &fi, &env), "waiting");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
}

#[test]
fn only_refs_heads_main_counts_and_a_remote_without_it_is_answers_unread() {
    let (mut f, fi, _id, _main, forged) = landed_unanswered();
    // The remote's HEAD names an answered branch; its main is unanswered: still waiting.
    let origin = seams::REMOTE.with(|r| r.borrow().clone()).unwrap();
    git(&f.ws, &["push", "-q", &origin, &format!("{forged}:refs/heads/forge")]);
    git(Path::new(&origin), &["symbolic-ref", "HEAD", "refs/heads/forge"]);
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Waiting { .. }));
    // A remote with no main at all, whose HEAD is the answered branch: unread, never its HEAD.
    git(Path::new(&origin), &["update-ref", "-d", "refs/heads/main"]);
    let e = f.ask(&fi, None).unwrap_err();
    assert_eq!(e.code(), "answers_unread");
    assert!(e.to_string().contains("has no refs/heads/main"), "{e}");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
}

#[test]
fn an_unanswered_row_on_the_answers_branch_is_not_waiting_to_land() {
    let mut f = fx(None, "");
    let fi = f.begin();
    assert!(waiting(&f.ask(&fi, Some(&filing())).unwrap()).1);
    f.put_on_answers_branch();
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Waiting { landing: None, .. }));
    let mut unmet = f.status(&fi).nodes["owner"].unmet.clone();
    landing(&f.paths, &mut unmet);
    assert!(matches!(&unmet[0], crate::kernel::Reason::Owner { .. }), "{unmet:?}");
}

#[test]
fn clipped_text_is_marked_and_a_long_ledger_error_is_clipped() {
    assert_eq!(clip("short"), "short");
    let cut = clip(&"x".repeat(MAX_ECHO + 10));
    assert_eq!(cut, format!("{}...", "x".repeat(MAX_ECHO)));
    // A ledger name of 250 characters that is a folder: the store's error names it, and the
    // refusal's detail is clipped.
    let mut f = fx(None, "");
    let fi = f.begin();
    let name = format!("{}.yaml", "l".repeat(250));
    std::os::unix::fs::symlink("ledger.yaml", f.ws.join(".control/asks").join(&name)).unwrap();
    f.commit();
    git(&f.ws, &["push", "-q", "origin", "main"]);
    let e = f.ask(&fi, Some(&Filing { ledger: name, body: body() })).unwrap_err();
    assert_eq!(e.code(), "ask_ledger_unread");
    let crate::kernel::KernelError::Refused(Refusal::AskLedgerUnread { detail, .. }) = e else { panic!() };
    assert!(detail.ends_with("...") && detail.len() <= MAX_ECHO + 3, "{} bytes: {detail}", detail.len());
}

#[test]
fn the_production_ls_remote_asks_the_compiled_remote_with_githubs_credential_helper_only() {
    let cmd = answers_ls_remote(ANSWERS_REMOTE);
    let program =
        if cfg!(target_os = "macos") { "/Library/Developer/CommandLineTools/usr/bin/git" } else { "/usr/bin/git" };
    assert_eq!(cmd.get_program(), program, "the platform's git, by absolute path");
    let path = cmd.get_envs().find(|(k, _)| *k == "PATH").and_then(|(_, v)| v);
    assert_eq!(path, Some(std::ffi::OsStr::new(maestro_store::tree::git_path())));
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(
        args,
        [
            "-c",
            "credential.helper=",
            "-c",
            "credential.https://github.com.helper=!gh auth git-credential",
            "ls-remote",
            "--",
            "https://github.com/broomva/workspace.git",
            "refs/heads/main",
        ]
    );
    assert_eq!(cmd.get_current_dir(), Some(Path::new("/")));
    let mut keys: Vec<String> = cmd.get_envs().map(|(k, _)| k.to_string_lossy().into_owned()).collect();
    keys.sort();
    assert_eq!(
        keys,
        ["GIT_CONFIG_GLOBAL", "GIT_CONFIG_NOSYSTEM", "GIT_TERMINAL_PROMPT", "HOME", "PATH", "xcrun_nocache"]
    );
}

/// Not run in CI (`--ignored` by hand): the production call, against GitHub, with this machine's
/// `gh` login. It prints main's commit.
#[test]
#[ignore = "network: asks GitHub for the answers remote's main"]
fn the_compiled_answers_remote_answers_from_github() {
    let sha = maestro_store::tree::remote_ref(answers_ls_remote(ANSWERS_REMOTE), MAIN, REMOTE_TIMEOUT)
        .unwrap_or_else(|e| panic!("{e}"))
        .expect("the answers remote has a main");
    println!("{ANSWERS_REMOTE} {MAIN} = {sha}");
    assert_eq!(sha.len(), 40);
}

// P20 round 3 (BRO-3635): git does not check an object's hash when it reads it, so content
// planted in the object store under a genuine id reads as genuine to git. The answer read checks
// every object on the way against its id (`maestro_store::tree::read_verified`).

/// The loose object file of `id` in the workspace's clone.
fn loose(f: &Fx, id: &str) -> PathBuf {
    f.ws.join(".git/objects").join(&id[..2]).join(&id[2..])
}

/// Puts `forged`'s loose object file where `genuine`'s is: a loose object overwritten in place.
fn overwrite_loose(f: &Fx, genuine: &str, forged: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    let to = loose(f, genuine);
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    if to.exists() {
        std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    std::fs::copy(loose(f, forged), &to).unwrap();
}

fn object(f: &Fx, rev: &str) -> String {
    git(&f.ws, &["rev-parse", rev]).trim().to_string()
}

/// Refused as unread with a hash mismatch, and nothing written.
fn refused_as_forged(f: &mut Fx, fi: &InstanceId) {
    let e = f.ask(fi, None).unwrap_err();
    assert_eq!(e.code(), "answers_unread", "{e}");
    assert!(e.to_string().contains("does not hash to its id"), "{e}");
    assert_eq!(f.kinds(fi), ["Started", "AskPinned", "AskLinked"]);
}

#[test]
fn an_overwritten_loose_ledger_blob_is_not_read_as_the_answer() {
    let (mut f, fi, _id, main, forged) = landed_unanswered();
    let ledger = ":.control/asks/ledger.yaml";
    overwrite_loose(&f, &object(&f, &format!("{main}{ledger}")), &object(&f, &format!("{forged}{ledger}")));
    assert!(plain_show(&f, &main, &[]).contains("answer_option"), "the control: plain git reads the forgery");
    refused_as_forged(&mut f, &fi);
}

#[test]
fn a_pack_naming_the_genuine_blob_with_forged_data_is_not_read_as_the_answer() {
    let (mut f, fi, _id, main, forged) = landed_unanswered();
    let genuine = object(&f, &format!("{main}:.control/asks/ledger.yaml"));
    let data = git(&f.ws, &["cat-file", "blob", &format!("{forged}:.control/asks/ledger.yaml")]);
    // A one-object pack (v2) and index naming `genuine`, its data the forged ledger.
    let script = r#"
import hashlib, struct, sys, zlib, pathlib
oid, data, dest = bytes.fromhex(sys.argv[1]), pathlib.Path(sys.argv[2]).read_bytes(), pathlib.Path(sys.argv[3])
size = len(data)
head = bytearray([(3 << 4) | (size & 0x0f) | (0x80 if size >> 4 else 0)])
size >>= 4
while size:
    head.append((size & 0x7f) | (0x80 if size >> 7 else 0))
    size >>= 7
entry = bytes(head) + zlib.compress(data)
pack = b"PACK" + struct.pack(">II", 2, 1) + entry
pack += hashlib.sha1(pack).digest()
fanout = b"".join(struct.pack(">I", 1 if i >= oid[0] else 0) for i in range(256))
idx = b"\xfftOc" + struct.pack(">I", 2) + fanout + oid + struct.pack(">I", zlib.crc32(entry)) + struct.pack(">I", 12) + pack[-20:]
idx += hashlib.sha1(idx).digest()
name = pack[-20:].hex()
(dest / f"pack-{name}.pack").write_bytes(pack)
(dest / f"pack-{name}.idx").write_bytes(idx)
"#;
    let tmp = f.tmp.path().join("forged-ledger");
    std::fs::write(&tmp, data).unwrap();
    let out = Command::new("python3")
        .args(["-c", script, &genuine, tmp.to_str().unwrap(), f.ws.join(".git/objects/pack").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // The genuine loose copy goes, so the pack is the one git reads (a pack also wins on its own).
    std::fs::remove_file(loose(&f, &genuine)).unwrap();
    assert!(plain_show(&f, &main, &[]).contains("answer_option"), "the control: plain git reads the forgery");
    refused_as_forged(&mut f, &fi);
}

#[test]
fn a_forged_loose_commit_under_mains_unfetched_id_is_not_read_as_the_answer() {
    // The remote's main moves on (another clone pushes an unanswered commit); before this clone
    // fetches it, a forged commit is planted under that id, so "the commit is here" holds.
    let (mut f, fi, _id, _main, forged) = landed_unanswered();
    let other = f.tmp.path().join("other");
    let origin = f.tmp.path().join("origin.git");
    git(f.tmp.path(), &["clone", "-q", origin.to_str().unwrap(), other.to_str().unwrap()]);
    std::fs::write(other.join("unrelated"), "x").unwrap();
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "-qm", "unrelated"]);
    git(&other, &["push", "-q", "origin", "main"]);
    let moved = git(&other, &["rev-parse", "HEAD"]).trim().to_string();
    overwrite_loose(&f, &moved, &forged);
    assert!(plain_show(&f, &moved, &[]).contains("answer_option"), "the control: plain git reads the forgery");
    refused_as_forged(&mut f, &fi);
}

#[test]
fn a_forged_tree_is_not_read_as_the_answer() {
    let (mut f, fi, _id, main, forged) = landed_unanswered();
    let asks = ":.control/asks";
    overwrite_loose(&f, &object(&f, &format!("{main}{asks}")), &object(&f, &format!("{forged}{asks}")));
    assert!(plain_show(&f, &main, &[]).contains("answer_option"), "the control: plain git reads the forgery");
    refused_as_forged(&mut f, &fi);
}

#[test]
fn a_git_dir_or_object_directory_in_the_environment_does_not_redirect_the_read() {
    // The answer has landed: in a child whose environment names another repository or another
    // object store, the kernel still reads this clone, and the flow resumes.
    let mut f = fx(None, "");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.answer_in_worktree(&id);
    f.land_on_main();
    let empty = f.tmp.path().join("empty-objects");
    std::fs::create_dir_all(&empty).unwrap();
    let nowhere = f.tmp.path().join("nowhere");
    assert_eq!(in_child(&f, &fi, &[("GIT_OBJECT_DIRECTORY", empty.to_str().unwrap())]), "answered");
    let fi2 = f.begin();
    let id2 = waiting(&f.ask(&fi2, Some(&filing())).unwrap()).0.to_string();
    f.answer_in_worktree(&id2);
    f.land_on_main();
    assert_eq!(in_child(&f, &fi2, &[("GIT_DIR", nowhere.to_str().unwrap())]), "answered");
}

#[test]
fn a_git_that_is_not_roots_alone_reads_no_answer() {
    // The floor (owner decision 2026-11-09): the git that asks and reads must be root's alone. A
    // binary this user owns stands in for it here (`seams::GIT_CHECKED`): the answer, landed on
    // main, is not read, and nothing is written.
    let mut f = fx(None, "");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.answer_in_worktree(&id);
    f.land_on_main();
    let mine = f.tmp.path().join("git");
    std::fs::write(&mine, "").unwrap();
    seams::GIT_CHECKED.with(|p| *p.borrow_mut() = Some(mine));
    let e = f.ask(&fi, None).unwrap_err();
    assert_eq!(e.code(), "answers_unread");
    assert!(e.to_string().contains("not root's alone"), "{e}");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
    seams::GIT_CHECKED.with(|p| *p.borrow_mut() = None);
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Answered { .. }));
}

#[test]
fn human_ask_pins_filled_params() {
    let mut f = fx(None, "");
    f.flow = compile(
        "flow: f\nversion: 1\nparams: [digest]\nnodes:\n  owner: {class: human, ask: 'Review {{params.digest}}?'}\n",
    )
    .unwrap();
    let fi = start(&mut f.ledger, &f.me, &f.flow, serde_json::json!({"digest": "sha256:example"})).unwrap().instance;
    f.ask(&fi, Some(&filing())).unwrap();
    assert_eq!(f.pinned(&fi)["ask"], "Review sha256:example?");
    // Waiting status names the pinned (filled) question, not the definition's template.
    let s = f.status(&fi);
    let crate::kernel::Reason::Owner { ask, .. } = &s.nodes["owner"].unmet[0] else { panic!() };
    assert_eq!(ask, "Review sha256:example?");
}

#[test]
fn human_ask_requires_declared_body_fields() {
    let mut f = fx(None, "");
    f.flow = compile(
        "flow: f\nversion: 1\nnodes:\n  owner: {class: human, ask: 'Review?', required_body_fields: [evidence]}\n",
    )
    .unwrap();
    let fi = f.begin();
    assert_eq!(code(f.ask(&fi, Some(&filing()))), "ask_body_invalid");
    assert_eq!(f.kinds(&fi), ["Started"]);
    let mut complete = filing();
    complete.body.insert("evidence".into(), Value::String("receipt".into()));
    f.ask(&fi, Some(&complete)).unwrap();
    assert_eq!(f.pinned(&fi)["evidence"], "receipt");
}

#[test]
fn required_kernel_owned_fields_cannot_be_supplied_or_pinned() {
    for key in ["id", "uid", "ask", "asked_at"] {
        let mut f = fx(None, "");
        let yaml = format!(
            "flow: f\nversion: 1\nnodes:\n  owner: {{class: human, ask: 'Review?', required_body_fields: [{key}]}}\n"
        );
        f.flow = compile(&yaml).unwrap();
        let fi = f.begin();
        let mut given = filing();
        assert_eq!(code(f.ask(&fi, Some(&given))), "ask_body_invalid", "missing {key}");
        given.body.insert(key.into(), Value::String("override".into()));
        assert_eq!(code(f.ask(&fi, Some(&given))), "ask_body_invalid", "supplied {key}");
        assert_eq!(f.kinds(&fi), ["Started"]);
    }
}

/// The writer's audit facts (BRO-3846), as the caller check attaches them.
pub(crate) fn audited(session: VerifiedSession, thread: &str) -> VerifiedSession {
    use maestro_core::session::{Audit, IdentityFacts, ProcessFacts};
    let identity = IdentityFacts {
        mode: "t3-attributed".into(),
        variable: Some("T3CODE_THREAD_ID".into()),
        thread: Some(thread.into()),
    };
    session.with_audit(Audit { identity, process: ProcessFacts { pid: 7, ..ProcessFacts::default() } })
}

#[test]
fn a_human_nodes_events_carry_the_writers_audit_facts() {
    let mut f = fx(None, "");
    f.me = audited(f.me.clone(), "owner");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.answer_in_worktree(&id);
    f.land_on_main();
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Answered { ask_id, .. } if ask_id == id));
    let events = f.events(&fi);
    for e in &events {
        let audit = &e["body"]["audit"];
        match e["kind"].as_str().unwrap() {
            "Started" | "AskPinned" | "Answered" => {
                assert_eq!(audit["identity"]["thread"], "owner", "{e}");
                assert_eq!(audit["process"]["pid"], 7, "{e}");
            }
            // Not a write of the session's own: it records no facts.
            _ => assert!(audit.is_null(), "{e}"),
        }
    }
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked", "Answered"]);
}

#[test]
fn returning_inputs_do_not_revive_an_answered_old_ask() {
    let clock = TestClock::install();
    let mut f = fx(None, "");
    f.flow = compile("flow: gate\nversion: 1\nworkdir: \"/tmp\"\nnodes:\n  probe: {class: observe, run: ['true'], fresh_for: 10s, exit_map: {0: ok}}\n  owner: {class: human, ask: 'Ship?', needs: [probe]}\n  ship: {class: pure, run: ['true'], needs: [owner]}\n").unwrap();
    let fi = f.begin();
    let observe = |f: &mut Fx, value| {
        let c = crate::kernel::claim(&mut f.ledger, &f.me, &fi, "probe").unwrap();
        crate::kernel::complete(
            &mut f.ledger,
            &f.me,
            &c.token,
            serde_json::json!({"v": value}),
            &crate::kernel::Limits::default(),
        )
        .unwrap();
    };
    observe(&mut f, 1);
    let first = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    clock.shift(jiff::SignedDuration::from_secs(11));
    observe(&mut f, 2);
    let second = waiting(&f.ask(&fi, None).unwrap()).0.to_string();
    clock.shift(jiff::SignedDuration::from_secs(11));
    observe(&mut f, 1);
    f.answer_in_worktree(&first);
    f.land_on_main(); // The answer exists, but no Answered event was recorded.
    let s = f.status(&fi);
    assert_eq!(s.nodes["owner"].state, State::Waiting);
    assert!(matches!(&s.nodes["owner"].unmet[0], crate::kernel::Reason::Unfiled { ask_id: None, .. }));
    let third = waiting(&f.ask(&fi, None).unwrap()).0.to_string();
    assert_ne!(third, first);
    assert_ne!(third, second);
    let s = f.status(&fi);
    assert_eq!(s.nodes["owner"].state, State::Waiting);
    assert!(matches!(&s.nodes["owner"].unmet[0], crate::kernel::Reason::Owner { ask_id, .. } if ask_id == &third));
    assert!(s.rejected.is_empty(), "{:?}", s.rejected);
}

#[test]
fn superseded_unlinked_pin_is_not_revived_after_delayed_refusal() {
    let clock = TestClock::install();
    let mut f = fx(None, "");
    f.flow = compile("flow: gate\nversion: 1\nworkdir: \"/tmp\"\nnodes:\n  probe: {class: observe, run: ['true'], fresh_for: 10s, exit_map: {0: ok}}\n  owner: {class: human, ask: 'Ship?', needs: [probe]}\n").unwrap();
    let fi = f.begin();
    let observe = |f: &mut Fx, value| {
        let c = crate::kernel::claim(&mut f.ledger, &f.me, &fi, "probe").unwrap();
        crate::kernel::complete(
            &mut f.ledger,
            &f.me,
            &c.token,
            serde_json::json!({"v": value}),
            &crate::kernel::Limits::default(),
        )
        .unwrap();
    };
    observe(&mut f, 1);
    let b_inputs = f.status(&fi).nodes["owner"].inputs.clone();
    let append = |f: &mut Fx, kind: &str, body: Value| {
        let mut guard = f.ledger.lock().unwrap();
        let seq = guard.log(&fi).unwrap().seq();
        guard.append(&fi, seq, crate::NewEvent::new(kind, body)).unwrap();
    };
    let pin = |id: &str, inputs| serde_json::json!({"node":"owner", "iteration":{}, "ask_id":id, "ledger":"ledger.yaml", "body":{}, "body_hash":"h", "inputs":inputs});
    append(&mut f, "AskPinned", pin("B", b_inputs));
    clock.shift(jiff::SignedDuration::from_secs(11));
    observe(&mut f, 2);
    let c_inputs = f.status(&fi).nodes["owner"].inputs.clone();
    append(&mut f, "AskPinned", pin("C", c_inputs));
    append(
        &mut f,
        "AskWithdrawn",
        serde_json::json!({"node":"owner", "iteration":{}, "ask_id":"B", "code":"ask_not_filed", "reason":"refused"}),
    );
    clock.shift(jiff::SignedDuration::from_secs(11));
    observe(&mut f, 1);
    let s = f.status(&fi);
    assert!(s.rejected.iter().any(|r| r.kind == "AskWithdrawn" && r.reason == "not_pinned"));
    assert!(matches!(&s.nodes["owner"].unmet[0], crate::kernel::Reason::Unfiled { ask_id: None, .. }));
    let result = f.ask(&fi, Some(&filing())).unwrap();
    assert!(matches!(result, Asked::Waiting { ref ask_id, .. } if ask_id != "B" && ask_id != "C"), "{result:?}");
}

#[test]
fn a_human_node_whose_inputs_changed_is_re_askable_and_revalidates() {
    let clock = TestClock::install();
    let mut f = fx(None, "");
    f.flow = compile(
        "flow: gate\nversion: 1\nworkdir: \"/tmp\"\nnodes:\n  probe: {class: observe, run: ['true'], fresh_for: 10s, exit_map: {0: ok, 1: moved}}\n  owner: {class: human, ask: 'Ship it?', needs: [probe]}\n  ship: {class: pure, run: ['true'], needs: [owner]}\n",
    )
    .unwrap();
    let fi = f.begin();
    let observe = |f: &mut Fx, fi: &InstanceId, outputs: Value| {
        let c = crate::kernel::claim(&mut f.ledger, &f.me, fi, "probe").unwrap();
        let done = crate::kernel::complete(&mut f.ledger, &f.me, &c.token, outputs, &crate::kernel::Limits::default())
            .unwrap();
        assert!(matches!(done, crate::kernel::Completion::Completed { .. }), "{done:?}");
    };
    observe(&mut f, &fi, serde_json::json!({ "v": 1 }));
    let s = f.status(&fi);
    assert_eq!(
        s.nodes["owner"].unmet,
        [crate::kernel::Reason::Unfiled { ask: "Ship it?".into(), ask_id: None, ledger: None }]
    );
    let d1 = s.nodes["owner"].inputs["probe"].clone();
    let first_id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.answer_in_worktree(&first_id);
    f.land_on_main();
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Answered { ask_id, .. } if ask_id == first_id));
    assert_eq!(f.status(&fi).nodes["owner"].state, State::Done);
    let owner_d1 = f.status(&fi).nodes["ship"].inputs["owner"].clone();

    // Duplicate ask with unchanged inputs is refused by `ask` and by replay.
    assert!(matches!(
        f.ask(&fi, Some(&filing())),
        Err(KernelError::Refused(Refusal::NotEnabled { ref node, ref state })) if node == "owner" && state == "done"
    ));
    let append = |f: &mut Fx, kind: &str, body: Value| {
        let mut guard = f.ledger.lock().unwrap();
        let seq = guard.log(&fi).unwrap().seq();
        guard.append(&fi, seq, crate::NewEvent::new(kind, body)).unwrap();
    };
    append(
        &mut f,
        "AskPinned",
        serde_json::json!({
            "node": "owner",
            "iteration": {},
            "ask_id": "flow-owner-dup",
            "ledger": "ledger.yaml",
            "body": {},
            "body_hash": "h",
            "inputs": { "probe": d1 },
        }),
    );
    assert!(
        f.status(&fi)
            .rejected
            .iter()
            .any(|r| r.kind == "AskPinned" && r.subject == "owner" && r.reason == "already_pinned")
    );

    // Upstream observation is re-run with a new digest: owner is waiting in `pending` (never `enabled`),
    // with `inputs_changed` naming `first_id`.
    clock.shift(jiff::SignedDuration::from_secs(11));
    observe(&mut f, &fi, serde_json::json!({ "v": 2 }));
    let s = f.status(&fi);
    let d2 = s.nodes["owner"].inputs["probe"].clone();
    assert_ne!(d1, d2);
    assert_eq!(s.nodes["owner"].state, State::Waiting);
    assert!(matches!(&s.nodes["owner"].unmet[0], crate::kernel::Reason::Unfiled { ask_id: None, .. }));
    assert!(
        s.nodes["owner"].unmet.contains(&crate::kernel::Reason::InputsChanged { supersedes: Some(first_id.clone()) })
    );
    let reader = Reader::new(&f.flows, Options::default()).unwrap();
    let nxt = crate::kernel::next(&reader.lock().unwrap(), &fi, clock::now()).unwrap();
    assert!(nxt.enabled.is_empty(), "{nxt:?}");
    assert_eq!(nxt.pending["owner"].state, State::Waiting);

    // An answer naming the old ask over the new inputs is rejected as `not_linked`.
    append(
        &mut f,
        "Answered",
        serde_json::json!({
            "node": "owner",
            "iteration": {},
            "ask_id": first_id,
            "ledger": "ledger.yaml",
            "inputs": { "probe": d2 },
            "outputs": { "answer_option": "ship" },
            "source": "main@0",
            "row_hash": "r",
        }),
    );
    assert!(
        f.status(&fi).rejected.iter().any(|r| r.kind == "Answered" && r.subject == "owner" && r.reason == "not_linked")
    );
    assert_eq!(f.status(&fi).nodes["owner"].state, State::Waiting);

    // Revalidation before re-asking: returning upstream to the earlier digest satisfies `owner` again.
    clock.shift(jiff::SignedDuration::from_secs(11));
    observe(&mut f, &fi, serde_json::json!({ "v": 1 }));
    assert_eq!(f.status(&fi).nodes["owner"].state, State::Done);

    // Move upstream to the new digest again; a withdrawn re-ask restores the prior ask, and the
    // next `ask` files a new ask superseding `first_id` with reason `inputs_changed` without
    // deleting or editing the old ask record.
    clock.shift(jiff::SignedDuration::from_secs(11));
    observe(&mut f, &fi, serde_json::json!({ "v": 2 }));
    let mut bad = filing();
    bad.body.insert("stakes".into(), Value::String("STUB-INVALID".into()));
    assert!(matches!(f.ask(&fi, Some(&bad)).unwrap(), Asked::Withdrawn { code: ASK_NOT_FILED, .. }));
    let second_id = match f.ask(&fi, None).unwrap() {
        Asked::Waiting { ask_id, supersedes, reason, appended: Some(_), .. } => {
            assert_eq!(supersedes.as_deref(), Some(first_id.as_str()));
            assert_eq!(reason.as_deref(), Some("inputs_changed"));
            ask_id
        }
        other => panic!("{other:?}"),
    };
    assert_ne!(second_id, first_id);
    let old_row = f.row(&first_id).expect("first ask record stays intact");
    assert_eq!(old_row.answer_option.as_deref(), Some("ship"));
    let new_row = f.row(&second_id).expect("new ask record is filed");
    assert_eq!(new_row.extra["supersedes"], first_id);
    assert_eq!(new_row.extra["reason"], "inputs_changed");
    let s = f.status(&fi);
    assert_eq!(s.nodes["owner"].state, State::Waiting);
    assert!(matches!(&s.nodes["owner"].unmet[0], crate::kernel::Reason::Owner { ask_id, .. } if *ask_id == second_id));
    assert!(
        s.nodes["owner"].unmet.contains(&crate::kernel::Reason::InputsChanged { supersedes: Some(first_id.clone()) })
    );

    // Answer the second ask; then verify revalidation across both answers (`d1` and `d2`).
    f.add_to_row(
        &second_id,
        "    answer: \"Ship v2\"\n    answer_option: \"ship\"\n    answered_at: \"2026-11-09T00:00:00Z\"\n    answered_by: \"owner:maestro-web\"\n",
    );
    f.land_on_main();
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Answered { ask_id, .. } if ask_id == second_id));
    assert_eq!(f.status(&fi).nodes["owner"].state, State::Done);
    let owner_d2 = f.status(&fi).nodes["ship"].inputs["owner"].clone();
    assert_ne!(owner_d1, owner_d2);

    clock.shift(jiff::SignedDuration::from_secs(11));
    observe(&mut f, &fi, serde_json::json!({ "v": 1 }));
    let s = f.status(&fi);
    assert_eq!(s.nodes["owner"].state, State::Done);
    assert_eq!(s.nodes["ship"].inputs["owner"], owner_d1);
    let log = f.ledger.lock().unwrap().log(&fi).unwrap();
    let guard_doc =
        crate::kernel::replay::guard_input(&log, &crate::kernel::replay::pinned(&log).unwrap(), "ship").unwrap();
    assert_eq!(guard_doc["nodes"]["owner"]["outputs"]["answer"], "Ship it");

    clock.shift(jiff::SignedDuration::from_secs(11));
    observe(&mut f, &fi, serde_json::json!({ "v": 2 }));
    let s = f.status(&fi);
    assert_eq!(s.nodes["owner"].state, State::Done);
    assert_eq!(s.nodes["ship"].inputs["owner"], owner_d2);
    let log = f.ledger.lock().unwrap().log(&fi).unwrap();
    let guard_doc =
        crate::kernel::replay::guard_input(&log, &crate::kernel::replay::pinned(&log).unwrap(), "ship").unwrap();
    assert_eq!(guard_doc["nodes"]["owner"]["outputs"]["answer"], "Ship v2");

    // Once `ship` completes, the instance latches terminal (BRO-3870) and refuses any further `ask`.
    let c = crate::kernel::claim(&mut f.ledger, &f.me, &fi, "ship").unwrap();
    let done = crate::kernel::complete(&mut f.ledger, &f.me, &c.token, Value::Null, &crate::kernel::Limits::default())
        .unwrap();
    assert!(matches!(done, crate::kernel::Completion::Completed { .. }), "{done:?}");
    assert!(f.status(&fi).terminal);
    clock.shift(jiff::SignedDuration::from_secs(11));
    assert_eq!(code(f.ask(&fi, Some(&filing()))), "instance_terminal");
}

#[test]
fn legacy_asks_and_answers_without_inputs_bind_to_consumed_digests_at_their_log_position() {
    let mut f = fx(None, "");
    f.flow = compile(
        "flow: gate\nversion: 1\nworkdir: \"/tmp\"\nnodes:\n  probe: {class: observe, run: ['true'], fresh_for: 10s, exit_map: {0: ok, 1: moved}}\n  owner: {class: human, ask: 'Ship it?', needs: [probe]}\n  ship: {class: pure, run: ['true'], needs: [owner]}\n",
    )
    .unwrap();
    let fi = f.begin();
    let c = crate::kernel::claim(&mut f.ledger, &f.me, &fi, "probe").unwrap();
    let done = crate::kernel::complete(
        &mut f.ledger,
        &f.me,
        &c.token,
        serde_json::json!({ "v": 1 }),
        &crate::kernel::Limits::default(),
    )
    .unwrap();
    assert!(matches!(done, crate::kernel::Completion::Completed { .. }), "{done:?}");

    let append = |f: &mut Fx, kind: &str, body: Value| {
        let mut guard = f.ledger.lock().unwrap();
        let seq = guard.log(&fi).unwrap().seq();
        guard.append(&fi, seq, crate::NewEvent::new(kind, body)).unwrap();
    };
    let legacy_id = "flow-owner-legacy000001";
    let mut pin_body = body();
    pin_body.insert("id".into(), Value::String(legacy_id.into()));
    pin_body.insert("ask".into(), Value::String("Ship it?".into()));
    pin_body.insert("asked_at".into(), Value::String("2026-11-09T00:00:00Z".into()));
    let pin_body = requests::compose_ask(&pin_body).unwrap();
    let body_hash = comparable_hash(&JsValue::from(Value::Object(pin_body.clone())));
    append(
        &mut f,
        "AskPinned",
        serde_json::json!({
            "node": "owner",
            "iteration": {},
            "ask_id": legacy_id,
            "ledger": "ledger.yaml",
            "body": pin_body,
            "body_hash": body_hash,
        }),
    );

    let s = f.status(&fi);
    assert_eq!(s.nodes["owner"].state, State::Waiting);
    assert_eq!(
        s.nodes["owner"].unmet,
        [crate::kernel::Reason::Unfiled {
            ask: "Ship it?".into(),
            ask_id: Some(legacy_id.into()),
            ledger: Some("ledger.yaml".into()),
        }]
    );
    match f.ask(&fi, None).unwrap() {
        Asked::Waiting { ask_id, supersedes, reason, appended: Some(_), .. } => {
            assert_eq!(ask_id, legacy_id);
            assert!(supersedes.is_none());
            assert!(reason.is_none());
        }
        other => panic!("{other:?}"),
    }

    append(
        &mut f,
        "Answered",
        serde_json::json!({
            "node": "owner",
            "iteration": {},
            "ask_id": legacy_id,
            "ledger": "ledger.yaml",
            "outputs": { "answer_option": "ship" },
            "source": "main@0",
            "row_hash": "r",
        }),
    );
    let s = f.status(&fi);
    assert_eq!(s.nodes["owner"].state, State::Done);
    assert!(s.nodes["owner"].unmet.is_empty());
    assert!(matches!(
        f.ask(&fi, None),
        Err(KernelError::Refused(Refusal::NotEnabled { ref node, ref state })) if node == "owner" && state == "done"
    ));
}

#[test]
fn amend_refuses_an_answer_on_main_or_waiting_to_land() {
    for landed in [false, true] {
        let mut f = fx(None, "");
        let fi = f.begin();
        let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
        f.answer_in_worktree(&id);
        if landed {
            f.land_on_main();
        } else {
            f.put_on_answers_branch();
        }
        let before = f.kinds(&fi);
        assert_eq!(code(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify")), "ask_amend_answered");
        assert_eq!(f.kinds(&fi), before);
    }
}

#[test]
fn amend_recovers_after_supersede_before_link() {
    let mut f = fx(None, "");
    let fi = f.begin();
    let old = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    seams::CRASH_AFTER_FILE.with(|c| c.set(true));
    assert!(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify").is_err());
    let replacement = f.events(&fi).into_iter().rfind(|e| e["kind"] == "AskPinned").unwrap()["body"]["ask_id"]
        .as_str()
        .unwrap()
        .to_string();
    f.mirror_filed();
    assert!(f.row(&replacement).is_some());
    assert_eq!(text(f.row(&old).unwrap().row.get("superseded_by")).as_deref(), Some(replacement.as_str()));
    let marker = f.ws.join("writer-invoked");
    f.edit_branch("scripts/ask_ledger.py", |_| {
        format!("from pathlib import Path\nPath({:?}).touch()\nraise SystemExit(99)\n", marker.to_str().unwrap())
    });
    assert_eq!(waiting(&f.ask(&fi, None).unwrap()).0, replacement);
    assert!(!marker.exists());
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked", "AskPinned", "AskLinked"]);
}

#[test]
fn amend_writer_refusal_restores_old_ask() {
    let mut f = fx(None, "");
    let fi = f.begin();
    let old = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.edit_branch("scripts/ask_ledger.py", |_| {
        "import sys\nsys.stdin.read()\nprint('error: refused', file=sys.stderr)\nsys.exit(2)\n".into()
    });
    let result = amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify").unwrap();
    assert!(matches!(result, Asked::Withdrawn { code: ASK_NOT_FILED, .. }));
    f.answer_in_worktree(&old);
    f.land_on_main();
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Answered { ask_id, .. } if ask_id == old));
}

#[test]
fn amend_requires_old_row_to_point_to_replacement() {
    let mut f = fx(None, "");
    let fi = f.begin();
    let old = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    seams::CRASH_AFTER_FILE.with(|c| c.set(true));
    assert!(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify").is_err());
    let replacement = f.events(&fi).into_iter().rfind(|e| e["kind"] == "AskPinned").unwrap()["body"]["ask_id"]
        .as_str()
        .unwrap()
        .to_string();
    f.edit_branch(".control/asks/ledger.yaml", |text| {
        text.replace(&format!("    superseded_by: \"{replacement}\"\n"), "")
    });
    f.mirror_filed();
    assert!(f.row(&old).is_some());
    assert_eq!(code(f.ask(&fi, None)), "ask_unconfirmed");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked", "AskPinned"]);
}

#[test]
fn amend_refuses_when_main_is_read_but_its_ledger_is_not() {
    // The answers remote answers, but the ledger at that commit does not read: unread, not open.
    let mut f = fx(None, "");
    let fi = f.begin();
    waiting(&f.ask(&fi, Some(&filing())).unwrap());
    std::fs::write(f.ws.join(".control/asks/ledger.yaml"), "asks: [unterminated\n").unwrap();
    f.land_on_main();
    let before = f.kinds(&fi);
    assert_eq!(code(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify")), "ask_amend_unread");
    assert_eq!(f.kinds(&fi), before);
}

#[test]
fn amend_refuses_an_ask_main_holds_twice() {
    let mut f = fx(None, "");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.add_entry(&format!("\n  - id: \"{id}\"\n    ask: \"a second row by the same id\"\n"));
    f.land_on_main();
    let before = f.kinds(&fi);
    assert_eq!(code(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify")), "ask_ambiguous");
    assert_eq!(f.kinds(&fi), before);
}

#[test]
fn an_answer_to_a_replaced_ask_is_never_the_receipt() {
    // Only the current ask's answer counts (owner, 2026-11-21: "Reject at gate"). An answer that
    // reaches the replaced row on main (the workspace gate, BRO-4039, refuses it there) is not
    // taken: the node waits on the replacement, and the replacement's answer completes it.
    let mut f = fx(None, "");
    let fi = f.begin();
    let a = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    let b = waiting(&amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify").unwrap()).0.to_string();
    f.answer_in_worktree(&a);
    f.land_on_main();
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Waiting { ask_id, .. } if ask_id == b));
    assert_ne!(f.status(&fi).nodes["owner"].state, State::Done);
    f.answer_in_worktree(&b);
    f.land_on_main();
    assert!(
        matches!(f.ask(&fi, None).unwrap(), Asked::Answered { ask_id, source, .. } if ask_id == b && !source.contains(&a))
    );
    assert_eq!(f.status(&fi).nodes["owner"].state, State::Done);
}

#[test]
fn recovery_does_not_link_while_the_old_row_is_open() {
    // The supersede wrote both rows, then the old row's archive was undone before recovery: the
    // replacement is not linked, which would leave two open asks.
    let mut f = fx(None, "");
    let fi = f.begin();
    let old = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    seams::CRASH_AFTER_FILE.with(|c| c.set(true));
    assert!(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify").is_err());
    f.edit_branch(".control/asks/ledger.yaml", |text| {
        text.lines()
            .filter(|l| !["archived_at:", "archived_reason:", "status:"].iter().any(|k| l.trim_start().starts_with(k)))
            .map(|l| format!("{l}\n"))
            .collect()
    });
    f.mirror_filed();
    assert_eq!(f.row(&old).unwrap().state(), maestro_core::ask::AskState::Open);
    let before = f.kinds(&fi);
    assert_eq!(code(f.ask(&fi, None)), "ask_unconfirmed");
    assert_eq!(f.kinds(&fi), before);
}

#[test]
fn amend_refuses_when_an_answers_branch_does_not_read_or_holds_the_ask_twice() {
    for broken in ["asks: [unterminated\n", "twice"] {
        let mut f = fx(None, "");
        let fi = f.begin();
        let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
        let good = f.ledger_text();
        if broken == "twice" {
            f.add_entry(&format!("\n  - id: \"{id}\"\n    ask: \"a second row by the same id\"\n"));
        } else {
            std::fs::write(f.ws.join(".control/asks/ledger.yaml"), broken).unwrap();
        }
        f.put_on_answers_branch();
        std::fs::write(f.ws.join(".control/asks/ledger.yaml"), &good).unwrap();
        let before = f.kinds(&fi);
        assert_eq!(
            code(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify")),
            "ask_amend_unread",
            "{broken}"
        );
        assert_eq!(f.kinds(&fi), before);
        assert!(f.row(&id).unwrap().row.get("superseded_by").is_none());
    }
}

#[test]
fn amend_refuses_when_an_answers_ref_is_there_but_broken() {
    // A ref that exists but names no commit (a blob), or whose file is not a ref at all, is
    // unread, never "no answer".
    for broken in ["blob", "garbage"] {
        let mut f = fx(None, "");
        let fi = f.begin();
        let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
        let target = if broken == "blob" {
            // In the fixture's own repository, whatever GIT_* the test inherited (a hook's
            // GIT_DIR would send the write elsewhere), and checked to be a blob there.
            let in_fixture = |args: &[&str], input: Option<&[u8]>| {
                let mut cmd = std::process::Command::new("git");
                cmd.args(["-C", f.ws.to_str().unwrap()]).args(args);
                for (var, _) in std::env::vars_os() {
                    if var.to_string_lossy().starts_with("GIT_") {
                        cmd.env_remove(var);
                    }
                }
                let mut child =
                    cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
                {
                    use std::io::Write as _;
                    let mut stdin = child.stdin.take().unwrap();
                    if let Some(bytes) = input {
                        stdin.write_all(bytes).unwrap();
                    }
                }
                let out = child.wait_with_output().unwrap();
                assert!(out.status.success(), "git {args:?}");
                String::from_utf8(out.stdout).unwrap().trim().to_string()
            };
            let blob = in_fixture(&["hash-object", "-w", "--stdin"], Some(b"not a commit"));
            assert_eq!(in_fixture(&["cat-file", "-t", &blob], None), "blob");
            format!("{blob}\n")
        } else {
            "this is not an object id\n".to_string()
        };
        let refs = f.ws.join(".git/refs/heads/maestro");
        std::fs::create_dir_all(&refs).unwrap();
        std::fs::write(refs.join("answers"), target).unwrap();
        let before = f.kinds(&fi);
        assert_eq!(
            code(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify")),
            "ask_amend_unread",
            "{broken}"
        );
        assert_eq!(f.kinds(&fi), before);
        assert!(f.row(&id).unwrap().row.get("superseded_by").is_none());
    }
}

#[test]
fn amend_refuses_an_ask_closed_on_main() {
    let mut f = fx(None, "");
    let fi = f.begin();
    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    f.add_to_row(&id, "    archived_at: \"2026-11-20T00:00:00Z\"\n    resolution: \"withdrawn\"\n");
    f.land_on_main();
    let before = f.kinds(&fi);
    assert_eq!(code(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify")), "ask_amend_closed");
    assert_eq!(f.kinds(&fi), before);
}

#[test]
fn amendment_uses_the_committed_asks_directory() {
    let mut f = fx(None, "");
    let fi = f.begin();
    f.ask(&fi, Some(&filing())).unwrap();
    std::fs::create_dir_all(f.ws.join(".broomva/asks")).unwrap();
    let result = amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify");
    assert!(matches!(result, Ok(Asked::Waiting { .. })), "{result:?}");
}

#[test]
fn amend_then_answer_replacement() {
    let mut f = fx(None, "");
    let fi = f.begin();
    let old = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    let result = amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify").unwrap();
    let new = waiting(&result).0.to_string();
    assert_ne!(old, new);
    assert!(f.row(&new).unwrap().origin.is_some(), "replacement retains Maestro origin");
    assert!(
        !f.status(&fi).nodes["owner"].unmet.iter().any(|r| matches!(r, crate::kernel::Reason::InputsChanged { .. }))
    );
    f.answer_in_worktree(&new);
    f.land_on_main();
    assert!(matches!(f.ask(&fi, None).unwrap(), Asked::Answered { ask_id, .. } if ask_id == new));
}

#[test]
fn amend_refuses_when_answers_unread_without_appending() {
    let mut f = fx(None, "");
    let fi = f.begin();
    f.ask(&fi, Some(&filing())).unwrap();
    seams::REMOTE.with(|r| *r.borrow_mut() = Some(f.tmp.path().join("missing").display().to_string()));
    let before = f.kinds(&fi);
    assert_eq!(code(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify")), "ask_amend_unread");
    assert_eq!(f.kinds(&fi), before);
}

#[test]
fn amend_refuses_prior_ask_when_inputs_return() {
    let clock = TestClock::install();
    let mut f = fx(None, "");
    f.flow = compile("flow: gate\nversion: 1\nworkdir: \"/tmp\"\nnodes:\n  probe: {class: observe, run: ['true'], fresh_for: 10s, exit_map: {0: ok}}\n  owner: {class: human, ask: 'Ship it?', needs: [probe]}\n").unwrap();
    let fi = f.begin();
    let observe = |f: &mut Fx, value| {
        let c = crate::kernel::claim(&mut f.ledger, &f.me, &fi, "probe").unwrap();
        crate::kernel::complete(
            &mut f.ledger,
            &f.me,
            &c.token,
            serde_json::json!({"v": value}),
            &crate::kernel::Limits::default(),
        )
        .unwrap();
    };
    observe(&mut f, 1);
    let old = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    clock.shift(jiff::SignedDuration::from_secs(11));
    observe(&mut f, 2);
    let current = waiting(&f.ask(&fi, None).unwrap()).0.to_string();
    clock.shift(jiff::SignedDuration::from_secs(11));
    observe(&mut f, 1);
    let before = f.kinds(&fi);
    assert_eq!(code(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify")), "ask_body_invalid");
    assert_eq!(f.kinds(&fi), before);
    assert!(f.row(&old).unwrap().row.get("archived_at").is_none());
    assert!(f.row(&current).is_some());
}

#[test]
fn amend_rejects_oversized_reason_before_pin() {
    let mut f = fx(None, "");
    let fi = f.begin();
    f.ask(&fi, Some(&filing())).unwrap();
    let before = f.kinds(&fi);
    assert_eq!(
        code(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), &"é".repeat(1025))),
        "ask_body_invalid"
    );
    assert_eq!(f.kinds(&fi), before);
}

#[test]
fn composed_ask_with_amend_reason_obeys_body_limit() {
    let mut f = fx(None, "");
    let fi = f.begin();
    f.ask(&fi, Some(&filing())).unwrap();
    let mut large = body();
    large.insert("stakes".into(), Value::String("x".repeat(MAX_ASK_BODY - 1000)));
    let before = f.kinds(&fi);
    assert_eq!(
        code(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &large, &"r".repeat(2000))),
        "ask_body_invalid"
    );
    assert_eq!(f.kinds(&fi), before);
}

#[test]
fn rejected_amend_pin_is_never_filed() {
    let mut f = fx(None, "");
    let fi = f.begin();
    let old = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    let before = f.ledger_text();
    let marker = f.ws.join("writer-invoked");
    std::fs::write(
        f.ws.join("scripts/ask_ledger.py"),
        format!("from pathlib import Path\nPath({:?}).touch()\nraise SystemExit(2)\n", marker.to_str().unwrap()),
    )
    .unwrap();
    seams::REJECT_AMEND_PIN.with(|c| c.set(true));
    assert_eq!(code(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify")), "ask_changed");
    assert_eq!(f.ledger_text(), before);
    assert!(!marker.exists(), "rejected pin reached the writer");
    assert!(f.row(&old).unwrap().row.get("archived_at").is_none());
}

#[test]
fn ambiguous_recovered_amend_is_reported_as_ambiguous() {
    let mut f = fx(None, "");
    let fi = f.begin();
    f.ask(&fi, Some(&filing())).unwrap();
    seams::CRASH_AFTER_FILE.with(|c| c.set(true));
    assert!(amend(&mut f.ledger, &f.paths, &f.me, &fi, "owner", &body(), "clarify").is_err());
    let replacement = f.events(&fi).into_iter().rfind(|e| e["kind"] == "AskPinned").unwrap()["body"]["ask_id"]
        .as_str()
        .unwrap()
        .to_string();
    f.edit_branch(".control/asks/ledger.yaml", |text| {
        text.replace("\ndecisions: []", &format!("\n  - id: \"{replacement}\"\n    ask: duplicate\ndecisions: []"))
    });
    assert_eq!(code(f.ask(&fi, None)), "ask_ambiguous");
}
