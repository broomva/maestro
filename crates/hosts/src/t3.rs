//! T3 thread identity adapter (`statev2.sqlite` and JSON thread snapshots).
//!
//! Reads T3's `orchestration_v2_projection_threads` / `projection_threads`,
//! `projection_projects`, `orchestration_v2_projection_runs`, and
//! `orchestration_v2_projection_runtime_requests` (or a JSON snapshot of the same shape)
//! to verify a T3 thread's existence, liveness, working directory (`worktreePath` or
//! project `workspace_root`), environment id, and pending permission prompts.

use crate::paseo::{RawRuntimeRequest, check_peer, decode_permissions, decode_runtime_requests, short_id, within};
use crate::read::{self, Read};
use maestro_core::session::{
    BridgeDiscovery, BridgeTransport, CallerClaim, Harness, IdentityRefusal, NodeId, PermissionPrompt, QuestionPrompt,
    RefusalReason, SessionLiveness, SessionRef, Surface, VerifiedSession,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The longest T3 thread id Maestro reads (BRO-3846): live delegated ids run 129 to 206 bytes.
pub const MAX_T3_ID: usize = 512;

/// Whether `id` is a plausible T3 thread id: `mcp:<uuid>`, `<uuid>`, or a percent-encoded composite
/// such as `mcp:launch:abc%3Adef`. A leading alphanumeric, then alphanumerics and `-_:.%`, at most
/// [`MAX_T3_ID`] bytes, never `/`, `..` or NUL. Ids are opaque: never percent-decoded, used only as
/// SQL parameters and in comparisons, never as a path, so `%2F` is just three bytes.
pub fn is_safe_t3_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_T3_ID {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    // The charset has no `/` and no NUL; `..` is the one sequence of allowed bytes refused.
    !id.contains("..")
        && bytes[1..].iter().all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'-' | b'_' | b':' | b'.' | b'%'))
}

/// What Maestro reads of a T3 thread record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct T3Thread {
    pub id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Every provider the thread has run under, as the registry's provider-thread rows name them
    /// (BRO-3828); `None` when the registry reports none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<String>>,
    /// The driver each provider-thread row records it ran, in the order of `providers`
    /// (BRO-4018); `None` when the registry records none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_drivers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_permissions: Vec<PermissionPrompt>,
    /// Pending permission records that did not decode, each with why (BRO-3863), and waiting
    /// questions whose cards did not read (BRO-3936).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread_permissions: Vec<String>,
    /// AskUserQuestion cards waiting for the owner in this thread (BRO-3936).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_questions: Vec<QuestionPrompt>,
}

impl T3Thread {
    pub fn liveness(&self) -> SessionLiveness {
        if self.deleted_at.is_some()
            || self.status.as_deref() == Some("closed")
            || self.status.as_deref() == Some("deleted")
        {
            SessionLiveness::Closed
        } else if self.archived_at.is_some() || self.status.as_deref() == Some("archived") {
            SessionLiveness::Archived
        } else if self.settled_at.is_some() && self.status.as_deref() != Some("running") {
            SessionLiveness::Settled
        } else {
            SessionLiveness::Live
        }
    }

    pub fn harness(&self) -> Option<Harness> {
        self.provider.as_deref().map(harness_of)
    }

    /// The harness the thread did its work under, attested (BRO-3828): every provider-thread row
    /// it has, active or retired, names one provider. A thread that switched providers, or whose
    /// rows the registry does not report, has none; its default provider is never taken for it.
    /// The harness a verified session of this thread runs (BRO-3828): with provider rows reported,
    /// the one provider they all name ([`Self::performed_harness`]), else none; a registry that
    /// reports no rows (a JSON snapshot, a schema without the column) leaves the thread's default.
    /// The thread's default provider is not what ran it once it has switched.
    pub fn session_harness(&self) -> Option<Harness> {
        match &self.providers {
            Some(_) => self.performed_harness(),
            None => self.harness(),
        }
    }

    pub fn performed_harness(&self) -> Option<Harness> {
        let rows = self.providers.as_ref()?;
        let first = rows.first().filter(|p| !p.is_empty())?;
        if !rows.iter().all(|p| p == first) {
            return None;
        }
        // The driver each row recorded, when the registry records it (BRO-4018): every row must
        // name the same one, and a row that names none attests nothing.
        match &self.provider_drivers {
            None => Some(harness_of(first)),
            Some(drivers) => {
                let driver = drivers.first().filter(|d| !d.is_empty())?;
                (drivers.len() == rows.len() && drivers.iter().all(|d| d == driver))
                    .then(|| harness_of_driver(first, driver))
            }
        }
    }
}

/// The harness a T3 provider instance ran, from the driver its provider-thread row recorded
/// (BRO-4018). The instance id is the owner's name for it (`codex_team`); the driver is what ran.
/// An `acpRegistry` instance names no agent in its driver, and one may run any model (`holon`
/// runs Claude), so only the instances [`acp_harness`] names are attested; any other driver
/// attests none.
///
/// What this attests is what T3 records the thread ran: the instance and its driver. Which agent
/// an ACP instance runs, and what an instance named like a provider runs when no driver is
/// recorded, are the owner's T3 configuration, as is the instance's name. That configuration is a
/// user-level write, the same that can replace Maestro's binary: the floor the owner accepted as
/// residual risk (BRO-3635). Attestation stops an agent misreporting its provider; it does not
/// defend against the owner mislabelling an instance (P20 round 2 on maestro#61).
fn harness_of_driver(instance: &str, driver: &str) -> Harness {
    match driver {
        "codex" => Harness::Codex,
        "claudeAgent" => Harness::ClaudeCode,
        "antigravity" => Harness::Agy,
        "grok" => Harness::Other("grok".to_string()),
        "acpRegistry" => acp_harness(instance),
        other => Harness::Other(format!("{other}:{instance}")),
    }
}

/// The `acpRegistry` instances whose agent is known: `gemini-vertex` runs the Gemini CLI.
fn acp_harness(instance: &str) -> Harness {
    match instance {
        "gemini-vertex" => Harness::Gemini,
        other => Harness::Other(format!("acpRegistry:{other}")),
    }
}

/// A T3 provider name as the harness it runs.
fn harness_of(raw: &str) -> Harness {
    match raw {
        "claudeAgent" | "claude-code" | "claude" => Harness::ClaudeCode,
        "codex" => Harness::Codex,
        "gemini-vertex" | "gemini" => Harness::Gemini,
        "antigravity" | "agy" => Harness::Agy,
        other => Harness::Other(other.to_string()),
    }
}

impl T3Thread {
    pub fn incarnation(&self) -> Option<String> {
        let created = self.created_at.as_deref().filter(|s| !s.is_empty());
        let run = self.run_id.as_deref().filter(|s| !s.is_empty());
        if created.is_none() && run.is_none() {
            return None;
        }
        Some(format!(
            "t3:{}:{}:{}:{}:{}",
            self.cwd,
            self.environment_id.as_deref().unwrap_or("-"),
            self.provider.as_deref().unwrap_or("-"),
            created.unwrap_or("-"),
            run.unwrap_or("-"),
        ))
    }
}

