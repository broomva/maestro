//! The human-step tests (BRO-3635; BRO-3624 items 1 and 7), each guard mutation-checked in
//! `scripts/mutation-check.sh`. They live in the crate under `cfg(test)` because two of them need
//! seams that exist in no other build: the test clock (`clock`), and the mint and crash seams
//! (`asks::seams`). The writer of record is the stub in `fixtures/requests` (the workspace
//! repository is private), run as `ask_ledger.py file` is, sometimes behind a wrapper that checks
//! something while it runs. The workspace is a git repository whose `origin/main` holds the
//! seeded ledger, so the answers ledger can be read at its revisions as in production.

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

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/requests")
}

/// A python preamble the wrapper runs before the stub writer, in the writer's own process.
fn wrapper(pre: &str) -> String {
    format!(
        "import fcntl, io, json, os, runpy, sys\nHERE = os.path.dirname(os.path.abspath(__file__))\n{pre}\nrunpy.run_path(os.path.join(HERE, 'stub.py'), run_name='__main__')\n"
    )
}

struct Fx {
    tmp: tempfile::TempDir,
    paths: Paths,
    ws: PathBuf,
    flows: PathBuf,
    ledger: Ledger,
    flow: CompiledFlow,
    me: VerifiedSession,
}

fn git(ws: &Path, args: &[&str]) -> String {
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
fn fx(writer: Option<&str>, extra_asks: &str) -> Fx {
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
fn body() -> Map<String, Value> {
    serde_json::json!({
        "headline": "Ship the release?",
        "stakes": "Nothing is blocked; a fixture.",
        "why_you": "Only the owner answers asks.",
        "options": [{ "id": "ship", "label": "Ship" }, { "id": "hold", "label": "Hold" }],
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

fn filing() -> Filing {
    Filing { ledger: "ledger.yaml".into(), body: body() }
}

fn code<T>(r: Result<T, KernelError>) -> String {
    r.map_or_else(|e| e.code().to_string(), |_| "ok".to_string())
}

impl Fx {
    fn begin(&mut self) -> InstanceId {
        start(&mut self.ledger, &self.me, &self.flow, serde_json::json!({})).unwrap().instance
    }

    fn ask(&mut self, fi: &InstanceId, filing: Option<&Filing>) -> Result<Asked, KernelError> {
        ask(&mut self.ledger, &self.paths, &self.me, fi, "owner", filing)
    }

    fn events(&self, fi: &InstanceId) -> Vec<Value> {
        let text =
            std::fs::read_to_string(self.flows.join("instances").join(fi.as_str()).join("events.jsonl")).unwrap();
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn kinds(&self, fi: &InstanceId) -> Vec<String> {
        self.events(fi).iter().map(|e| e["kind"].as_str().unwrap().to_string()).collect()
    }

    fn pinned(&self, fi: &InstanceId) -> Value {
        self.events(fi).into_iter().find(|e| e["kind"] == "AskPinned").expect("a pin")["body"]["body"].clone()
    }

    fn ledger_text(&self) -> String {
        std::fs::read_to_string(self.ws.join(".control/asks/ledger.yaml")).unwrap()
    }

    /// The working tree's row with `id`, as the store reads it.
    fn row(&self, id: &str) -> Option<Ask> {
        let tree = Store::new(self.paths.clone(), TreeSource::WorkingTree);
        tree.ask_ledger("ledger.yaml").unwrap().unwrap().asks.into_iter().find(|a| a.id == id)
    }

    fn status(&self, fi: &InstanceId) -> crate::kernel::Status {
        let reader = Reader::new(&self.flows, Options::default()).unwrap();
        status(&reader.lock().unwrap(), fi, clock::now()).unwrap()
    }

    /// Adds the owner's answer to the row `id` in the working tree, as a writer of answers would.
    fn answer_in_worktree(&self, id: &str) {
        self.add_to_row(id, "    answer: \"Ship it\"\n    answer_option: \"ship\"\n    answered_at: \"2026-11-09T00:00:00Z\"\n    answered_by: \"owner:maestro-web\"\n");
    }

    /// Adds `lines` (YAML keys at the row's indent) to the working tree's row `id`.
    fn add_to_row(&self, id: &str, lines: &str) {
        let text = self.ledger_text();
        let line = format!("  - id: \"{id}\"\n");
        assert!(text.contains(&line), "{text}");
        std::fs::write(self.ws.join(".control/asks/ledger.yaml"), text.replace(&line, &format!("{line}{lines}")))
            .unwrap();
    }

    /// Adds a whole entry at the end of the working tree's asks.
    fn add_entry(&self, entry: &str) {
        let text = self.ledger_text().replace("\ndecisions: []", &format!("{entry}\ndecisions: []"));
        std::fs::write(self.ws.join(".control/asks/ledger.yaml"), text).unwrap();
    }

    /// Commits the working tree here (on the local main only).
    fn commit(&self) {
        git(&self.ws, &["add", "-A"]);
        git(&self.ws, &["commit", "-qm", "here"]);
    }

    /// Commits the working tree and points `maestro/answers` at it: an answer waiting to land.
    fn put_on_answers_branch(&self) {
        self.commit();
        git(&self.ws, &["branch", "-f", "maestro/answers"]);
    }

    /// Commits the working tree, pushes it to origin's main and fetches: the answer has landed.
    fn land_on_main(&self) {
        self.commit();
        git(&self.ws, &["push", "-q", "origin", "main"]);
        git(&self.ws, &["fetch", "-q", "origin"]);
    }

    /// A commit holding the working tree's ledger, made without moving main (`commit-tree`), and
    /// the working tree and index put back: what an agent forging an answer could make.
    fn forge(&self) -> String {
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
fn confirmation_reads_the_working_tree_not_origin_main() {
    // BRO-3624 item 7: the writer writes the working tree, and origin/main does not have the row
    // until it lands; the link is made from the working tree.
    let mut f = fx(None, "");
    let fi = f.begin();
    let asked = f.ask(&fi, Some(&filing())).unwrap();
    let (id, linked) = waiting(&asked);
    assert!(linked);
    let at_main = git(&f.ws, &["show", "refs/remotes/origin/main:.control/asks/ledger.yaml"]);
    assert!(!at_main.contains(id) && f.ledger_text().contains(id));
}

#[test]
fn a_busy_ask_ledger_is_retried_until_the_kernels_deadline_then_refused() {
    // BRO-3624 item 7: the ask ledger's lock busy is the kernel's timed retry, against its own
    // clock, then `ask_refused_busy`; never the owner's wait. The writer is busy for five tries.
    let count = "C = os.path.join(HERE, 'tries')\nn = int(open(C).read()) + 1 if os.path.exists(C) else 1\nopen(C, 'w').write(str(n))\nif n < 6:\n    sys.stdin.read()\n    print('error: held by pid 1', file=sys.stderr)\n    sys.exit(75)";
    let mut f = fx(Some(&wrapper(count)), "");
    let fi = f.begin();
    let tries = f.ws.join("scripts/tries");
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
    let fi = f.begin();
    let asked = f.ask(&fi, Some(&filing())).unwrap();
    assert!(waiting(&asked).1, "{asked:?}");
    assert!(f.ws.join("scripts/checked").exists(), "the writer checked the locks");
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
    assert_eq!(code(f.ask(&fi, Some(&big))), "ask_body_invalid", "over MAX_ASK_BODY");
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
    f.add_entry(&format!("\n  - id: other-2\n    uid: \"{id}\"\n    ask: \"Addressed as the flow's ask\"\n"));
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
    let hold = "open(os.path.join(HERE, 'reached'), 'w').write('1')\nimport time\nwhile not os.path.exists(os.path.join(HERE, 'go')):\n    time.sleep(0.01)";
    let mut f = fx(Some(&wrapper(hold)), "");
    let fi = f.begin();
    let (paths, me, flows, fi2) = (f.paths.clone(), f.me.clone(), f.flows.clone(), fi.clone());
    let filer = std::thread::spawn(move || {
        let mut ledger = Ledger::open(&flows, &host("mac"), Options::default()).unwrap();
        code(ask(&mut ledger, &paths, &me, &fi2, "owner", Some(&filing())))
    });
    let reached = f.ws.join("scripts/reached");
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
    std::fs::write(f.ws.join("scripts/go"), "").unwrap();
    assert_eq!(filer.join().unwrap(), "ask_changed");
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskWithdrawn"], "nothing linked");
}

// P20 round 1 (BRO-3635): the guards no test killed, and the answer source the owner chose on
// 2026-11-09 ("Landed main, remote-checked (Recommended)").

/// Every file under the flows ledger and the asks folder, by path: what a refused write must leave.
fn machine(f: &Fx) -> BTreeMap<PathBuf, Vec<u8>> {
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
    let hold = "C = os.path.join(HERE, 'tries')\nn = int(open(C).read()) + 1 if os.path.exists(C) else 1\nopen(C, 'w').write(str(n))\nimport time\nwhile not os.path.exists(os.path.join(HERE, 'go-%d' % n)):\n    time.sleep(0.005)\nif n < 6:\n    sys.stdin.read()\n    print('error: held by pid 1', file=sys.stderr)\n    sys.exit(75)";
    let mut f = fx(Some(&wrapper(hold)), "");
    let fi = f.begin();
    let scripts = f.ws.join("scripts");
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
    assert_eq!(f.ledger_text().matches("id: \"flow-owner-").count(), 1, "filed once");
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
    // Busy, loudly, for five tries, then the stub files: a kernel that never stops retrying ends
    // in a filing, never a hang.
    let loud = "C = os.path.join(HERE, 'tries')\nn = int(open(C).read()) + 1 if os.path.exists(C) else 1\nopen(C, 'w').write(str(n))\nif n < 6:\n    sys.stdin.read()\n    print('error: ' + 'w' * 100000, file=sys.stderr)\n    sys.exit(75)";
    let mut f = fx(Some(&wrapper(loud)), "");
    let fi = f.begin();
    let clock = TestClock::install();
    clock.shift(SignedDuration::from_secs(0));
    let mover = {
        let clock = clock.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            clock.shift(ASK_BUSY_DEADLINE + SignedDuration::from_secs(1));
        })
    };
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
    f.land_on_main();
    let main = git(&f.ws, &["rev-parse", "HEAD"]).trim().to_string();
    f.answer_in_worktree(&id);
    let forged = f.forge();
    (f, fi, id, main, forged)
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
    std::fs::create_dir(f.ws.join(".control/asks").join(&name)).unwrap();
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
