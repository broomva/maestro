//! The Stop-hook gate (flow kernel spec, "L4: gates"; BRO-3636): `maestro flow stop-gate`, which
//! Claude Code and Codex run as a Stop hook. It refuses a final message while one of the session's
//! flow instances is open and has work the session can do now, the kernel's own judgment
//! (`maestro_flow::kernel::stop`), and answers in the harness's Stop-hook format. It is no MCP tool:
//! only a harness runs it, as `driver-skill` is only run by hand.
//!
//! # What it reads
//!
//! The harness's Stop-hook JSON on standard input (`hook_event_name: "Stop"`; Claude Code and
//! Codex 0.164 both send `session_id`, `cwd`, `stop_hook_active` and `last_assistant_message`).
//! The Maestro session is not in it: a hook process inherits the harness's environment, so the
//! session is `--session` or `MAESTRO_SESSION`, verified from the caller variables every CLI write
//! reads (`MAESTRO_DIRECT_TOKEN` with `MAESTRO_CALLER_CWD`, or `MAESTRO_BRIDGE_CREDENTIAL`).
//!
//! # What it answers
//!
//! Both harnesses take `{"decision": "block", "reason": ...}` on standard output, with exit 0, as a
//! refusal: the agent goes on with `reason` as its next instruction. Anything else lets the stop
//! through; the gate prints `{}`. Codex's schema allows no other key, so the gate prints no other.
//! Notes for a person go to standard error. The exit status is always 0.
//!
//! # Decisions (the owner's, BRO-3636)
//!
//! - **Fail open.** The gate refuses only on a positive answer from the kernel: an instance the
//!   session owns that is open and has work. A malformed standard input, a session that does not
//!   verify, a ledger that does not read (a root moved or archived included, never "no ledger"),
//!   or refusal counts that cannot be kept (the ledger's write lock held past
//!   [`REFUSALS_WAIT`](maestro_flow::kernel::stop::REFUSALS_WAIT), another host's ledger) let the
//!   stop through, say why on standard error, and file the alert when they can. By design: a
//!   broken ledger must not trap every session. An instance that does not read is noted and alerted
//!   whatever else the gate decides: beside an instance with work, the stop is still refused, and
//!   the refusal names it; with no work beside it, the stop goes through. No session named, or no
//!   flow ledger in the workspace, is no Maestro flow at all: the stop goes through with a note and
//!   no alert. An unreadable refusal-count file blocks and names the file; malformed count files
//!   are quarantined under the lock, alerted, and counted again from zero.
//! - **The loop bound.** A session refused at one instance's log position
//!   [`MAX_REFUSALS`](maestro_flow::kernel::stop::MAX_REFUSALS) times in a row is let through the
//!   next time: the gate appends `StopUnblockable` with reason `no_progress` to each such instance
//!   and files the alert (`kernel::stop::Refusals`). The release persists until a later event
//!   moves that instance's log, including while it temporarily has no work.
//! - **A harness that cannot block** (`--cannot-block`, for a turn-end program that cannot refuse):
//!   the gate appends `StopUnblockable` with reason `harness_cannot_block` to each instance with
//!   work and files the alert. Claude Code and Codex 0.164 both block (README, harness matrix).
//!
//! # The alert
//!
//! A fleet alert is a Maestro ask, filed into `--alert-ledger` through the asks ledger's writer of
//! record (`ask_ledger.py file`, which validates it) and journalled, as `maestro_ask_file` files an
//! ask, but as an unverified caller with no caller check (BRO-3846): an alert reports that the gate
//! could not check or keep a session, often because its claim did not verify, which a caller check
//! would refuse again. An ask is
//! what the owner's Decisions view lists; an escalation is recorded in the requests journal, which
//! no screen shows. Its id is a digest of what it is about, so the same alert is filed once: for a
//! `StopUnblockable`, its instance and position; for a failure, the session, the code and the day.

use crate::{Ambient, Check};
use maestro_core::authority::Principal;
use maestro_core::session::{IdentityRefusal, RefusalReason, SessionRef, Surface, VerifiedSession};
use maestro_flow::kernel::events::{StopUnverified, Unblockable, digest};
use maestro_flow::kernel::stop::{self as kstop, Assessment, Recorded, Refusals};
use maestro_flow::kernel::{self, KernelError};
use maestro_flow::{Ledger, Options, Reader};
use maestro_hosts::{Attributed, IdentityMode, STDIN_SESSION_ID, ThreadVars, VarOutcome};
use maestro_store::{Paths, Store, TreeSource};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// The most bytes of standard input the gate reads.
pub const MAX_INPUT: usize = 1 << 20;

/// The most instances a block reason names.
pub const MAX_NAMED: usize = 5;

/// The alert ledger when none is named: the workspace's fleet ledger.
pub const DEFAULT_ALERT_LEDGER: &str = "bstack-fleet.yaml";

/// A harness the gate answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    pub fn name(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
        }
    }

    /// A refusal, in the harness's Stop-hook output format.
    pub fn block(self, reason: &str) -> String {
        match self {
            // Claude Code: top-level `decision` and `reason` (code.claude.com/docs/en/hooks, Stop).
            Harness::Claude => json!({ "decision": "block", "reason": reason }).to_string(),
            // Codex 0.164: `stop.command.output`, the same two keys; it admits no unknown key.
            Harness::Codex => json!({ "decision": "block", "reason": reason }).to_string(),
        }
    }

    /// The stop goes through.
    pub fn allow(self) -> String {
        "{}".to_string()
    }
}

/// How the gate runs.
#[derive(Debug, Clone)]
pub struct Gate {
    pub harness: Harness,
    /// The harness runs the gate but cannot refuse a stop: record and alert instead.
    pub cannot_block: bool,
    /// `--session` or `MAESTRO_SESSION`.
    pub session: Option<String>,
    /// The asks ledger alerts are filed into.
    pub alert_ledger: String,
}