#[derive(Deserialize)]
struct RawT3Thread {
    id: String,
    cwd: String,
    #[serde(default)]
    environment_id: Option<String>,
    #[serde(default)]
    node: Option<NodeId>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default, alias = "createdAt")]
    created_at: Option<String>,
    #[serde(default, alias = "runId", alias = "session_id", alias = "sessionId")]
    run_id: Option<String>,
    #[serde(default, alias = "archivedAt")]
    archived_at: Option<String>,
    #[serde(default, alias = "deletedAt")]
    deleted_at: Option<String>,
    #[serde(default)]
    settled_at: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    pending_permissions: Option<Vec<Value>>,
    /// The thread's pending runtime requests, raw, as the SQLite reader emits them (BRO-3863).
    #[serde(default)]
    runtime_requests: Vec<RawRuntimeRequest>,
    /// The thread's pending `user_input` requests (AskUserQuestion cards), raw, each with its
    /// card's turn-item payload when one was found (BRO-3936). Raw values, as permission records
    /// are: one that does not decode is unread, and never fails the thread's row (P20 r1).
    #[serde(default)]
    pending_questions: Option<Vec<Value>>,
    /// The thread's active provider-thread row (`active_provider_thread_id`).
    #[serde(default)]
    active_provider_thread_id: Option<String>,
    /// Every provider its provider-thread rows name (BRO-3828).
    #[serde(default)]
    providers: Option<Vec<String>>,
    /// The driver each of those rows ran, in the same order (BRO-4018).
    #[serde(default)]
    provider_drivers: Option<Vec<String>>,
    /// The thread's provider-thread rows, active or retired, each with its native id
    /// (`payload_json.$.nativeThreadRef.nativeId`), as `orchestration_v2_projection_provider_threads`
    /// holds them.
    #[serde(default)]
    provider_threads: Vec<RawProviderThread>,
    /// Set by the listing only (BRO-3846): why the thread's payload did not read. The thread is
    /// listed from what is known, its permission prompts included, and the listing reports it.
    #[serde(default)]
    unreadable: Option<String>,
}

/// How much of a card's first question a need's title carries.
const QUESTION_DETAIL_CHARS: usize = 160;

/// One field of a question request or of a card's question, as the one rule every field follows
/// reads it (P20 narrowed round 2, hoisted): a non-blank string reads; absent, null or blank is
/// missing; any other value does not read. Each caller says whether a missing field is unread.
enum Field {
    Text(String),
    Missing,
    NotText,
}

fn field(record: &serde_json::Map<String, Value>, key: &str) -> Field {
    match record.get(key) {
        None | Some(Value::Null) => Field::Missing,
        Some(Value::String(s)) if s.trim().is_empty() => Field::Missing,
        Some(Value::String(s)) => Field::Text(s.clone()),
        Some(_) => Field::NotText,
    }
}

/// A card's question, read field by field: its header and text where they read, and a reason
/// for every field that did not. Both fields are required: a card question always has them.
struct Question {
    header: Option<String>,
    text: Option<String>,
    unread: Vec<String>,
}

fn read_question(n: usize, q: &Value) -> Question {
    let Some(q) = q.as_object() else {
        return Question { header: None, text: None, unread: vec![format!("question {n} is not an object")] };
    };
    let mut unread = Vec::new();
    let mut required = |key: &str| match field(q, key) {
        Field::Text(s) => Some(s),
        Field::Missing => {
            unread.push(format!("question {n} has no {key}"));
            None
        }
        Field::NotText => {
            unread.push(format!("question {n} has a {key} that is not text"));
            None
        }
    };
    let header = required("header");
    let text = required("question");
    Question { header, text, unread }
}

/// A thread's waiting AskUserQuestion cards as prompts (BRO-3936). Every field is read by
/// [`field`], so one rule decides what read and what did not. A request is dropped only when it
/// has no id to key it by, and that is reported unread. A field that did not read anywhere else is
/// reported unread by name, and the need stays: a card that reads in part is listed from what
/// read, and one with no readable question is listed as a card that did not read.
fn decode_questions(raw: Vec<Value>, thread: &str) -> (Vec<QuestionPrompt>, Vec<String>) {
    let mut prompts = Vec::new();
    let mut unread = Vec::new();
    for (i, value) in raw.into_iter().enumerate() {
        let Some(record) = value.as_object() else {
            unread.push(format!("question request {i} of thread {thread} is not an object"));
            continue;
        };
        let Field::Text(id) = field(record, "id") else {
            unread.push(format!("question request {i} of thread {thread} has no id"));
            continue;
        };
        let at = format!("question request {id} of thread {thread}");
        let asked_at = match field(record, "created_at") {
            Field::Text(s) => Some(s),
            Field::Missing => None,
            Field::NotText => {
                unread.push(format!("{at} has a created_at that is not text"));
                None
            }
        };
        let card = match field(record, "item_payload_json") {
            Field::Missing => Err("has no card (no user_input_request item)"),
            Field::NotText => Err("has a card that is not text"),
            Field::Text(text) => match serde_json::from_str::<Value>(&text) {
                Err(_) => Err("has a card that does not parse"),
                Ok(v) => match v.get("questions") {
                    Some(Value::Array(qs)) if !qs.is_empty() => {
                        Ok(qs.iter().enumerate().map(|(n, q)| read_question(n + 1, q)).collect::<Vec<_>>())
                    }
                    Some(Value::Array(_)) | None => Err("has a card with no questions"),
                    Some(_) => Err("has a card whose questions are not a list"),
                },
            },
        };
        let (title, detail) = match card {
            Ok(qs) => {
                let missing: Vec<String> = qs.iter().flat_map(|q| q.unread.iter().cloned()).collect();
                if qs.iter().any(|q| q.text.is_some()) {
                    if !missing.is_empty() {
                        unread.push(format!("{at} has fields that did not read: {}", missing.join("; ")));
                    }
                    describe_questions(&qs)
                } else {
                    unread.push(format!("{at} has a card with no readable question: {}", missing.join("; ")));
                    ("Question waiting (its card did not read)".to_string(), None)
                }
            }
            Err(why) => {
                unread.push(format!("{at} {why}"));
                ("Question waiting (its card did not read)".to_string(), None)
            }
        };
        prompts.push(QuestionPrompt { request_id: id, title, detail, asked_at });
    }
    (prompts, unread)
}

/// `Question: <header> · <header>` and the first question that read, cut at
/// [`QUESTION_DETAIL_CHARS`]. A header that did not read shows as `question`; it is reported.
fn describe_questions(qs: &[Question]) -> (String, Option<String>) {
    let headers: Vec<&str> = qs.iter().map(|q| q.header.as_deref().unwrap_or("question")).collect();
    let first = qs.iter().find_map(|q| q.text.as_deref());
    let detail = first.map(|q| {
        let mut cut: String = q.chars().take(QUESTION_DETAIL_CHARS).collect();
        if q.chars().count() > QUESTION_DETAIL_CHARS {
            cut.push('…');
        }
        if qs.len() > 1 { format!("{cut} ({} questions)", qs.len()) } else { cut }
    });
    (format!("Question: {}", headers.join(" · ")), detail)
}

/// The live threads a listing found, and what it could not read (BRO-3846). The listing feeds
/// the owner's permission prompts, not identity, so it fails open with a signal: a thread whose
/// payload did not read is still in `threads`, prompts and all, and named in `unreadable`; a
/// snapshot row that did not read is left out and named; a registry that did not read at all is
/// named by its path. A consumer must not show the prompts as complete while `unreadable` holds
/// anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveThreads {
    pub threads: Vec<T3Thread>,
    /// `(where, why)`: a thread id, or the registry's path.
    pub unreadable: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct RawProviderThread {
    provider_thread_id: String,
    #[serde(default)]
    native_id: Option<String>,
}

/// Where a T3 registry is: a JSON snapshot, or a `statev2.sqlite`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Located {
    Json(PathBuf),
    Sqlite(PathBuf),
}

impl LiveThreads {
    /// A thread the listing read: listed when live, and each of its pending permission records
    /// that did not decode reported unread (BRO-3863), never dropped.
    fn take(&mut self, thread: T3Thread) {
        if !thread.liveness().is_live() {
            return;
        }
        for why in &thread.unread_permissions {
            self.unreadable.push((format!("t3 thread {}", thread.id), why.clone()));
        }
        self.threads.push(thread);
    }
}

