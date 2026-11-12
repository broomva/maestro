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
//!   verify, a ledger or an instance that does not read, or refusal counts that cannot be kept let
//!   the stop through, say why on standard error, and file the alert when they can. By design: a
//!   broken ledger must not trap every session. No session named, or no flow ledger in the
//!   workspace, is no Maestro flow at all: the stop goes through with a note and no alert.
//! - **The loop bound.** A session refused at one instance's log position
//!   [`MAX_REFUSALS`](maestro_flow::kernel::stop::MAX_REFUSALS) times in a row is let through the
//!   next time: the gate appends `StopUnblockable` with reason `no_progress` to each such instance
//!   and files the alert (`kernel::stop::Refusals`).
//! - **A harness that cannot block** (`--cannot-block`, for a turn-end program that cannot refuse):
//!   the gate appends `StopUnblockable` with reason `harness_cannot_block` to each instance with
//!   work and files the alert. Claude Code and Codex 0.164 both block (README, harness matrix).
//!
//! # The alert
//!
//! A fleet alert is a Maestro ask, filed through `maestro_ask_file`, the agent path every ask takes
//! (its writer of record, `ask_ledger.py file`, validates it), into `--alert-ledger`. An ask is
//! what the owner's Decisions view lists; an escalation is recorded in the requests journal, which
//! no screen shows. Its id is a digest of what it is about, so the same alert is filed once: for a
//! `StopUnblockable`, its instance and position; for a failure, the session, the code and the day.

use crate::Ambient;
use maestro_core::request::RequestKind;
use maestro_core::session::{SessionRef, VerifiedSession};
use maestro_flow::kernel::events::{Unblockable, digest};
use maestro_flow::kernel::stop::{self as kstop, Assessment, Recorded, Refusals};
use maestro_flow::kernel::{self, KernelError};
use maestro_flow::{Ledger, Options, Reader};
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