/// What the gate decided, and what it did on the way.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// What the harness reads on standard output.
    pub stdout: String,
    pub blocked: bool,
    /// Notes for standard error.
    pub notes: Vec<String>,
    /// The `StopUnblockable` events appended: instance and position.
    pub recorded: Vec<(String, u64)>,
    /// The ask ids of the alerts filed (or filed before).
    pub alerts: Vec<String>,
}

/// The Stop-hook payload, as far as the gate reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Payload {
    pub harness_session: Option<String>,
    pub stop_hook_active: bool,
}

/// Reads the harness's Stop-hook JSON: an object whose `hook_event_name` is `Stop`.
pub fn payload(input: &[u8]) -> Result<Payload, String> {
    if input.len() > MAX_INPUT {
        return Err(format!("standard input is over {MAX_INPUT} bytes"));
    }
    let v: Value = serde_json::from_slice(input).map_err(|e| format!("standard input is not JSON ({e})"))?;
    let Some(o) = v.as_object() else { return Err("standard input is not a JSON object".into()) };
    match o.get("hook_event_name").and_then(Value::as_str) {
        Some("Stop") => {}
        Some(other) => return Err(format!("hook_event_name is {other:?}, not \"Stop\"")),
        None => return Err("standard input has no hook_event_name".into()),
    }
    Ok(Payload {
        harness_session: o.get("session_id").and_then(Value::as_str).map(str::to_string),
        stop_hook_active: o.get("stop_hook_active").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Where the run stands, for its notes and alerts.
struct Run<'a> {
    paths: &'a Paths,
    hosts: &'a maestro_hosts::SurfaceRegistry,
    ambient: Ambient,
    claim: &'a Value,
    gate: &'a Gate,
    out: Outcome,
}

/// Runs the gate over `input` (see the module docs). `claim` carries the caller's claim fields
/// (`cwd`, `token`, `node`, `credential`) when they are not in the environment.
pub fn gate(
    paths: &Paths,
    hosts: &maestro_hosts::SurfaceRegistry,
    ambient: Ambient,
    claim: &Value,
    gate: &Gate,
    input: &[u8],
) -> Outcome {
    let out = Outcome {
        stdout: gate.harness.allow(),
        blocked: false,
        notes: Vec::new(),
        recorded: Vec::new(),
        alerts: Vec::new(),
    };
    let mut run = Run { paths, hosts, ambient, claim, gate, out };
    run.judge(input);
    run.out
}

/// [`gate`] as the CLI runs it, in the harness's hook process: the surface registries from the
/// environment, and the caller's claim from the caller variables the hook inherits.
pub fn gate_in_env(paths: &Paths, g: &Gate, input: &[u8]) -> Outcome {
    let hosts = maestro_hosts::SurfaceRegistry::from_env();
    gate(paths, &hosts, Ambient::Caller, &json!({}), g, input)
}

/// Use the same fail-open code whether the reader or the writer encounters a busy lock.
fn busy_code(code: &str) -> &str {
    if code == "busy" { "refusals_busy" } else { code }
}

impl Run<'_> {
    fn note(&mut self, text: impl Into<String>) {
        self.out.notes.push(format!("maestro stop-gate: {}", text.into()));
    }

    /// Lets the stop through on a failure (fail open), files the alert when it can.
    fn fail_open(&mut self, session: Option<&SessionRef>, code: &str, reason: &str) {
        let reason = reason.trim_end_matches('.');
        self.note(format!("{code}: {reason}. The stop goes through (the gate fails open)."));
        match session {
            Some(s) => {
                let day = maestro_flow::clock::now().to_string();
                // Keyed by the workspace, never the named session: a session that does not verify
                // is the caller's own string, and naming it would mint an alert per string.
                let workspace = self.paths.workspace.display();
                let key = format!("fail_open\n{workspace}\n{code}\n{}", day.get(..10).unwrap_or_default());
                let words = format!(
                    "The flow stop gate could not check session {s}, so it let the session stop without knowing whether a flow instance it owns had work left. Why: {code}, {reason}."
                );
                self.alert(s, &key, UNCHECKED, &words, &Principal::Unverified);
            }
            None => self.note("no session to file the alert as, so none was filed."),
        }
    }

    /// A count file that cannot be read or quarantined must not bypass the gate.
    fn block_unread_counts(&mut self, session: &SessionRef, why: &str) {
        let reason = format!(
            "refusals_unread: the refusal count file is unreadable: {why}. Report and repair this file before stopping."
        );
        self.note(&reason);
        self.out.stdout = self.gate.harness.block(&reason);
        self.out.blocked = true;
        let key = format!("refusals_unread\n{session}\n{why}");
        self.alert(session, &key, UNCHECKED, &reason, &Principal::Unverified);
    }

    /// Notes and alerts the instances that did not read (see `judge`).
    fn report_unread(&mut self, session: &SessionRef, list: &str) {
        self.note(format!("instances_unread: {list} did not read, so the gate could not check it for work."));
        let day = maestro_flow::clock::now().to_string();
        let key = format!("fail_open\n{session}\ninstances_unread\n{}", day.get(..10).unwrap_or_default());
        let words = format!(
            "The flow stop gate could not read flow instance(s) {list} while it checked session {session}, so it could not tell whether they had work left. Repair or retire them; until then the gate cannot check them."
        );
        self.alert(session, &key, UNCHECKED, &words, &Principal::Unverified);
    }

    fn judge(&mut self, input: &[u8]) {
        let named = self.gate.session.clone().filter(|s| !s.trim().is_empty());
        let Some(text) = named else {
            return self.bind(None, input);
        };
        let session: SessionRef = match text.parse() {
            Ok(s) => s,
            Err(e) => return self.fail_open(None, "session_invalid", &format!("{text:?} is not a session ({e})")),
        };
        if let Err(e) = payload(input) {
            return self.fail_open(Some(&session), "input_malformed", &e);
        }
        // Attributed mode (BRO-3846): a named session is bound through the hook's own thread
        // variables too, the stdin `session_id` of a Codex hook included, and must be their thread.
        if self.hosts.identity_mode() == IdentityMode::Attributed {
            if session.surface == Surface::T3 && !self.hook_vars(input).is_empty() {
                return self.bind(Some(session), input);
            }
            // Beside a Paseo or Direct session, on a host with a T3 registry: a hook variable (the
            // environment's, or a Codex hook's stdin `session_id`) that resolves to a T3 thread names
            // a thread the session can never be, so the binding is unavailable, with that thread a
            // candidate. A registry that does not read is unavailable too. One that names no thread
            // is Codex's own id, which any Codex session has: it binds nothing.
            if session.surface != Surface::T3 && self.hosts.has_surface(&Surface::T3) {
                let each = self.hosts.resolve_each(&self.hook_vars(input));
                let threads = self.threads_of(&each);
                if let Some(first) = threads.first() {
                    let why = format!("a hook variable names T3 thread {}, never the session {session}", first.id);
                    return self.unbound(&threads, &IdentityRefusal::new(RefusalReason::ThreadMismatch, why));
                }
                if let Some((v, VarOutcome::Unreadable(why))) =
                    each.iter().find(|(_, o)| matches!(o, VarOutcome::Unreadable(_)))
                {
                    let why = format!("{} could not be resolved: T3's registry did not read ({why})", v.name);
                    return self.unbound(&[], &IdentityRefusal::new(RefusalReason::RegistryUnavailable, why));
                }
            }
        }
        let me = match crate::verified_caller(self.hosts, Some(&session), self.claim, self.ambient, Check::Write) {
            Ok(Some(me)) => me,
            Ok(None) => {
                let why = format!("session {session} is not verified: no launch token or bridge credential");
                return self.fail_open(Some(&session), "caller_unverified", &why);
            }
            Err(e) => return self.refused_named(&session, &[], e),
        };
        self.gated(&session, &me);
    }

    /// The hook's thread variables: the registry's captured ones, and a Codex hook's stdin
    /// `session_id` on a host with a T3 registry (elsewhere it is no T3 thread's).
    fn hook_vars(&self, input: &[u8]) -> ThreadVars {
        let stdin = self.hook_stdin(input).filter(|_| self.hosts.has_surface(&Surface::T3));
        self.hosts.thread_vars().clone().with_native(STDIN_SESSION_ID, stdin.as_deref())
    }

    /// A Codex hook's stdin `session_id`: Codex's native thread id. A Claude hook's is Claude's own
    /// and binds nothing.
    fn hook_stdin(&self, input: &[u8]) -> Option<String> {
        match self.gate.harness {
            Harness::Codex => payload(input).ok().and_then(|p| p.harness_session),
            Harness::Claude => None,
        }
    }

    /// A named session's caller check refused: an attribution or a T3 registry failure is a
    /// binding that is unavailable ([`Run::unbound`], with `candidates` and the named T3 session);
    /// any other refusal (a credential that does not verify) fails open as before.
    fn refused_named(&mut self, session: &SessionRef, candidates: &[SessionRef], e: rmcp::ErrorData) {
        let reason = e.data.as_ref().and_then(|d| d.get("reason")).cloned();
        let reason = reason.and_then(|r| serde_json::from_value::<RefusalReason>(r).ok());
        let registry = matches!(reason, Some(RefusalReason::RegistryUnavailable | RefusalReason::RegistryDisconnected));
        match reason {
            Some(r) if r.is_attribution() || (registry && session.surface == Surface::T3) => {
                let mut all = candidates.to_vec();
                self.add_candidate(&mut all, session);
                self.unbound(&all, &IdentityRefusal::new(r, e.message.to_string()));
            }
            _ => self.fail_open(Some(session), "caller_refused", &e.message),
        }
    }

    /// The T3 threads the hook's variables resolved to, each canonical, once.
    fn threads_of(&self, each: &[(maestro_hosts::ThreadVar, VarOutcome)]) -> Vec<SessionRef> {
        let host = self.hosts.t3_environment();
        let mut threads: Vec<SessionRef> = Vec::new();
        for (_, outcome) in each {
            if let VarOutcome::Thread { id, environment } = outcome {
                let environment = environment.clone().or_else(|| host.clone());
                let s = SessionRef { surface: Surface::T3, environment, id: id.clone() };
                if !threads.contains(&s) {
                    threads.push(s);
                }
            }
        }
        threads
    }

    /// `session` as a candidate thread, canonical, when it is a T3 session not yet listed.
    fn add_candidate(&self, candidates: &mut Vec<SessionRef>, session: &SessionRef) {
        if session.surface != Surface::T3 {
            return;
        }
        let s = session.canonical(self.hosts.t3_environment().as_deref());
        if !candidates.contains(&s) {
            candidates.push(s);
        }
    }

    /// The stop bound through the hook's thread variables (BRO-3846): `T3CODE_THREAD_ID`
    /// (Claude), or the `session_id` a Codex Stop hook reads on stdin, resolved as every attributed
    /// write resolves it. No variable and no session named: never bound, and the stop goes through
    /// silently. With `named`, the variables' thread must be that session, once canonical. Variables
    /// that disagree, do not resolve, or name another thread than `named`, a registry that does not
    /// read, or a verification of the resolved thread that fails: the binding is unavailable
    /// ([`Run::unbound`]), with every thread known so far as a candidate. Strict identity binds
    /// nothing.
    fn bind(&mut self, named: Option<SessionRef>, input: &[u8]) {
        let unnamed = "no Maestro session is named (--session or MAESTRO_SESSION)";
        if self.hosts.identity_mode() == IdentityMode::Strict {
            return self.note(format!("{unnamed}, so there is no flow to gate."));
        }
        let each = self.hosts.resolve_each(&self.hook_vars(input));
        let host = self.hosts.t3_environment();
        let mut candidates = self.threads_of(&each);
        if let Some(n) = &named {
            self.add_candidate(&mut candidates, n);
        }
        let (a, environment) = match maestro_hosts::attribution::combine(&each) {
            Attributed::Absent => {
                let why = "and no thread variable binds this stop, so there is no flow to gate.";
                return self.note(format!("{unnamed} {why}"));
            }
            Attributed::Refused(refusal) => return self.unbound(&candidates, &refusal),
            Attributed::Resolved { attribution, environment } => (attribution, environment),
        };
        let thread = SessionRef { surface: Surface::T3, environment: environment.or(host), id: a.thread.clone() };
        if let Some(n) = &named
            && n.canonical(thread.environment.as_deref()) != thread
        {
            let why = format!("{} names T3 thread {}, but the stop's session is {n}", a.variable, a.thread);
            return self.unbound(&candidates, &IdentityRefusal::new(RefusalReason::ThreadMismatch, why));
        }
        // Credential first: a session that presents a credential or token is verified by it, in
        // full, and one whose credential does not parse is refused by that error; neither is ever
        // attributed. Any other is verified by its resolved thread.
        let named_or_thread = named.clone().unwrap_or_else(|| thread.clone());
        let me = match crate::presented_credential(self.claim, self.ambient) {
            Err(e) => return self.fail_open(Some(&named_or_thread), "caller_refused", &e.message),
            Ok(true) => {
                match crate::verified_caller(self.hosts, named.as_ref(), self.claim, self.ambient, Check::Write) {
                    Ok(Some(me)) => me,
                    Ok(None) => {
                        let why =
                            format!("session {named_or_thread} is not verified: no launch token or bridge credential");
                        return self.fail_open(Some(&named_or_thread), "caller_unverified", &why);
                    }
                    Err(e) => return self.refused_named(&named_or_thread, &candidates, e),
                }
            }
            Ok(false) => {
                let mut claim = maestro_hosts::CallerClaim::t3(Some(a.thread.clone()), None::<String>);
                claim.attribution = Some(a);
                match self.hosts.verify_caller(&claim) {
                    Ok(me) => me,
                    Err(r) => return self.unbound(&candidates, &r),
                }
            }
        };
        if let Err(why) = payload(input) {
            return self.fail_open(Some(&thread), "input_malformed", &why);
        }
        let session = me.session().clone();
        self.gated(&session, &me);
    }

    /// The binding is unavailable (BRO-3846): the stop goes through, `StopUnverified` is appended
    /// to every unfinished instance any candidate thread owns, and the fleet alert is filed, keyed
    /// by the workspace and the reason, as an unverified caller (a caller check would fail for the
    /// same reason). A record that fails still lets the stop through, and the alert names it.
    fn unbound(&mut self, candidates: &[SessionRef], refusal: &IdentityRefusal) {
        let code = refusal.reason.code();
        let message = refusal.message.trim_end_matches('.').to_string();
        self.note(format!(
            "binding_unavailable: {code}: {message}. The stop goes through (the gate fails open), recorded as StopUnverified."
        ));
        let host = self.hosts.t3_environment();
        let candidates = candidates.to_vec();
        let (mut made, mut failed) = (Vec::new(), Vec::new());
        if !candidates.is_empty()
            && let Ok(root) = crate::flow::flows_root(&self.paths.workspace)
        {
            let owned = Reader::new(&root, Options { lock_wait: kstop::REFUSALS_WAIT })
                .and_then(|r| r.lock())
                .map_err(KernelError::from)
                .and_then(|guard| kstop::owned_by_threads(&guard, &candidates, host.as_deref()));
            match owned {
                Ok((owned, unread)) => {
                    for u in unread {
                        failed.push(format!("{} ({})", u.instance, u.code));
                    }
                    let event = StopUnverified {
                        harness: self.gate.harness.name().to_string(),
                        reason: format!("{code}: {message}"),
                        candidates: candidates.clone(),
                        audit: Some(self.hosts.audit("unbound", None)),
                    };
                    for instance in owned {
                        let recorded =
                            maestro_flow::InstanceId::new(&instance).map_err(KernelError::from).and_then(|id| {
                                let mut ledger =
                                    self.ledger(&root).map_err(|(c, w)| KernelError::Encode(format!("{c}: {w}")))?;
                                kstop::record_unverified(&mut ledger, &id, &event)
                            });
                        match recorded {
                            Ok(Recorded::Appended(a)) => made.push((instance, a.event.seq)),
                            Ok(Recorded::Already { seq }) => {
                                self.note(format!("{instance} has not moved since its StopUnverified at event {seq}."))
                            }
                            Err(e) => {
                                let c = busy_code(e.code()).to_string();
                                self.note(format!("StopUnverified was not recorded on {instance} ({c}: {e})."));
                                failed.push(format!("{instance} ({c})"));
                            }
                        }
                    }
                }
                Err(missing) if missing.code() == "no_ledger" => {}
                Err(e) => failed.push(format!("the flow ledger ({}: {e})", busy_code(e.code()))),
            }
        }
        let session = candidates.first().cloned().unwrap_or_else(|| SessionRef {
            surface: Surface::T3,
            environment: None,
            id: "unverified".to_string(),
        });
        let day = maestro_flow::clock::now().to_string();
        let day = day.get(..10).unwrap_or_default();
        let mut key = format!("unverified\n{}\n{code}\n{day}", self.paths.workspace.display());
        let mut words = format!(
            "The flow stop gate could not tell which T3 thread was stopping ({code}: {message}), so it let the stop through without checking that thread's flow work."
        );
        let list = record_list(&made);
        if !list.is_empty() {
            words.push_str(&format!(" Recorded as StopUnverified on: {}.", list.join(", ")));
        }
        if !failed.is_empty() {
            words.push_str(&format!(" StopUnverified could not be recorded on: {}.", failed.join(", ")));
            key.push_str(&format!("\nunrecorded\n{}", failed.join("\n")));
        }
        self.out.recorded.extend(made);
        self.alert(&session, &key, UNCHECKED, &words, &Principal::Unverified);
    }

    /// The gate's judgment for a verified session `me`, named as `session`.
    fn gated(&mut self, session: &SessionRef, me: &VerifiedSession) {
        let session = session.clone();
        let me = me.clone();
        let root = match crate::flow::flows_root(&self.paths.workspace) {
            Ok(root) => root,
            Err(v) if v["code"] == "workspace_unready" => {
                return self.note(format!("{}; there is no flow to gate.", s(&v["reason"])));
            }
            Err(v) => return self.fail_open(Some(&session), s(&v["code"]), s(&v["reason"])),
        };
        let (open, counted) = match self.open(&root, &me) {
            Ok(open) => open,
            // No ledger here is no flow. Any other refusal (a root moved, or archived, which the
            // ledger reads as moved; a genesis left half made) is a ledger the gate cannot read: it
            // fails open, alerted.
            Err(e) if e.code() == "no_ledger" => {
                return self.note(format!("there is no flow ledger at {}, so no flow to gate.", root.display()));
            }
            Err(e) => {
                let code = busy_code(e.code());
                return self.fail_open(Some(&session), code, &e.to_string());
            }
        };
        // An instance that does not read is reported (a note and an alert) whatever else the gate
        // decides, so a broken log never goes unseen; it is named in a refusal too.
        let unread = unread_list(&open.unread);
        if let Some(list) = &unread {
            self.report_unread(&session, list);
        }
        let refusing: Vec<&Assessment> = open.refusing().collect();
        if refusing.is_empty() {
            // Nothing refused: clear interrupted refusal counts while preserving releases, even
            // when an instance is temporarily unread.
            if counted {
                let cleared =
                    self.ledger(&root).map_err(|(code, why)| format!("{code}: {why}")).and_then(|mut ledger| {
                        let guard = ledger.lock().map_err(|e| format!("{}: {e}", busy_code(e.code())))?;
                        let mut r = Refusals::load(&guard, &me).map_err(|e| format!("{}: {e}", busy_code(e.code())))?;
                        r.prune(&open);
                        r.tally(&[]);
                        r.save(&guard, &me).map_err(|e| format!("{}: {e}", busy_code(e.code())))
                    });
                if let Err(e) = cleared {
                    self.note(format!("the refusal counts were not cleared ({e})."));
                }
            }
            if unread.is_some() {
                self.note("instances_unread: no instance that did read has work, so the stop goes through (the gate fails open).");
            }
            return;
        }
        let ids: Vec<String> = refusing.iter().map(|a| a.instance.clone()).collect();
        let mut ledger = match self.ledger(&root) {
            Ok(l) => l,
            Err((code, why)) => return self.fail_open(Some(&session), &code, &why),
        };
        if self.gate.cannot_block {
            self.note(format!(
                "{} cannot refuse this stop: recording it on {}.",
                self.gate.harness.name(),
                ids.join(", ")
            ));
            return self.unblockable(&mut ledger, &me, &refusing, Unblockable::HarnessCannotBlock, &ids);
        }
        // The ledger's write lock is held from the load through the save: two stops at once are
        // both counted, and the lock's own checks (host, epoch, root) bind the counts. A lock still
        // held past its wait lets this stop through.
        let tally = {
            let guard = match ledger.lock() {
                Ok(g) => g,
                Err(e) => {
                    let code = if e.code() == "busy" { "refusals_busy" } else { "refusals_unread" };
                    return self.fail_open(Some(&session), code, &format!("{}: {e}", busy_code(e.code())));
                }
            };
            let mut counts = match Refusals::load(&guard, &me) {
                Ok(c) => c,
                Err(e) if matches!(e, KernelError::Encode(_)) => match Refusals::quarantine(&guard, &me) {
                    Ok(path) => {
                        let why = format!("refusal count file was corrupt: {e}; {path}; counting again from zero");
                        self.note(&why);
                        let key = format!("refusals_corrupt\n{session}\n{path}");
                        self.alert(&session, &key, UNCHECKED, &why, &Principal::Unverified);
                        match Refusals::load(&guard, &me) {
                            Ok(c) => c,
                            Err(e) => return self.block_unread_counts(&session, &e.to_string()),
                        }
                    }
                    Err(e) => return self.block_unread_counts(&session, &e.to_string()),
                },
                Err(e) => return self.block_unread_counts(&session, &e.to_string()),
            };
            counts.prune(&open);
            let tally = counts.tally(&refusing);
            if let Err(e) = counts.save(&guard, &me) {
                return self.fail_open(Some(&session), "refusals_unwritten", &format!("{}: {e}", busy_code(e.code())));
            }
            tally
        };
        if tally.refused.is_empty() {
            self.note(format!(
                "refused {} times in a row at the same position of {}: the stop goes through, recorded as no_progress.",
                kstop::MAX_REFUSALS,
                tally.spent.join(", ")
            ));
            let spent: Vec<&Assessment> = refusing.into_iter().filter(|a| tally.spent.contains(&a.instance)).collect();
            return self.unblockable(&mut ledger, &me, &spent, Unblockable::NoProgress, &tally.spent);
        }
        let blocking: Vec<&Assessment> = refusing.into_iter().filter(|a| tally.refused.contains(&a.instance)).collect();
        let mut reason = kstop::block_reason(&blocking, MAX_NAMED);
        if let Some(list) = &unread {
            reason.push_str(&format!(" Maestro could not read {list}, so the gate could not check it for work."));
        }
        self.out.stdout = self.gate.harness.block(&reason);
        self.out.blocked = true;
    }

    /// The ledger at `root` as this host's writer, waiting at most
    /// [`REFUSALS_WAIT`](kstop::REFUSALS_WAIT) for its lock: a code and why when it does not open
    /// (`foreign_host` on a ledger bound to another host).
    fn ledger(&self, root: &Path) -> Result<Ledger, (String, String)> {
        let host = crate::flow::host_id().map_err(|v| (s(&v["code"]).to_string(), s(&v["reason"]).to_string()))?;
        Ledger::open(root, &host, Options { lock_wait: kstop::REFUSALS_WAIT }).map_err(|e| {
            let code = busy_code(e.code());
            (code.to_string(), e.to_string())
        })
    }

    /// The session's open instances, and whether it has refusal counts to clear, under one shared
    /// lock. Counts that do not read are taken as there, so a clear is tried and its error noted.
    fn open(&self, root: &Path, me: &VerifiedSession) -> Result<(kstop::Open, bool), KernelError> {
        let guard = Reader::new(root, Options { lock_wait: kstop::REFUSALS_WAIT })?.lock()?;
        let now = maestro_flow::clock::now();
        let open = kstop::open(&guard, me, now, &|l, a| kernel::seen(self.paths, l, a))?;
        Ok((open, Refusals::any(&guard, me).unwrap_or(true)))
    }

    /// Records `StopUnblockable` on each of `on`, and files one alert for the records made.
    fn unblockable(
        &mut self,
        ledger: &mut Ledger,
        me: &VerifiedSession,
        on: &[&Assessment],
        reason: Unblockable,
        open_instances: &[String],
    ) {
        let session = me.session().clone();
        // Appended now; recorded by an earlier stop (its alert is filed again, idempotently, since
        // that stop may have failed to file it); and not recorded at all.
        let (mut made, mut already, mut failed) = (Vec::new(), Vec::new(), Vec::new());
        for a in on {
            let instance = match maestro_flow::InstanceId::new(&a.instance) {
                Ok(i) => i,
                Err(e) => {
                    self.note(format!("{} is not an instance id ({e}); nothing recorded on it.", a.instance));
                    failed.push(format!("{} (invalid)", a.instance));
                    continue;
                }
            };
            match kstop::record(ledger, me, &instance, self.gate.harness.name(), reason, open_instances) {
                Ok(Recorded::Appended(appended)) => made.push((a.instance.clone(), appended.event.seq)),
                Ok(Recorded::Already { seq }) => {
                    self.note(format!(
                        "{} has not moved since this session's record at event {seq}; not recorded again.",
                        a.instance
                    ));
                    already.push((a.instance.clone(), seq));
                }
                Err(e) => {
                    let code = busy_code(e.code());
                    self.note(format!("StopUnblockable was not recorded on {} ({code}: {e}).", a.instance));
                    failed.push(format!("{} ({code})", a.instance));
                }
            }
        }
        if made.is_empty() && already.is_empty() && failed.is_empty() {
            return;
        }
        let what = match reason {
            Unblockable::HarnessCannotBlock => format!(
                "Session {session} stopped while flow work was left, and its harness ({}) cannot refuse a stop.",
                self.gate.harness.name()
            ),
            Unblockable::NoProgress => format!(
                "Session {session} was refused a stop {} times in a row with no progress on its flow work, so the gate let it stop.",
                kstop::MAX_REFUSALS
            ),
        };
        let records: Vec<(String, u64)> = made.iter().chain(&already).cloned().collect();
        let list = record_list(&records);
        let mut words = what;
        // The key names the records alone, in order, so a record's alert has one id whichever stop
        // files it and however its records were found.
        let mut key = unblockable_key(&records);
        if !list.is_empty() {
            words.push_str(&format!(" Recorded as StopUnblockable on: {}.", list.join(", ")));
        }
        if !failed.is_empty() {
            words.push_str(&format!(
                " StopUnblockable could not be recorded on: {}; the stop went through all the same (the gate fails open).",
                failed.join(", ")
            ));
            let day = maestro_flow::clock::now().to_string();
            let day = day.get(..10).unwrap_or_default();
            key.push_str(&format!("\nunrecorded\n{session}\n{reason:?}\n{}\n{day}", failed.join("\n")));
        }
        if reason == Unblockable::NoProgress && !made.is_empty() {
            let saved = (|| -> Result<(), KernelError> {
                let guard = ledger.lock()?;
                let mut counts = Refusals::load(&guard, me)?;
                for (instance, seq) in &made {
                    counts.recorded_release(instance, *seq);
                }
                counts.save(&guard, me)
            })();
            if let Err(e) = saved {
                self.note(format!("the release position could not be saved ({}: {e}).", busy_code(e.code())));
            }
        }
        self.out.recorded.extend(made);
        self.alert(&session, &key, LEFT_WORK, &words, &Principal::Unverified);
    }

    /// Files the alert ask, once per `key` (see the module docs), straight through the asks
    /// ledger's writer of record as `caller`, never through a caller check (BRO-3846): an alert
    /// reports that the gate could not check or keep a session, often because its claim did not
    /// verify, so a caller check would refuse the alert for the reason it reports. Every call site
    /// files as [`Principal::Unverified`]: the gate vouches for no one.
    fn alert(&mut self, session: &SessionRef, key: &str, headline: &str, words: &str, caller: &Principal) {
        let id = alert_id(key);
        let ask = alert_ask(&id, headline, words);
        let store = Store::new(self.paths.clone(), TreeSource::WorkingTree);
        let filed = crate::file_ask_as(&store, session.clone(), caller, &self.gate.alert_ledger, &ask);
        match filed {
            Ok(_) => {
                self.note(format!("filed alert ask {id} in {}.", self.gate.alert_ledger));
                self.out.alerts.push(id);
            }
            Err(e) if e.message.contains("already has an ask") => {
                self.note(format!("alert ask {id} was filed before."));
                self.out.alerts.push(id);
            }
            Err(e) => self.note(format!("the alert was not filed in {}: {}", self.gate.alert_ledger, e.message)),
        }
    }
}

/// The instances that did not read, each with its code, at most [`MAX_NAMED`]; none when all read.
fn unread_list(unread: &[kstop::Unread]) -> Option<String> {
    let mut named: Vec<String> =
        unread.iter().take(MAX_NAMED).map(|u| format!("{} ({})", u.instance, u.code)).collect();
    if unread.len() > MAX_NAMED {
        named.push(format!("{} more", unread.len() - MAX_NAMED));
    }
    (!named.is_empty()).then(|| named.join(", "))
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

/// The records an alert names, sorted, as `<instance> (event <seq>)`.
fn record_list(records: &[(String, u64)]) -> Vec<String> {
    let mut sorted = records.to_vec();
    sorted.sort();
    sorted.iter().map(|(i, seq)| format!("{i} (event {seq})")).collect()
}

/// What an alert for `StopUnblockable` records is about: the records, in order whatever order
/// they were found in, so the same records always give the same alert id.
pub fn unblockable_key(records: &[(String, u64)]) -> String {
    format!("unblockable\n{}", record_list(records).join("\n"))
}

/// `stop-gate-<12 hex>`, a digest of what the alert is about.
pub fn alert_id(key: &str) -> String {
    let d = digest(&json!(key));
    format!("stop-gate-{}", d.trim_start_matches("sha256:").get(..12).unwrap_or_default())
}

/// Every alert's summary: plain sentences within the asks validator's bounds (40 to 700 characters).
pub const SUMMARY: &str = "The flow stop gate let a session stop while it could not keep the session on its flow work, or could not check that work. The instances and the reason are below. Resuming the session, or looking into the reason, is what this asks.";

/// An alert's headline: a session stopped with work left.
pub const LEFT_WORK: &str = "A session stopped with flow work left: should someone resume it?";

/// An alert's headline: the gate could not check a session (it failed open).
pub const UNCHECKED: &str = "The flow stop gate could not check a session: should someone look at it?";

/// The alert's ask fields, as the asks ledger's validator of record requires them for an open ask
/// (checked against the workspace's `ask_ledger.py validate`).
pub fn alert_ask(id: &str, headline: &str, words: &str) -> Map<String, Value> {
    let ask = json!({
        "id": id,
        "headline": headline,
        // Always given: the validator of record requires one whenever any of the ask's words (a
        // session's name, a reason quoted from an error) reads as a spec or design word.
        "summary": SUMMARY,
        "ask": words,
        "stakes": "A flow instance may stay open and unfinished until a session picks it up again.",
        "why_you": "The stop gate could not keep the session working, so only you can send someone back to it.",
        "options": [
            {
                "id": "look",
                "label": "Look into it",
                "consequence": "Someone checks the session's open flow instances and resumes the work left, if any.",
                "reversible": true,
                "recommended": true,
            },
            {
                "id": "leave",
                "label": "Leave it",
                "consequence": "Nothing changes; the gate files another alert if it lets another such stop through.",
                "reversible": true,
                "status_quo": true,
            },
        ],
        "default": "leave",
        "class": "decision",
        "gates": [],
        "exhausted": [
            "loop: the stop gate refused the stop while the harness let it",
            "default: the session stopped, so its flow work is left open",
        ],
        "blocking": false,
        "preanswerable": false,
        "project": "maestro",
        "topics": ["stop-gate"],
    });
    ask.as_object().cloned().unwrap_or_default()
}

/// Whether a harness's settings wire the gate: what `--check-install` reports.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Install {
    pub harness: &'static str,
    pub settings: PathBuf,
    pub wired: bool,
    /// The Stop hook commands that run the gate for this harness.
    pub commands: Vec<String>,
    pub detail: String,
}

/// Whether `name` is a shell identifier matching `^[A-Za-z_][A-Za-z0-9_]*$`.
fn is_ident(name: &[u8]) -> bool {
    let mut chars = name.iter().copied();
    let head_ok = chars.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_');
    let tail_ok = chars.all(|b| b.is_ascii_alphanumeric() || b == b'_');
    head_ok && tail_ok
}

/// Whether `word` is a `NAME=value` assignment matching `^[A-Za-z_][A-Za-z0-9_]*=`.
fn is_env_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| is_ident(name.as_bytes()))
}