/// What one thread variable resolved to (BRO-3846).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one thread, neither archived nor deleted.
    Thread(Box<T3Thread>),
    /// No thread, more than one, or one archived or deleted: why.
    Unresolved(String),
    /// A native id that no provider row of any thread carries (live or not, active or retired), all
    /// rows read: a session T3 never ran, such as a nested `codex exec` started inside a thread
    /// (BRO-4227). Refused as `Unresolved` is.
    Unnamed(String),
}

/// T3 thread registry reader (`statev2.sqlite` or JSON snapshot).
#[derive(Debug, Clone)]
pub struct T3Registry {
    root: PathBuf,
    expected_uid: Option<u32>,
    expected_node: Option<NodeId>,
    disconnected: bool,
}

const SQLITE_READER_PY: &str = r#"
import json, pathlib, sqlite3, sys

class PayloadError(Exception):
    pass

def payload_of(raw, what):
    # The one parser of a thread's or a provider row's payload, in every mode (BRO-3846): the
    # payload carries archivedAt, deletedAt, node and nativeThreadRef, so one that is NULL,
    # empty, blank, not JSON or not an object is an error (`registry_unavailable`), never `{}`.
    if raw is None or not str(raw).strip():
        raise PayloadError(f"{what} is empty")
    try:
        payload = json.loads(raw)
    except Exception:
        raise PayloadError(f"{what} does not parse")
    if not isinstance(payload, dict):
        raise PayloadError(f"{what} is not an object")
    return payload