impl Run<'_> {
    fn note(&mut self, text: impl Into<String>) {
        self.out.notes.push(format!("maestro stop-gate: {}", text.into()));
    }

    /// Lets the stop through on a failure (fail open), files the alert when it can.
    fn fail_open(&mut self, session: Option<&SessionRef>, code: &str, reason: &str) {
        self.note(format!("{code}: {reason}. The stop goes through (the gate fails open)."));
        match session {
            Some(s) => {
                let day = maestro_flow::clock::now().to_string();
                let key = format!("fail_open\n{s}\n{code}\n{}", day.get(..10).unwrap_or_default());
                let words = format!(
                    "The flow stop gate could not check session {s}, so it let the session stop without knowing whether a flow instance it owns had work left. Why: {code}, {reason}."
                );
                self.alert(s, &key, UNCHECKED, &words);
            }
            None => self.note("no session to file the alert as, so none was filed."),
        }
    }

    fn judge(&mut self, input: &[u8]) {
        let named = self.gate.session.clone().filter(|s| !s.trim().is_empty());
        let Some(text) = named else {
            self.note("no Maestro session is named (--session or MAESTRO_SESSION), so there is no flow to gate.");
            return;
        };
        let session: SessionRef = match text.parse() {
            Ok(s) => s,
            Err(e) => return self.fail_open(None, "session_invalid", &format!("{text:?} is not a session ({e})")),
        };
        if let Err(e) = payload(input) {
            return self.fail_open(Some(&session), "input_malformed", &e);
        }
        let me = match crate::verified_caller(self.hosts, &session, self.claim, self.ambient) {
            Ok(Some(me)) => me,
            Ok(None) => {
                let why = format!("session {session} is not verified: no launch token or bridge credential");
                return self.fail_open(Some(&session), "caller_unverified", &why);
            }
            Err(e) => return self.fail_open(Some(&session), "caller_refused", &e.message),
        };
        let root = match crate::flow::flows_root(&self.paths.workspace) {
            Ok(root) => root,
            Err(v) if v["code"] == "workspace_unready" => {
                return self.note(format!("{}; there is no flow to gate.", s(&v["reason"])));
            }
            Err(v) => return self.fail_open(Some(&session), s(&v["code"]), s(&v["reason"])),
        };
        if std::fs::symlink_metadata(&root).is_err() {
            return self.note(format!("there is no flow ledger at {}, so no flow to gate.", root.display()));
        }
        let open = match self.open(&root, &me) {
            Ok(open) => open,
            Err(e) => return self.fail_open(Some(&session), e.code(), &e.to_string()),
        };
        let refusing: Vec<&Assessment> = open.refusing().collect();
        if refusing.is_empty() {
            if let Some(u) = open.unread.first() {
                let why = format!(
                    "{} instance(s) did not read, {} first ({}: {})",
                    open.unread.len(),
                    u.instance,
                    u.code,
                    u.reason
                );
                return self.fail_open(Some(&session), "instances_unread", &why);
            }
            // Nothing refused: a count in a row is broken.
            if let Err(e) = Refusals::load(&root, &me).and_then(|mut r| {
                r.tally(&[]);
                r.save(&me)
            }) {
                self.note(format!("the refusal counts were not cleared ({}: {e}).", e.code()));
            }
            return;
        }
        let ids: Vec<String> = refusing.iter().map(|a| a.instance.clone()).collect();
        if self.gate.cannot_block {
            self.note(format!(
                "{} cannot refuse this stop: recording it on {}.",
                self.gate.harness.name(),
                ids.join(", ")
            ));
            return self.unblockable(&root, &me, &refusing, Unblockable::HarnessCannotBlock, &ids);
        }
        let mut counts = match Refusals::load(&root, &me) {
            Ok(c) => c,
            Err(e) => return self.fail_open(Some(&session), "refusals_unread", &format!("{}: {e}", e.code())),
        };
        let tally = counts.tally(&refusing);
        if let Err(e) = counts.save(&me) {
            return self.fail_open(Some(&session), "refusals_unwritten", &format!("{}: {e}", e.code()));
        }
        if tally.refused.is_empty() {
            self.note(format!(
                "refused {} times in a row at the same position of {}: the stop goes through, recorded as no_progress.",
                kstop::MAX_REFUSALS,
                tally.spent.join(", ")
            ));
            let spent: Vec<&Assessment> = refusing.into_iter().filter(|a| tally.spent.contains(&a.instance)).collect();
            return self.unblockable(&root, &me, &spent, Unblockable::NoProgress, &tally.spent);
        }
        let blocking: Vec<&Assessment> = refusing.into_iter().filter(|a| tally.refused.contains(&a.instance)).collect();
        self.out.stdout = self.gate.harness.block(&kstop::block_reason(&blocking, MAX_NAMED));
        self.out.blocked = true;
    }

    fn open(&self, root: &Path, me: &VerifiedSession) -> Result<kstop::Open, KernelError> {
        let guard = Reader::new(root, Options::default())?.lock()?;
        let now = maestro_flow::clock::now();
        kstop::open(&guard, me, now, &|l, a| kernel::seen(self.paths, l, a))
    }

    /// Records `StopUnblockable` on each of `on`, and files one alert for the records made.
    fn unblockable(
        &mut self,
        root: &Path,
        me: &VerifiedSession,
        on: &[&Assessment],
        reason: Unblockable,
        open_instances: &[String],
    ) {
        let session = me.session().clone();
        let mut ledger = match crate::flow::host_id()
            .map_err(|v| format!("{}: {}", s(&v["code"]), s(&v["reason"])))
            .and_then(|host| Ledger::open(root, &host, Options::default()).map_err(|e| format!("{}: {e}", e.code())))
        {
            Ok(l) => l,
            Err(e) => return self.fail_open(Some(&session), "ledger_unopened", &e),
        };
        let mut made = Vec::new();
        for a in on {
            let instance = match maestro_flow::InstanceId::new(&a.instance) {
                Ok(i) => i,
                Err(e) => {
                    self.note(format!("{} is not an instance id ({e}); nothing recorded on it.", a.instance));
                    continue;
                }
            };
            match kstop::record(&mut ledger, me, &instance, self.gate.harness.name(), reason, open_instances) {
                Ok(Recorded::Appended(appended)) => made.push((a.instance.clone(), appended.event.seq)),
                Ok(Recorded::Already { seq }) => self.note(format!(
                    "{} has not moved since this session's record at event {seq}; not recorded again.",
                    a.instance
                )),
                Err(e) => self.note(format!("StopUnblockable was not recorded on {} ({}: {e}).", a.instance, e.code())),
            }
        }
        if made.is_empty() {
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
        let list: Vec<String> = made.iter().map(|(i, seq)| format!("{i} (event {seq})")).collect();
        let words = format!("{what} Recorded as StopUnblockable on: {}.", list.join(", "));
        let key = format!("unblockable\n{}", list.join("\n"));
        self.out.recorded.extend(made);
        self.alert(&session, &key, LEFT_WORK, &words);
    }

    /// Files the alert ask, once per `key` (see the module docs).
    fn alert(&mut self, session: &SessionRef, key: &str, headline: &str, words: &str) {
        let id = alert_id(key);
        let mut args = match self.claim.as_object() {
            Some(o) => o.clone(),
            None => Map::new(),
        };
        args.insert("session".into(), json!(session.to_string()));
        args.insert("ledger".into(), json!(self.gate.alert_ledger));
        args.insert("ask".into(), Value::Object(alert_ask(&id, headline, words)));
        let store = Store::new(self.paths.clone(), TreeSource::WorkingTree);
        match crate::agent_request_in(&store, self.hosts, self.ambient, RequestKind::AskFile, Value::Object(args)) {
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

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

/// `stop-gate-<12 hex>`, a digest of what the alert is about.
pub fn alert_id(key: &str) -> String {
    let d = digest(&json!(key));
    format!("stop-gate-{}", d.trim_start_matches("sha256:").get(..12).unwrap_or_default())
}

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

/// Whether `command` runs this gate for `harness`.
fn runs_gate(command: &str, harness: Harness) -> bool {
    // The program is the first word after any `env` and `NAME=value` words: maestro itself.
    let mut words = command.split_whitespace().skip_while(|w| *w == "env" || w.contains('='));
    let maestro = words.next().is_some_and(|w| Path::new(w).file_name().is_some_and(|n| n == "maestro"));
    let words: Vec<&str> = words.collect();
    let flag = words.windows(2).any(|w| w == ["--harness", harness.name()])
        || words.contains(&format!("--harness={}", harness.name()).as_str());
    let verb = words.windows(2).any(|w| w == ["flow", "stop-gate"]);
    maestro && verb && flag && !words.contains(&"--check-install")
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
    let text = match std::fs::read(settings) {
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