/// Byte length of a `$NAME` or `${NAME}` variable expansion starting at `bytes[0] == b'$'`.
fn dollar_var_len(bytes: &[u8]) -> Option<usize> {
    if bytes.get(1) == Some(&b'{') {
        let end = 2 + bytes[2..].iter().take_while(|b| **b != b'}').count();
        (bytes.get(end) == Some(&b'}') && is_ident(&bytes[2..end])).then_some(end + 1)
    } else {
        let end = 1 + bytes[1..].iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_').count();
        is_ident(&bytes[1..end]).then_some(end)
    }
}

/// Scans `command` once, tracking quotes: refuses unquoted shell control characters, cuts at the
/// first unquoted `|`, and counts leading unquoted words before the first quote or escape.
fn scan_command(command: &str) -> Option<(&str, usize)> {
    let bytes = command.as_bytes();
    let (mut i, mut end, mut unquoted_prefix, mut in_prefix) = (0, None, 0, true);
    while i < bytes.len() {
        if matches!(bytes[i], b' ' | b'\t') {
            i += 1;
            continue;
        }
        let word_start = i;
        let mut word_quoted = false;
        while i < bytes.len() && !matches!(bytes[i], b' ' | b'\t') {
            let is_pipe = bytes[i] == b'|';
            let is_amp = bytes[i] == b'&';
            let is_semi = bytes[i] == b';';
            let is_redir = matches!(bytes[i], b'<' | b'>');
            let is_sub = bytes[i] == b'$' && dollar_var_len(&bytes[i..]).is_none();
            let is_paren = matches!(bytes[i], b'(' | b')');
            let is_nl = matches!(bytes[i], b'\n' | b'\r');
            let is_tick = bytes[i] == b'`';
            if is_amp || is_semi || is_redir || is_sub || is_paren || is_nl || is_tick {
                return None;
            }
            if is_pipe {
                break;
            }
            match bytes[i] {
                b'$' => i += dollar_var_len(&bytes[i..]).unwrap_or(1),
                b'\'' => {
                    word_quoted = true;
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'\'' {
                        i += 1;
                    }
                    i += usize::from(i < bytes.len());
                }
                b'"' => {
                    word_quoted = true;
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        i += 1 + usize::from(bytes[i] == b'\\' && i + 1 < bytes.len());
                    }
                    i += usize::from(i < bytes.len());
                }
                b'\\' => {
                    word_quoted = true;
                    i += 1 + usize::from(i + 1 < bytes.len());
                }
                _ => i += 1,
            }
        }
        if end.is_none() && i > word_start {
            if in_prefix && !word_quoted {
                unquoted_prefix += 1;
            } else {
                in_prefix = false;
            }
        }
        if i < bytes.len() && bytes[i] == b'|' {
            if end.replace(i).is_some() {
                return None;
            }
            i += 1;
        }
    }
    // Allow `| tee -a <path>` because the conformance runner captures the hook's output that way.
    if let Some(at) = end
        && !shlex::split(&command[at + 1..])
            .is_some_and(|w| matches!(w.iter().map(String::as_str).collect::<Vec<_>>().as_slice(), ["tee", "-a", _]))
    {
        return None;
    }
    Some((&command[..end.unwrap_or(bytes.len())], unquoted_prefix))
}

