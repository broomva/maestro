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
/// seeded ledger committed and `origin/main` at that commit, and an empty flows ledger.
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
    git(&ws, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
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
        let text = self.ledger_text();
        let line = format!("  - id: \"{id}\"\n");
        assert!(text.contains(&line), "{text}");
        let answer = "    answer: \"Ship it\"\n    answer_option: \"ship\"\n    answered_at: \"2026-11-09T00:00:00Z\"\n    answered_by: \"owner:maestro-web\"\n";
        std::fs::write(self.ws.join(".control/asks/ledger.yaml"), text.replace(&line, &format!("{line}{answer}")))
            .unwrap();
    }

    /// Commits the working tree and points `maestro/answers` at it.
    fn land_on_answers_branch(&self) {
        git(&self.ws, &["add", "-A"]);
        git(&self.ws, &["commit", "-qm", "answers"]);
        git(&self.ws, &["branch", "-f", "maestro/answers"]);
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
    let crate::kernel::Reason::Owner { ask_id, linked, .. } = &s.nodes["owner"].unmet[0] else { panic!() };
    assert!(*linked && ask_id.is_some());
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
    let s = f.status(&fi);
    let crate::kernel::Reason::Owner { linked, .. } = &s.nodes["owner"].unmet[0] else { panic!() };
    assert!(!linked);
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
    assert_eq!(code(ask(&mut f.ledger, &f.paths, &f.me, &fi, "ship", Some(&filing()))), "wrong_verb");

    let id = waiting(&f.ask(&fi, Some(&filing())).unwrap()).0.to_string();
    // (a) On the answers branch, unanswered: waiting, nothing appended.
    f.land_on_answers_branch();
    assert_eq!(waiting(&f.ask(&fi, None).unwrap()), (id.as_str(), false));
    // (b) Answered in the working tree only, by anyone who can write it: still waiting.
    f.answer_in_worktree(&id);
    assert_eq!(waiting(&f.ask(&fi, None).unwrap()), (id.as_str(), false));
    assert_eq!(f.status(&fi).nodes["owner"].state, State::Waiting);
    assert_eq!(f.kinds(&fi), ["Started", "AskPinned", "AskLinked"]);
    // (c) The owner's answer lands on the answers branch: the flow resumes.
    f.land_on_answers_branch();
    match f.ask(&fi, None).unwrap() {
        Asked::Answered { ask_id, outputs, source, .. } => {
            assert_eq!(ask_id, id);
            assert_eq!(outputs["answer_option"], "ship");
            assert_eq!(outputs["answer"], "Ship it");
            assert_eq!(source, "maestro/answers");
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