def main():
    mode = sys.argv[1]
    db_path = pathlib.Path(sys.argv[2])
    target_id = sys.argv[3] if len(sys.argv) > 3 else ""

    env_file = db_path.parent / "environment-id"
    # The read rule: only a missing file is absent; any other error is the database's unread.
    try:
        env_id = env_file.read_text().strip() or None
    except FileNotFoundError:
        env_id = None

    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    tables = {r["name"] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")}

    def runtime_requests_of(thread_id):
        # A thread's pending permission requests, raw: stored apart from its payload, so they read
        # even when the payload does not. The reader does not interpret them: the one rule for
        # prompt or unread lives in Rust (`decode_runtime_requests`, BRO-3863 round 11).
        requests = []
        if "orchestration_v2_projection_runtime_requests" in tables:
            for req in conn.execute(
                "SELECT runtime_request_id, kind, status, payload_json "
                "FROM orchestration_v2_projection_runtime_requests "
                "WHERE thread_id = ? AND status = 'pending' ORDER BY created_at ASC",
                (thread_id,),
            ):
                if req["kind"] in ("approval", "permission", "tool_approval"):
                    requests.append({"id": req["runtime_request_id"], "kind": req["kind"], "payload_json": req["payload_json"]})
        return requests

    # Whether provider-thread rows name their provider (BRO-3828). A schema without the column
    # reports no providers, so no thread there attests a performer; identity reads go on.
    provider_column = "orchestration_v2_projection_provider_threads" in tables and any(
        c["name"] == "provider" for c in conn.execute("PRAGMA table_info(orchestration_v2_projection_provider_threads)")
    )
    # Whether those rows also record the driver each ran (BRO-4018): the provider names an
    # instance the owner chose (`codex_team`), and the driver is what that instance ran then.
    driver_column = provider_column and any(
        c["name"] == "driver" for c in conn.execute("PRAGMA table_info(orchestration_v2_projection_provider_threads)")
    )

    def pending_questions_of(thread_id):
        # A thread's waiting AskUserQuestion cards (BRO-3936): pending `user_input` requests, raw,
        # each with the payload of its `user_input_request` turn item (the questions) or None when
        # none was found. Stored apart from the payload, like permission requests. The reader does
        # not interpret the card: Rust does (`decode_questions`).
        out = []
        if "orchestration_v2_projection_runtime_requests" not in tables:
            return out
        has_items = "orchestration_v2_projection_turn_items" in tables
        for req in conn.execute(
            "SELECT runtime_request_id, created_at "
            "FROM orchestration_v2_projection_runtime_requests "
            "WHERE thread_id = ? AND status = 'pending' AND kind = 'user_input' ORDER BY created_at ASC",
            (thread_id,),
        ):
            item = None
            if has_items:
                hit = conn.execute(
                    "SELECT payload_json FROM orchestration_v2_projection_turn_items "
                    "WHERE thread_id = ? AND type = 'user_input_request' "
                    "AND CASE WHEN json_valid(payload_json) THEN json_extract(payload_json, '$.requestId') END = ? LIMIT 1",
                    (thread_id, req["runtime_request_id"]),
                ).fetchone()
                item = hit["payload_json"] if hit else None
            out.append({"id": req["runtime_request_id"], "created_at": req["created_at"], "item_payload_json": item})
        return out

    def build_thread(row):
        payload = payload_of(row["payload_json"], f"thread {row['thread_id']}'s payload")
        cwd = payload.get("worktreePath")
        if not cwd and "projection_projects" in tables:
            proj = conn.execute(
                "SELECT workspace_root FROM projection_projects WHERE project_id = ?",
                (row["project_id"],),
            ).fetchone()
            if proj:
                cwd = proj["workspace_root"]
        if not isinstance(cwd, str) or not cwd:
            # Attribution picks a thread by its variable, never by a folder: a live thread with
            # no worktree and no project root is still the thread. The listing feeds permission
            # prompts, which need no folder either.
            if mode not in ("attr", "list"):
                return None
            cwd = ""
        run_status = payload.get("status")
        latest_run_id = None
        if "orchestration_v2_projection_runs" in tables:
            r = conn.execute(
                "SELECT run_id, status FROM orchestration_v2_projection_runs WHERE thread_id = ? ORDER BY ordinal DESC LIMIT 1",
                (row["thread_id"],),
            ).fetchone()
            if r:
                if r["status"]:
                    run_status = r["status"]
                latest_run_id = r["run_id"]
        settled_at = payload.get("settledAt")
        unsettled_at = payload.get("unsettledAt")
        if settled_at and unsettled_at and unsettled_at > settled_at:
            settled_at = None
        parent_id = None
        lineage = payload.get("lineage") or payload.get("forkedFrom")
        if isinstance(lineage, dict):
            parent_id = lineage.get("parentThreadId") or lineage.get("threadId")
        created_at = row["created_at"] if "created_at" in row.keys() and row["created_at"] else payload.get("createdAt")
        # Every provider the thread has run under, active row and retired ones (BRO-3828): a
        # performer is attested only when they are one provider, never by the thread's default.
        providers = None
        provider_drivers = None
        if provider_column:
            # A provider that is not text (NULL) names none: it reads, and confirms nobody.
            rows = conn.execute(
                "SELECT provider" + (", driver" if driver_column else "")
                + " FROM orchestration_v2_projection_provider_threads WHERE thread_id = ?",
                (row["thread_id"],),
            ).fetchall()
            providers = [p["provider"] if isinstance(p["provider"], str) else "" for p in rows]
            if driver_column:
                # Each row's driver, in the same order; one that is not text names none.
                provider_drivers = [p["driver"] if isinstance(p["driver"], str) else "" for p in rows]
        run_marker = latest_run_id or payload.get("runId") or payload.get("sessionId")
        return {
            "id": row["thread_id"],
            "cwd": cwd,
            "environment_id": env_id,
            "node": payload.get("node") if isinstance(payload.get("node"), str) else None,
            "provider": row["default_provider"],
            "providers": providers,
            "provider_drivers": provider_drivers,
            "created_at": created_at,
            "run_id": run_marker if isinstance(run_marker, str) and run_marker else None,
            "archived_at": row["archived_at"] or payload.get("archivedAt"),
            "deleted_at": row["deleted_at"] or payload.get("deletedAt"),
            "settled_at": settled_at,
            "status": run_status,
            "parent_id": parent_id,
            "pending_permissions": [],
            "runtime_requests": runtime_requests_of(row["thread_id"]),
            "pending_questions": pending_questions_of(row["thread_id"]),
        }

    if mode == "native_any":
        # Whether ANY provider row, of any thread, live or not, carries this native id
        # (BRO-4227). A payload that does not parse means "cannot tell": exit non-zero.
        if "orchestration_v2_projection_provider_threads" not in tables:
            sys.stderr.write("sqlite_error: missing table orchestration_v2_projection_provider_threads\n")
            sys.exit(2)
        # Validated as the `native` mode validates an active row: a malformed nativeThreadRef or
        # a non-string nativeId is "cannot tell", never "not found".
        found = False
        for r in conn.execute("SELECT thread_id, payload_json FROM orchestration_v2_projection_provider_threads"):
            payload = payload_of(r["payload_json"], f"a provider row of thread {r['thread_id']}")
            ref = payload.get("nativeThreadRef")
            if ref is None:
                continue
            if not isinstance(ref, dict):
                raise PayloadError(f"a provider row of thread {r['thread_id']} has a malformed nativeThreadRef")
            native = ref.get("nativeId")
            if native is None:
                continue
            if not isinstance(native, str):
                raise PayloadError(f"a provider row of thread {r['thread_id']} has a malformed nativeThreadRef")
            if native == target_id:
                found = True
        print(json.dumps(found))
        sys.exit(0)
    if mode == "native":
        # The threads whose ACTIVE provider-thread row carries this native id (a Codex
        # CODEX_THREAD_ID, or a Stop hook's session_id). A retired provider row never attributes.
        # Uniqueness needs every active row read: a missing table, an active pointer with no row,
        # or a payload that does not parse is an error, never a skipped row.
        missing = sorted({"orchestration_v2_projection_threads", "orchestration_v2_projection_provider_threads"} - tables)
        if missing:
            sys.stderr.write(f"sqlite_error: missing table(s) {', '.join(missing)}\n")
            sys.exit(2)
        def unreadable(thread_id, why):
            raise PayloadError(f"the active provider row of thread {thread_id} {why}")
        out = []
        rows = conn.execute(
            "SELECT t.thread_id AS thread_id, t.payload_json AS thread_payload, "
            "p.provider_thread_id AS provider_thread_id, p.payload_json AS payload_json "
            "FROM orchestration_v2_projection_threads t "
            "LEFT JOIN orchestration_v2_projection_provider_threads p "
            "ON p.provider_thread_id = t.active_provider_thread_id AND p.thread_id = t.thread_id "
            "WHERE t.active_provider_thread_id IS NOT NULL AND t.archived_at IS NULL AND t.deleted_at IS NULL"
        )
        for r in rows:
            # A thread archived or deleted in its payload only (`archivedAt`, `deletedAt`, as
            # `build_thread` reads them) is no live thread either. Its payload must read to tell.
            thread_payload = payload_of(r["thread_payload"], f"thread {r['thread_id']}'s payload")
            if thread_payload.get("archivedAt") or thread_payload.get("deletedAt"):
                continue
            if r["provider_thread_id"] is None:
                unreadable(r["thread_id"], "is missing")
            payload = payload_of(r["payload_json"], f"the active provider row of thread {r['thread_id']}")
            ref = payload.get("nativeThreadRef")
            if ref is None:
                continue
            if not isinstance(ref, dict):
                unreadable(r["thread_id"], "has a malformed nativeThreadRef")
            # A native id not yet assigned matches nothing; any other non-string one is malformed.
            if ref.get("nativeId") is None:
                continue
            if not isinstance(ref.get("nativeId"), str):
                unreadable(r["thread_id"], "has a malformed nativeThreadRef")
            if ref["nativeId"] == target_id and r["thread_id"] not in out:
                out.append(r["thread_id"])
        print(json.dumps(out))
        sys.exit(0)

    if mode == "list":
        out = []
        if "orchestration_v2_projection_threads" in tables:
            for row in conn.execute(
                "SELECT rowid AS _rowid, thread_id, project_id, default_provider, created_at, archived_at, deleted_at, payload_json "
                "FROM orchestration_v2_projection_threads ORDER BY updated_at DESC"
            ):
                # The listing feeds permission prompts, not identity: it fails open, with a signal.
                # A thread whose payload does not read is still listed, from what is known (its
                # columns, and its prompts, stored apart), with the reason in `unreadable`; its
                # payload fields are unknown. Its prompts are never dropped.
                try:
                    built = build_thread(row)
                except PayloadError as exc:
                    built = {
                        "id": row["thread_id"],
                        "cwd": "",
                        "environment_id": env_id,
                        "node": None,
                        "provider": row["default_provider"],
                        "created_at": row["created_at"],
                        "run_id": None,
                        "archived_at": row["archived_at"],
                        "deleted_at": row["deleted_at"],
                        "settled_at": None,
                        "status": None,
                        "parent_id": None,
                        "pending_permissions": [],
                        "runtime_requests": runtime_requests_of(row["thread_id"]),
                        "pending_questions": pending_questions_of(row["thread_id"]),
                        "unreadable": str(exc),
                    }
                if built is not None:
                    out.append(built)
        print(json.dumps(out))
        sys.exit(0)

    if mode == "attr" and not ({"orchestration_v2_projection_threads", "projection_threads"} & tables):
        sys.stderr.write("sqlite_error: no thread table\n")
        sys.exit(2)

    if "orchestration_v2_projection_threads" in tables:
        row = conn.execute(
            "SELECT rowid AS _rowid, thread_id, project_id, default_provider, created_at, archived_at, deleted_at, payload_json "
            "FROM orchestration_v2_projection_threads WHERE thread_id = ?",
            (target_id,),
        ).fetchone()
        if row is not None:
            built = build_thread(row)
            if built is not None:
                print(json.dumps(built))
                sys.exit(0)

    if "projection_threads" in tables:
        pcols = {r["name"] for r in conn.execute("PRAGMA table_info(projection_threads)")}
        created_sel = "created_at" if "created_at" in pcols else "NULL AS created_at"
        prow = conn.execute(
            f"SELECT rowid AS _rowid, thread_id, project_id, worktree_path, {created_sel}, archived_at, deleted_at, settled_at "
            "FROM projection_threads WHERE thread_id = ?",
            (target_id,),
        ).fetchone()
        if prow is not None:
            cwd = prow["worktree_path"]
            if not cwd and "projection_projects" in tables:
                proj = conn.execute(
                    "SELECT workspace_root FROM projection_projects WHERE project_id = ?",
                    (prow["project_id"],),
                ).fetchone()
                if proj:
                    cwd = proj["workspace_root"]
            provider = None
            status = None
            session_id = None
            if "projection_thread_sessions" in tables:
                scols = {r["name"] for r in conn.execute("PRAGMA table_info(projection_thread_sessions)")}
                sid_sel = "session_id" if "session_id" in scols else ("run_id AS session_id" if "run_id" in scols else "NULL AS session_id")
                srow = conn.execute(
                    f"SELECT provider_name, status, {sid_sel} FROM projection_thread_sessions WHERE thread_id = ?",
                    (target_id,),
                ).fetchone()
                if srow:
                    provider = srow["provider_name"]
                    status = srow["status"]
                    session_id = srow["session_id"]
            if cwd or mode == "attr":
                print(json.dumps({
                    "id": prow["thread_id"],
                    "cwd": cwd or "",
                    "environment_id": env_id,
                    "node": None,
                    "provider": provider,
                    "created_at": prow["created_at"],
                    "run_id": session_id if isinstance(session_id, str) and session_id else None,
                    "archived_at": prow["archived_at"],
                    "deleted_at": prow["deleted_at"],
                    "settled_at": prow["settled_at"],
                    "status": status,
                    "parent_id": None,
                    "pending_permissions": [],
                }))
                sys.exit(0)

    sys.exit(3)

try:
    main()
except SystemExit:
    raise
except PayloadError as exc:
    # A payload that does not read: exit 4, which every reader maps to `registry_unavailable`.
    sys.stderr.write(f"sqlite_error: {exc}\n")
    sys.exit(4)
except Exception as exc:
    sys.stderr.write(f"sqlite_error: {exc}\n")
    sys.exit(2)
"#;

impl T3Registry {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), expected_uid: None, expected_node: None, disconnected: false }
    }

    pub fn with_expected_uid(mut self, uid: u32) -> Self {
        self.expected_uid = Some(uid);
        self
    }

    pub fn with_expected_node(mut self, node: NodeId) -> Self {
        self.expected_node = Some(node);
        self
    }

    pub fn with_disconnected(mut self, disconnected: bool) -> Self {
        self.disconnected = disconnected;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_connected(&self) -> bool {
        !self.disconnected && (self.json_path().is_some() || self.sqlite_path().is_some())
    }

    /// Discover T3 runtime version, transport, environment id, and connection state.
    pub fn discover(&self) -> BridgeDiscovery {
        // A `userdata` folder that cannot be told is taken as the one: the reads under it then fail
        // visibly, never as a folder that is not there.
        let userdata = if !matches!(read::is_dir(&self.root.join("userdata")), Read::Present(false) | Read::Absent) {
            self.root.join("userdata")
        } else if self.sqlite_path().and_then(|p| p.parent().map(Path::to_path_buf)).is_some() {
            self.sqlite_path().unwrap().parent().unwrap().to_path_buf()
        } else {
            self.root.clone()
        };

        // Discovery reports what it can: a file that does not read is not reported. The registry's
        // own reads (`locate`, the readers) are what fail on an unreadable registry.
        let text = |path: PathBuf| match read::read_text(&path) {
            Read::Present(t) => Some(t),
            Read::Absent | Read::Unreadable(_) => None,
        };
        let env_id = text(userdata.join("environment-id")).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        // Display metadata only (the runtime's version and endpoint): a file that does not decode
        // reports them unknown. Identity and needs never read these two files; the read guard lists
        // this as its one reviewed exception.
        let decode = |t: String| serde_json::from_str::<Value>(&t).ok();
        let runtime_json = text(userdata.join("server-runtime.json")).and_then(decode);
        let service_json = text(self.root.join("runtime/service-state.json")).and_then(decode);

        let version = runtime_json
            .as_ref()
            .and_then(|v| v.get("serverVersion"))
            .and_then(Value::as_str)
            .or_else(|| service_json.as_ref().and_then(|v| v.get("activeVersion")).and_then(Value::as_str))
            .unwrap_or("0.1.3")
            .to_string();

        let endpoint = std::env::var("T3_ACP_MCP_ENDPOINT").ok().filter(|s| !s.is_empty()).or_else(|| {
            runtime_json
                .as_ref()
                .and_then(|v| v.get("origin"))
                .and_then(Value::as_str)
                .map(|origin| format!("{}/mcp", origin.trim_end_matches('/')))
        });

        let transport = if endpoint.is_some() && self.sqlite_path().is_some() {
            BridgeTransport::T3McpBridge
        } else if self.sqlite_path().is_some() {
            BridgeTransport::T3SqliteReadonly
        } else {
            BridgeTransport::T3JsonSnapshot
        };

        BridgeDiscovery {
            transport,
            version,
            endpoint,
            environment_id: env_id,
            node: self.expected_node.clone(),
            connected: self.is_connected(),
        }
    }

    /// Where the registry is (BRO-3846): a JSON snapshot (`<root>` or `<root>/threads.json`), else a
    /// `statev2.sqlite` (`<root>`, `<root>/statev2.sqlite`, `<root>/userdata/statev2.sqlite`),
    /// each probed by the [`read`](crate::read) rule: a candidate whose metadata does not read
    /// makes the registry unreadable, never "not there".
    pub(crate) fn locate(&self) -> Read<Located> {
        let json = [self.root.clone(), self.root.join("threads.json")].map(|c| (c, true));
        let sqlite = [self.root.clone(), self.root.join("statev2.sqlite"), self.root.join("userdata/statev2.sqlite")]
            .map(|c| (c, false));
        for (candidate, is_json) in json.into_iter().chain(sqlite) {
            let ext = candidate.extension().and_then(|e| e.to_str()).unwrap_or_default();
            if !(if is_json { ext == "json" } else { ext == "sqlite" || ext == "db" }) {
                continue;
            }
            match read::is_file(&candidate) {
                Read::Present(true) => {
                    return Read::Present(if is_json { Located::Json(candidate) } else { Located::Sqlite(candidate) });
                }
                Read::Present(false) | Read::Absent => {}
                Read::Unreadable(why) => return Read::Unreadable(why),
            }
        }
        Read::Absent
    }

    fn sqlite_path(&self) -> Option<PathBuf> {
        match self.locate() {
            Read::Present(Located::Sqlite(p)) => Some(p),
            _ => None,
        }
    }

    fn json_path(&self) -> Option<PathBuf> {
        match self.locate() {
            Read::Present(Located::Json(p)) => Some(p),
            _ => None,
        }
    }

    fn from_raw(raw: RawT3Thread) -> T3Thread {
        let (mut pending_permissions, mut unread_permissions) =
            decode_permissions(raw.pending_permissions.unwrap_or_default(), &raw.cwd, "t3-perm");
        let (requested, unread_requests) = decode_runtime_requests(raw.runtime_requests, &raw.id, &raw.cwd, "t3-perm");
        pending_permissions.extend(requested);
        unread_permissions.extend(unread_requests);
        let (pending_questions, unread_questions) =
            decode_questions(raw.pending_questions.unwrap_or_default(), &raw.id);
        unread_permissions.extend(unread_questions);
        T3Thread {
            id: raw.id,
            cwd: raw.cwd,
            environment_id: raw.environment_id,
            node: raw.node,
            provider: raw.provider,
            created_at: raw.created_at,
            run_id: raw.run_id,
            archived_at: raw.archived_at,
            deleted_at: raw.deleted_at,
            settled_at: raw.settled_at,
            status: raw.status,
            parent_id: raw.parent_id,
            providers: raw.providers,
            provider_drivers: raw.provider_drivers,
            pending_permissions,
            unread_permissions,
            pending_questions,
        }
    }

    /// Every row of a JSON snapshot, by the one rule the SQLite reader's `payload_of` applies to a
    /// thread's payload (BRO-3846). A file that does not read or is not a JSON array is
    /// `registry_disconnected`, as an unreadable database is. A row that is not an object, whose
    /// `cwd` is neither missing, null nor a string, or that does not deserialize, is
    /// `registry_unavailable`: never skipped, never read as `{}`. A missing or null `cwd` is none.
    fn snapshot_rows(json_file: &Path) -> Result<Vec<RawT3Thread>, IdentityRefusal> {
        Self::snapshot_each(json_file)?.into_iter().collect()
    }

    /// [`Self::snapshot_rows`], each row's outcome kept apart: the listing leaves out only the row
    /// that does not read, never every row.
    fn snapshot_each(json_file: &Path) -> Result<Vec<Result<RawT3Thread, IdentityRefusal>>, IdentityRefusal> {
        let disconnected =
            |e: String| IdentityRefusal::new(RefusalReason::RegistryDisconnected, format!("T3 JSON registry {e}."));
        let unavailable =
            |e: String| IdentityRefusal::new(RefusalReason::RegistryUnavailable, format!("T3 JSON registry {e}."));
        let text = match read::read_text(json_file) {
            Read::Present(text) => text,
            Read::Absent => {
                return Err(disconnected(format!("could not be read: {} is not there", json_file.display())));
            }
            Read::Unreadable(why) => return Err(disconnected(format!("could not be read: {why}"))),
        };
        let rows: Vec<Value> = serde_json::from_str(&text).map_err(|e| disconnected(format!("is malformed: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|mut row| {
                let id = row.get("id").and_then(Value::as_str).unwrap_or("?").to_string();
                let Some(o) = row.as_object_mut() else {
                    return Err(unavailable("has a row that is not an object".to_string()));
                };
                match o.get("cwd") {
                    None | Some(Value::Null) => {
                        o.insert("cwd".into(), Value::String(String::new()));
                    }
                    Some(Value::String(_)) => {}
                    Some(_) => return Err(unavailable(format!("row {id} has a cwd that is not a string"))),
                }
                serde_json::from_value(row).map_err(|e| unavailable(format!("row {id} is malformed: {e}")))
            })
            .collect::<Vec<_>>())
    }

    /// A JSON snapshot's row: one with no environment of its own is in the snapshot's environment
    /// (`environment-id` beside it), as every `statev2.sqlite` row is in its database's.
    fn snapshot_row(raw: RawT3Thread, environment: &Option<String>) -> T3Thread {
        let mut thread = Self::from_raw(raw);
        if thread.environment_id.is_none() {
            thread.environment_id = environment.clone();
        }
        thread
    }

    /// The environment id this host's T3 registry reports (`userdata/environment-id`), the
    /// canonical form of a `local` or `-` T3 session (BRO-3846).
    pub fn environment_id(&self) -> Option<String> {
        self.discover().environment_id.or_else(|| {
            // A JSON snapshot's root is the file: its `environment-id` sits beside it.
            let beside = self.root.parent().filter(|_| matches!(read::is_file(&self.root), Read::Present(true)))?;
            match read::read_text(&beside.join("environment-id")) {
                Read::Present(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
                Read::Absent | Read::Unreadable(_) => None,
            }
        })
    }

    /// The threads whose active provider-thread row carries `native_id`, read from the registry.
    /// Whether any provider row of any thread (archived, deleted or retired rows included) carries
    /// `native_id`. Only [`Resolution::Unnamed`] reads it; an error means "cannot tell".
    fn native_ever_carried(&self, native_id: &str) -> Result<bool, IdentityRefusal> {
        let unreadable = |detail: String| IdentityRefusal::new(RefusalReason::RegistryUnavailable, detail);
        if let Some(json_file) = self.json_path() {
            // Read raw, not through `RawT3Thread`, whose `provider_threads` defaults to empty: a row
            // must list its provider rows explicitly, each `native_id` a string or null, or the
            // snapshot cannot prove that no row carries the id (BRO-4227 P20 r4).
            let Read::Present(text) = read::read_text(&json_file) else {
                return Err(unreadable("T3 JSON registry did not read".to_string()));
            };
            let rows: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| unreadable(format!("T3 JSON registry is not JSON: {e}")))?;
            let rows = rows.as_array().ok_or_else(|| unreadable("T3 JSON registry is not an array".to_string()))?;
            let mut found = false;
            for row in rows {
                let providers = row
                    .get("provider_threads")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| unreadable("a T3 thread row does not list its provider rows".to_string()))?;
                for p in providers {
                    match p.get("native_id") {
                        Some(serde_json::Value::String(id)) => found |= id == native_id,
                        Some(serde_json::Value::Null) => {}
                        _ => return Err(unreadable("a provider row has no readable native_id".to_string())),
                    }
                }
            }
            return Ok(found);
        }
        if let Some(db_file) = self.sqlite_path() {
            let out = Command::new("python3")
                .arg("-I")
                .arg("-c")
                .arg(SQLITE_READER_PY)
                .arg("native_any")
                .arg(&db_file)
                .arg(native_id)
                .output()
                .map_err(|e| unreadable(format!("T3 SQLite reader failed to launch: {e}.")))?;
            if out.status.code() != Some(0) {
                return Err(unreadable(String::from_utf8_lossy(&out.stderr).trim().to_string()));
            }
            return serde_json::from_slice(&out.stdout)
                .map_err(|e| unreadable(format!("T3 SQLite reader returned invalid JSON: {e}.")));
        }
        Err(unreadable("no T3 registry file".to_string()))
    }

    fn native_threads(&self, native_id: &str) -> Result<Vec<String>, IdentityRefusal> {
        let unreadable = |detail: String| IdentityRefusal::new(RefusalReason::RegistryUnavailable, detail);
        if self.disconnected {
            return Err(unreadable("T3 registry is disconnected.".to_string()));
        }
        if let Read::Unreadable(why) = self.locate() {
            return Err(unreadable(format!("T3 registry is unreadable: {why}.")));
        }
        if let Some(json_file) = self.json_path() {
            let rows = Self::snapshot_rows(&json_file).map_err(|e| unreadable(e.message))?;
            let mut out: Vec<String> = Vec::new();
            for raw in rows {
                let Some(active) = raw.active_provider_thread_id.as_deref() else { continue };
                // Only live threads attribute, so only theirs need reading whole.
                if raw.archived_at.is_some() || raw.deleted_at.is_some() {
                    continue;
                }
                if !raw.provider_threads.iter().any(|p| p.provider_thread_id == active) {
                    return Err(unreadable(format!("The active provider row of thread {} is missing.", raw.id)));
                }
                let hit = raw
                    .provider_threads
                    .iter()
                    .any(|p| p.provider_thread_id == active && p.native_id.as_deref() == Some(native_id));
                if hit && !out.contains(&raw.id) {
                    out.push(raw.id);
                }
            }
            return Ok(out);
        }
        if let Some(db_file) = self.sqlite_path() {
            let out = Command::new("python3")
                .arg("-I")
                .arg("-c")
                .arg(SQLITE_READER_PY)
                .arg("native")
                .arg(&db_file)
                .arg(native_id)
                .output()
                .map_err(|e| unreadable(format!("T3 SQLite reader failed to launch: {e}.")))?;
            if out.status.code() != Some(0) {
                let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(unreadable(format!("T3 SQLite database is unreadable: {detail}")));
            }
            return serde_json::from_slice(&out.stdout)
                .map_err(|e| unreadable(format!("T3 SQLite reader returned invalid JSON: {e}.")));
        }
        Err(unreadable(format!("No T3 registry database or snapshot at {}.", self.root.display())))
    }

    /// Resolves a thread variable to its thread (BRO-3846). `native` is false for
    /// `T3CODE_THREAD_ID`, which names a thread row; true for `CODEX_THREAD_ID` or a Stop hook's
    /// `session_id`, which name a provider's native thread and resolve through the thread's active
    /// provider row only. Liveness is `archived_at` and `deleted_at` unset; `settled_at` and the run
    /// status are ignored. A registry that does not read is `registry_unavailable`, never a fallback.
    pub fn resolve(&self, value: &str, native: bool) -> Result<Resolution, IdentityRefusal> {
        let thread_id = if native {
            if value.is_empty() || value.len() > MAX_T3_ID || value.contains('\0') {
                return Ok(Resolution::Unresolved(format!("{} is not a native thread id", short_id(value))));
            }
            let found = self.native_threads(value)?;
            match found.as_slice() {
                [] => {
                    let why = format!("no T3 thread's active provider row is {}", short_id(value));
                    // Unnamed only when no provider row of any thread, live or not, active or
                    // retired, carries the id (BRO-4227): an archived or deleted thread's session is
                    // still a thread's. Anything that does not read stays Unresolved.
                    return Ok(match self.native_ever_carried(value) {
                        Ok(false) => Resolution::Unnamed(why),
                        Ok(true) | Err(_) => Resolution::Unresolved(why),
                    });
                }
                [one] => one.clone(),
                many => {
                    return Ok(Resolution::Unresolved(format!(
                        "{} active provider rows carry {}",
                        many.len(),
                        short_id(value)
                    )));
                }
            }
        } else {
            value.to_string()
        };
        if !is_safe_t3_id(&thread_id) {
            return Ok(Resolution::Unresolved(format!("{} is not a T3 thread id", short_id(&thread_id))));
        }
        if self.disconnected {
            return Err(IdentityRefusal::new(RefusalReason::RegistryUnavailable, "T3 registry is disconnected."));
        }
        let thread = self
            .lookup_attributed(&thread_id)
            .map_err(|r| IdentityRefusal::new(RefusalReason::RegistryUnavailable, r.message))?;
        Ok(match thread {
            None => Resolution::Unresolved(format!("no T3 thread has the id {}", short_id(&thread_id))),
            Some(t) if t.archived_at.is_some() => {
                Resolution::Unresolved(format!("thread {} is archived", short_id(&t.id)))
            }
            Some(t) if t.deleted_at.is_some() => {
                Resolution::Unresolved(format!("thread {} is deleted", short_id(&t.id)))
            }
            Some(t) => Resolution::Thread(Box::new(t)),
        })
    }

    fn lookup(&self, id: &str) -> Result<Option<T3Thread>, IdentityRefusal> {
        self.lookup_in(id, false)
    }

    /// The thread `id`, for attribution (BRO-3846): as [`Self::lookup`], which the credentialed
    /// reader keeps, except that a thread with no usable folder (no `worktreePath`, no project
    /// root) is still the thread, with an empty `cwd`. The variable picks it, never a folder.
    fn lookup_attributed(&self, id: &str) -> Result<Option<T3Thread>, IdentityRefusal> {
        self.lookup_in(id, true)
    }

    fn lookup_in(&self, id: &str, attributed: bool) -> Result<Option<T3Thread>, IdentityRefusal> {
        if !is_safe_t3_id(id) {
            return Ok(None);
        }
        // A registry whose location cannot be told is not "no thread": identity fails closed.
        if let Read::Unreadable(why) = self.locate() {
            return Err(IdentityRefusal::new(
                RefusalReason::RegistryDisconnected,
                format!("T3 registry is unreadable: {why}."),
            ));
        }
        if let Some(json_file) = self.json_path() {
            let env = self.environment_id();
            let found = Self::snapshot_rows(&json_file)?.into_iter().find(|raw| raw.id == id);
            // The credentialed reader needs the thread's folder; attribution never does.
            return Ok(found.filter(|raw| attributed || !raw.cwd.is_empty()).map(|raw| Self::snapshot_row(raw, &env)));
        }
        if let Some(db_file) = self.sqlite_path() {
            let out = Command::new("python3")
                .arg("-I")
                .arg("-c")
                .arg(SQLITE_READER_PY)
                .arg(if attributed { "attr" } else { "get" })
                .arg(&db_file)
                .arg(id)
                .output()
                .map_err(|e| {
                    IdentityRefusal::new(
                        RefusalReason::RegistryDisconnected,
                        format!("T3 SQLite reader failed to launch: {e}."),
                    )
                })?;
            match out.status.code() {
                Some(0) => {
                    let raw: RawT3Thread = serde_json::from_slice(&out.stdout).map_err(|e| {
                        IdentityRefusal::new(
                            RefusalReason::RegistryDisconnected,
                            format!("T3 SQLite reader returned invalid JSON: {e}."),
                        )
                    })?;
                    if raw.id != id {
                        return Ok(None);
                    }
                    return Ok(Some(Self::from_raw(raw)));
                }
                Some(3) => return Ok(None),
                // A thread's payload that does not read (BRO-3846): its liveness and folder are
                // unknown, so neither reader may take the thread as found, or as absent.
                Some(4) => {
                    return Err(IdentityRefusal::new(
                        RefusalReason::RegistryUnavailable,
                        format!("T3 registry: {}", String::from_utf8_lossy(&out.stderr).trim()),
                    ));
                }
                _ => {
                    return Err(IdentityRefusal::new(
                        RefusalReason::RegistryDisconnected,
                        format!("T3 SQLite database is unreadable: {}", String::from_utf8_lossy(&out.stderr).trim()),
                    ));
                }
            }
        }
        Err(IdentityRefusal::new(
            RefusalReason::RegistryDisconnected,
            format!("No T3 registry database or snapshot at {}.", self.root.display()),
        ))
    }

    /// Look up `id` in T3's thread registry (`statev2.sqlite` or `threads.json`).
    pub fn find(&self, id: &str) -> Option<T3Thread> {
        if !self.is_connected() {
            return None;
        }
        self.lookup(id).ok().flatten()
    }

    /// Enumerate all live T3 threads in the registry.
    pub fn list_live_threads(&self) -> Vec<T3Thread> {
        self.list_live_report().threads
    }

    /// Every live thread, and what could not be read (see [`LiveThreads`]).
    pub fn list_live_report(&self) -> LiveThreads {
        let mut report = LiveThreads::default();
        // A configured registry that is disconnected, missing or inaccessible is unread, never
        // empty: only a host with no T3 registry at all has nothing to read.
        let at = format!("t3 registry {}", self.root.display());
        if self.disconnected {
            report.unreadable.push((at, "the T3 registry is disconnected".to_string()));
            return report;
        }
        match self.locate() {
            Read::Present(_) => {}
            Read::Absent => {
                let why = format!("no T3 registry database or snapshot is readable at {}", self.root.display());
                report.unreadable.push((at, why));
                return report;
            }
            Read::Unreadable(why) => {
                report.unreadable.push((at, why));
                return report;
            }
        }
        if let Some(json_file) = self.json_path() {
            let rows = match Self::snapshot_each(&json_file) {
                Ok(rows) => rows,
                Err(e) => {
                    report.unreadable.push((at, e.message));
                    return report;
                }
            };
            let env = self.environment_id();
            for row in rows {
                match row {
                    Ok(raw) => report.take(Self::snapshot_row(raw, &env)),
                    Err(e) => report.unreadable.push((at.clone(), e.message)),
                }
            }
            return report;
        }
        if let Some(db_file) = self.sqlite_path() {
            let out = match Command::new("python3")
                .arg("-I")
                .arg("-c")
                .arg(SQLITE_READER_PY)
                .arg("list")
                .arg(&db_file)
                .output()
            {
                Ok(out) => out,
                Err(e) => {
                    report.unreadable.push((at, format!("the T3 SQLite reader failed to launch: {e}")));
                    return report;
                }
            };
            if !out.status.success() {
                let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
                report.unreadable.push((at, format!("the T3 SQLite database is unreadable: {why}")));
                return report;
            }
            let rows = match serde_json::from_slice::<Vec<RawT3Thread>>(&out.stdout) {
                Ok(rows) => rows,
                Err(e) => {
                    report.unreadable.push((at, format!("the T3 SQLite reader returned invalid JSON: {e}")));
                    return report;
                }
            };
            for raw in rows {
                if let Some(why) = raw.unreadable.clone() {
                    report.unreadable.push((format!("t3 thread {}", raw.id), why));
                }
                report.take(Self::from_raw(raw));
            }
        }
        report
    }

    /// Verify the thread a claiming process's own thread variable resolved to (BRO-3846). Called
    /// from `SurfaceRegistry::verify_session` for a T3 claim with an attribution and no credential,
    /// so the node, environment, generation and cross-surface checks there still apply. It needs
    /// no folder: the variable, not a cwd, picks the thread. Liveness is attribution's (`archived_at`
    /// and `deleted_at` unset). It sets the same owner marker [`T3Registry::verify`] sets.
    pub fn verify_attributed(
        &self,
        thread_id: &str,
        node: Option<&NodeId>,
    ) -> Result<VerifiedSession, IdentityRefusal> {
        if self.disconnected {
            return Err(IdentityRefusal::new(RefusalReason::RegistryDisconnected, "T3 registry is disconnected."));
        }
        let Some(thread) = self.lookup_attributed(thread_id)? else {
            return Err(IdentityRefusal::new(
                RefusalReason::AttributionUnresolved,
                format!("No T3 thread has the id {}.", short_id(thread_id)),
            ));
        };
        self.check_node(&thread, node)?;
        if thread.archived_at.is_some() || thread.deleted_at.is_some() {
            return Err(IdentityRefusal::new(
                RefusalReason::AttributionUnresolved,
                format!("Thread {} is archived or deleted.", short_id(thread_id)),
            ));
        }
        let Some(incarnation) = thread.incarnation() else {
            return Err(IdentityRefusal::new(
                RefusalReason::InvalidToken,
                format!("Thread {thread_id} lacks an authoritative incarnation marker."),
            ));
        };
        Ok(Self::verified(thread, incarnation))
    }

    /// The thread's node, and this host's, against the node the claim presents.
    fn check_node(&self, thread: &T3Thread, node: Option<&NodeId>) -> Result<(), IdentityRefusal> {
        let presented = node.or(self.expected_node.as_ref());
        for (want, actual) in [(thread.node.as_ref(), presented), (self.expected_node.as_ref(), node)] {
            if let (Some(want), Some(actual)) = (want, actual)
                && want != actual
            {
                return Err(IdentityRefusal::new(
                    RefusalReason::WrongNode,
                    format!("Thread {} is bound to node {want}, not {actual}.", thread.id),
                ));
            }
        }
        Ok(())
    }

    /// A verified T3 session for `thread`, with its owner marker (ADR flow-ledger decision 6): fixed
    /// across turns and provider switches, new when the thread is recreated. The environment lives
    /// in the SessionRef, not here. No `created_at`, no marker: writes are then `no_owner_marker`.
    fn verified(thread: T3Thread, incarnation: String) -> VerifiedSession {
        let harness = thread.session_harness();
        let owner_marker = thread
            .created_at
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|created_at| format!("t3-owner:{}:{created_at}", thread.id));
        let verified = VerifiedSession::new(
            SessionRef { surface: Surface::T3, environment: thread.environment_id, id: thread.id },
            thread.cwd,
            harness,
            thread.parent_id,
            thread.pending_permissions,
        )
        .with_incarnation(incarnation);
        match owner_marker {
            Some(marker) => verified.with_owner_marker(marker),
            None => verified,
        }
    }

    /// The thread `id` as the attested performer of a node `owner` drives (BRO-3828): a thread on
    /// this host, not deleted or closed, whose parent is the owner's thread. A delegated child has
    /// usually settled by the time its owner completes, so a settled or archived child still
    /// attests; nothing about the child's own process is asked, only the registry's record of it.
    pub fn performer(&self, owner: &VerifiedSession, id: &str) -> Result<VerifiedSession, IdentityRefusal> {
        let unverified = |detail: String| IdentityRefusal::new(RefusalReason::PerformerUnverified, detail);
        if owner.session().surface != Surface::T3 {
            return Err(unverified("only a T3 owner names a T3 performer".into()));
        }
        if self.disconnected {
            return Err(IdentityRefusal::new(RefusalReason::RegistryDisconnected, "T3 registry is disconnected."));
        }
        let Some(thread) = self.lookup_attributed(id)? else {
            return Err(unverified(format!("no T3 thread on this host has the id {id}")));
        };
        // A child runs on this host's node: the owner's, else the registry's expected one. A child
        // that names a node this host cannot confirm is foreign (P20 round 4, CodeRabbit round 6);
        // one that names none claims no other host.
        let here = owner.node().or(self.expected_node.as_ref());
        let foreign = match (thread.node.as_ref(), here) {
            (Some(child), Some(here)) => child != here,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if foreign || self.check_node(&thread, None).is_err() {
            return Err(unverified(format!("thread {} is bound to another node", short_id(id))));
        }
        if thread.liveness() == SessionLiveness::Closed {
            return Err(unverified(format!("thread {} is closed", short_id(id))));
        }
        if thread.parent_id.as_deref() != Some(owner.session().id.as_str()) {
            return Err(unverified(format!("thread {} is not a child of the owner's thread", short_id(id))));
        }
        let Some(harness) = thread.performed_harness() else {
            return Err(unverified(format!(
                "thread {} has run under more than one provider, or the registry records none",
                short_id(id)
            )));
        };
        Ok(VerifiedSession::new(
            SessionRef { surface: Surface::T3, environment: thread.environment_id, id: thread.id },
            thread.cwd,
            Some(harness),
            thread.parent_id,
            Vec::new(),
        ))
    }

    /// Verify a caller claim against T3's thread registry.
    pub fn verify(&self, claim: &CallerClaim) -> Result<VerifiedSession, IdentityRefusal> {
        if claim.surface != Surface::T3 {
            return Err(IdentityRefusal::new(
                RefusalReason::CrossSurface,
                format!("Surface `{}` cannot be verified by the T3 registry.", claim.surface.as_str()),
            ));
        }
        if self.disconnected {
            return Err(IdentityRefusal::new(RefusalReason::RegistryDisconnected, "T3 registry is disconnected."));
        }
        check_peer(self.expected_uid, claim.peer)?;
        let Some(thread_id) = claim.session_id.as_deref().filter(|s| !s.is_empty()) else {
            return Err(IdentityRefusal::new(
                RefusalReason::MissingSessionId,
                "Maestro cannot tell which T3 thread is asking: no thread id was supplied.",
            ));
        };
        let Some(cwd) = claim.cwd.as_deref().filter(|s| !s.is_empty()) else {
            return Err(IdentityRefusal::new(
                RefusalReason::MissingCwd,
                "Maestro cannot tell where this shell is: the request carried no folder.",
            ));
        };
        if !Path::new(cwd).is_absolute() {
            return Err(IdentityRefusal::new(
                RefusalReason::RelativeCwd,
                format!("The shell's folder must be an absolute path, not {cwd}."),
            ));
        }
        let Some(thread) = self.lookup(thread_id)? else {
            return Err(IdentityRefusal::new(RefusalReason::NotFound, format!("No T3 thread has the id {thread_id}.")));
        };
        self.check_node(&thread, claim.node.as_ref())?;
        let liveness = thread.liveness();
        if !liveness.is_live() {
            let short = short_id(thread_id);
            return Err(IdentityRefusal::new(
                RefusalReason::NotLive,
                format!("Thread {short} is {}, not live.", liveness.as_str()),
            ));
        }
        if !within(Path::new(cwd), Path::new(&thread.cwd)) {
            let short = short_id(thread_id);
            return Err(IdentityRefusal::new(
                RefusalReason::OutsideCwd,
                format!(
                    "This shell is in {cwd}, but thread {short} runs in {}. Run maestro from inside that folder.",
                    thread.cwd
                ),
            ));
        }
        let Some(incarnation) = thread.incarnation() else {
            return Err(IdentityRefusal::new(
                RefusalReason::InvalidToken,
                format!("Thread {thread_id} lacks an authoritative incarnation marker."),
            ));
        };
        Ok(Self::verified(thread, incarnation))
    }
}