/// Whether `command` runs this gate for `harness`.
fn runs_gate(command: &str, harness: Harness) -> bool {
    // The program is the first word after any `env` and unquoted `NAME=value` words: maestro itself.
    let Some((head, unquoted_prefix)) = scan_command(command) else { return false };
    let parsed = shlex::split(head);
    let Some(parsed) = parsed else { return false };
    let mut words = parsed
        .iter()
        .map(String::as_str)
        .enumerate()
        .skip_while(|(i, w)| *i < unquoted_prefix && (*w == "env" || is_env_assignment(w)))
        .map(|(_, w)| w);
    let maestro = words.next().is_some_and(|w| Path::new(w).file_name().is_some_and(|n| n == "maestro"));
    let words: Vec<&str> = words.collect();
    let flag = words.windows(2).any(|w| w == ["--harness", harness.name()])
        || words.contains(&format!("--harness={}", harness.name()).as_str());
    let verb = words.windows(2).any(|w| w == ["flow", "stop-gate"]);
    maestro && verb && flag && !words.iter().any(|w| *w == "--check-install" || w.starts_with("--check-install="))
}

/// Reads `settings` (Claude Code's `settings.json`, or Codex's `hooks.json`) for a Stop hook that
/// runs the gate for `harness`. Both name hooks the same way: `hooks.Stop[].hooks[]`, each
/// `{"type": "command", "command": ...}`.
pub fn check_install(harness: Harness, settings: &Path) -> Install {
    let mut install = Install {
        harness: harness.name(),
        settings: settings.to_path_buf(),
        wired: false,
        commands: Vec::new(),
        detail: String::new(),
    };
    // Read to one byte past the bound, as the gate reads its standard input.
    let read = std::fs::File::open(settings).and_then(|f| {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::Read::take(f, MAX_INPUT as u64 + 1), &mut bytes).map(|_| bytes)
    });
    let text = match read {
        Ok(bytes) if bytes.len() <= MAX_INPUT => bytes,
        Ok(_) => {
            install.detail = format!("{} is over {MAX_INPUT} bytes", settings.display());
            return install;
        }
        Err(e) => {
            install.detail = format!("{} does not read ({e})", settings.display());
            return install;
        }
    };
    let v: Value = match serde_json::from_slice(&text) {
        Ok(v) => v,
        Err(e) => {
            install.detail = format!("{} is not JSON ({e})", settings.display());
            return install;
        }
    };
    let stops = v.get("hooks").and_then(|h| h.get("Stop")).and_then(Value::as_array).cloned().unwrap_or_default();
    for group in &stops {
        for hook in group.get("hooks").and_then(Value::as_array).into_iter().flatten() {
            let command = hook.get("command").and_then(Value::as_str).unwrap_or_default();
            if hook.get("type").and_then(Value::as_str) == Some("command") && runs_gate(command, harness) {
                install.commands.push(command.to_string());
            }
        }
    }
    install.wired = !install.commands.is_empty();
    install.detail = match (install.wired, harness) {
        (false, _) => format!("no Stop hook in {} runs maestro flow stop-gate --harness {}", settings.display(), harness.name()),
        (true, Harness::Claude) => "wired".to_string(),
        (true, Harness::Codex) => {
            "wired; Codex runs a hook only once it is trusted (/hooks, or hooks.state in config.toml), which this check does not read".to_string()
        }
    };
    install
}
