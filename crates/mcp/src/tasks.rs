use crate::{Ambient, Check, MaestroServer, verified_caller};
use axum::http::{header::AUTHORIZATION, request::Parts};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use maestro_core::session::{SessionRef, Surface};
use rmcp::ErrorData as McpError;
use rmcp::model::{
    DetailedTask, ElicitRequest, ElicitRequestParams, InputRequest, InputResponses, Task, TaskPayload, TaskStatus,
};
use rmcp::service::{RequestContext, RoleServer};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const DAY_MS: i64 = 86_400_000;
const REPORT_MS: i64 = 300_000;
const LEASE_MS: i64 = 60_000;
const OBSERVER_MS: i64 = 30 * 60_000;
const OBSERVER_POLL_MS: i64 = 5_000;
const DECISIONS_PROMPT: &str = "open-decisions";
const ACTIVE: &str = "state IN ('queued','running','input_required','watching')";
const MAX_RESULT: usize = 256 * 1024;
const MAX_ACTIVE: i64 = 4;
const MAX_ROWS: i64 = 128;
const MAX_RETAINED_BYTES: i64 = 16 * 1024 * 1024;
const MAX_ATTEMPTS: i64 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReportSection {
    #[default]
    All,
    Census,
    Loops,
    Capacity,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReportArgs {
    #[serde(default)]
    pub section: ReportSection,
    pub retry_key: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DecisionArgs {
    pub ledger: String,
    pub id: String,
    #[serde(default)]
    pub interactive: bool,
    pub retry_key: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Decision {
    ledger: String,
    id: String,
    body_hash: String,
    url: String,
}

pub(crate) struct DecisionView {
    ledger: String,
    id: String,
    url: Option<String>,
    state: Value,
    pub decision: Option<Decision>,
}

impl DecisionView {
    pub fn read(store: &maestro_store::Store, args: DecisionArgs, enabled: bool) -> Self {
        let DecisionArgs { ledger, id, .. } = args;
        let section = store.asks();
        let (rows, unread) = crate::ask_matches(store, &section, &ledger, &id);
        let url = crate::owner_web::web_origin()
            .filter(|origin| {
                origin.starts_with("https://")
                    && origin.parse::<axum::http::Uri>().ok().is_some_and(|uri| uri.host().is_some())
            })
            .zip(maestro_view::decision_path(&ledger, &id))
            .map(|(origin, path)| format!("{origin}{path}"));
        let (state, decision) = match rows.as_slice() {
            _ if section.data.is_none() || !unread.is_empty() => (json!("unreadable"), None),
            [ask] => {
                let state = ask.state();
                let decision =
                    url.as_ref().filter(|_| enabled && state == maestro_core::ask::AskState::Open).map(|url| {
                        Decision {
                            ledger: ledger.clone(),
                            id: id.clone(),
                            body_hash: maestro_flow::kernel::asks::comparable_hash(&ask.row),
                            url: url.clone(),
                        }
                    });
                (json!(state), decision)
            }
            [] => (json!("not_found"), None),
            _ => (json!("ambiguous"), None),
        };
        Self { ledger, id, url, state, decision }
    }

    pub fn fallback(&self, reason: &str) -> Value {
        json!({
            "askHandle": {"ledger": self.ledger, "id": self.id},
            "decisionsUrl": self.url,
            "nextTool": "maestro_ask_get",
            "currentState": self.state,
            "reason": reason,
        })
    }
}

enum Work {
    Report(ReportSection),
    Decision(Decision),
}

impl ReportSection {
    pub(crate) fn read(self, store: &maestro_store::Store) -> Result<Value, McpError> {
        let report = match self {
            Self::All => {
                json!({"census": store.health_census(), "capacity": store.health_capacity(), "loops": store.health_loops()})
            }
            Self::Census => json!({"census": store.health_census()}),
            Self::Capacity => json!({"capacity": store.health_capacity()}),
            Self::Loops => json!({"loops": store.health_loops()}),
        };
        let result = rmcp::model::CallToolResult::structured(report.clone());
        if serde_json::to_vec(&result).map_err(internal)?.len() > MAX_RESULT {
            return Err(internal("health report exceeded the result limit; request a smaller section"));
        }
        Ok(report)
    }
}

pub struct TaskStore {
    conn: Mutex<Connection>,
    workspace: String,
}

struct Record {
    id: String,
    state: String,
    created: i64,
    updated: i64,
    deadline: i64,
    body: Option<String>,
    status_message: Option<String>,
}

struct Lease {
    id: String,
    generation: i64,
    section: ReportSection,
}

impl Record {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            state: row.get(1)?,
            created: row.get(2)?,
            updated: row.get(3)?,
            deadline: row.get(4)?,
            body: row.get(5)?,
            status_message: row.get(6)?,
        })
    }

    fn wire(self) -> Result<DetailedTask, McpError> {
        let payload = match self.state.as_str() {
            "queued" | "running" | "watching" => TaskPayload::Working,
            "input_required" => {
                let decision: Decision =
                    serde_json::from_str(self.body.as_deref().unwrap_or_default()).map_err(internal)?;
                let request = ElicitRequest::new(ElicitRequestParams::UrlElicitationParams {
                    meta: None,
                    message: "Open the existing Maestro Decisions page to review this ask.".into(),
                    url: decision.url,
                    elicitation_id: self.id.clone(),
                });
                TaskPayload::InputRequired {
                    input_requests: [(DECISIONS_PROMPT.into(), InputRequest::Elicitation(request))].into(),
                }
            }
            "completed" => TaskPayload::Completed {
                result: serde_json::from_str(self.body.as_deref().unwrap_or_default()).map_err(internal)?,
            },
            "failed" => TaskPayload::Failed {
                error: as_object(
                    json!({"code": -32603, "message": self.body.unwrap_or_else(|| "task failed".into())}),
                )?,
            },
            "cancelled" => TaskPayload::Cancelled,
            _ => return Err(internal("unknown stored task state")),
        };
        let retain_until = (if payload.status().is_terminal() { self.updated } else { self.deadline }) + DAY_MS;
        let stamp = |ms| jiff::Timestamp::from_millisecond(ms).map(|t| t.to_string()).map_err(internal);
        let mut task = Task::new(self.id, TaskStatus::Working, stamp(self.created)?, stamp(self.updated)?)
            .with_poll_interval_ms(if matches!(self.state.as_str(), "input_required" | "watching") {
                OBSERVER_POLL_MS as u64
            } else {
                1_000
            })
            .with_ttl_ms((retain_until - self.created).max(0) as u64);
        task.status_message = self.status_message;
        Ok(DetailedTask::new(task, payload))
    }
}

fn as_object(value: Value) -> Result<serde_json::Map<String, Value>, McpError> {
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(internal("task payload must be an object")),
    }
}

const COLUMNS: &str = "id, state, created, updated, deadline, body, status_message";

impl TaskStore {
    /// Durable task state belongs to a single configured HTTP daemon and never to the ask ledger.
    pub fn open(path: &Path, workspace: &Path) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let filename = path.file_name().ok_or("the Task database needs a file name")?;
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let path = parent.canonicalize()?.join(filename);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            let metadata = path.symlink_metadata()?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
                return Err("the Task database must be a regular file accessible only to its owner".into());
            }
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.busy_timeout(Duration::from_secs(2))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS task (
                id TEXT PRIMARY KEY, owner TEXT NOT NULL, workspace TEXT NOT NULL,
                operation TEXT NOT NULL, retry_key TEXT NOT NULL, key_until INTEGER NOT NULL,
                args TEXT NOT NULL, state TEXT NOT NULL, created INTEGER NOT NULL,
                updated INTEGER NOT NULL, deadline INTEGER NOT NULL,
                attempt INTEGER NOT NULL DEFAULT 0, lease_until INTEGER NOT NULL DEFAULT 0, body TEXT, status_message TEXT,
                UNIQUE(owner, workspace, operation, retry_key)
            );",
        )?;
        let workspace = workspace
            .canonicalize()?
            .into_os_string()
            .into_string()
            .map_err(|_| "the Task workspace must have a UTF-8 path")?;
        Ok(Self { conn: Mutex::new(conn), workspace })
    }

    fn transact<T>(&self, now: i64, op: impl FnOnce(&Transaction<'_>) -> Result<T, McpError>) -> Result<T, McpError> {
        let mut conn = self.conn.lock().map_err(|_| internal("Task database lock lost"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(internal)?;
        tx.execute(
            &format!(
                "UPDATE task SET state='failed', updated=?1,
             body=CASE WHEN operation='decision' AND body IS NOT NULL AND state='watching'
                 THEN 'decision deadline expired or source unavailable' ELSE 'task deadline expired' END
             WHERE workspace=?2 AND {ACTIVE} AND deadline<=?1"
            ),
            params![now, self.workspace],
        )
        .map_err(internal)?;
        tx.execute(
            "DELETE FROM task WHERE workspace=?1 AND state IN ('completed','failed','cancelled')
             AND updated<=?2 AND key_until<=?3",
            params![self.workspace, now - DAY_MS, now],
        )
        .map_err(internal)?;
        // Even a request that cannot find its handle should persist expiration and collection.
        let value = op(&tx)?;
        tx.commit().map_err(internal)?;
        Ok(value)
    }

    pub(crate) fn report(&self, owner: &str, key: &str, section: ReportSection) -> Result<DetailedTask, McpError> {
        self.enqueue(owner, key, Work::Report(section))
    }

    pub(crate) fn decision(&self, owner: &str, key: &str, decision: Decision) -> Result<DetailedTask, McpError> {
        self.enqueue(owner, key, Work::Decision(decision))
    }

    pub(crate) fn report_retry(
        &self,
        owner: &str,
        key: &str,
        section: ReportSection,
    ) -> Result<Option<DetailedTask>, McpError> {
        self.existing(owner, key, "report", &serde_json::to_string(&section).map_err(internal)?)
    }

    pub(crate) fn decision_retry(
        &self,
        owner: &str,
        key: &str,
        ledger: &str,
        id: &str,
    ) -> Result<Option<DetailedTask>, McpError> {
        self.existing(owner, key, "decision", &serde_json::to_string(&[ledger, id]).map_err(internal)?)
    }

    fn existing(&self, owner: &str, key: &str, operation: &str, args: &str) -> Result<Option<DetailedTask>, McpError> {
        let now = now_ms();
        let (key, _) = retry_key(key, now)?;
        self.transact(now, |tx| self.find_retry(tx, owner, operation, &key.to_string(), args))?
            .map(Record::wire)
            .transpose()
    }

    fn find_retry(
        &self,
        tx: &Transaction<'_>,
        owner: &str,
        operation: &str,
        key: &str,
        args: &str,
    ) -> Result<Option<Record>, McpError> {
        let existing = tx
            .query_row(
                &format!(
                    "SELECT {COLUMNS}, args FROM task WHERE owner=?1 AND workspace=?2 AND operation=?3 AND retry_key=?4"
                ),
                params![owner, self.workspace, operation, key],
                |row| Ok((Record::read(row)?, row.get::<_, String>(7)?)),
            )
            .optional()
            .map_err(internal)?;
        match existing {
            Some((_, saved)) if saved != args => Err(invalid("retry_key already belongs to different arguments")),
            Some((record, _)) => Ok(Some(record)),
            None => Ok(None),
        }
    }

    fn enqueue(&self, owner: &str, key: &str, work: Work) -> Result<DetailedTask, McpError> {
        let now = now_ms();
        let (uuid, issued) = retry_key(key, now)?;
        let key = uuid.to_string();
        let (operation, state, args, body, ttl) = match work {
            Work::Report(section) => {
                ("report", "queued", serde_json::to_string(&section).map_err(internal)?, None, REPORT_MS)
            }
            Work::Decision(decision) => (
                "decision",
                "input_required",
                // Body and origin are pinned on first use. An identical request can reconnect after an answer.
                serde_json::to_string(&[&decision.ledger, &decision.id]).map_err(internal)?,
                Some(serde_json::to_string(&decision).map_err(internal)?),
                OBSERVER_MS,
            ),
        };
        let record = self.transact(now, |tx| {
            if let Some(record) = self.find_retry(tx, owner, operation, &key, &args)? {
                return Ok(record);
            }
            let (active, rows, reserved) = tx.query_row(
                &format!("SELECT COUNT(CASE WHEN {ACTIVE} THEN 1 END), COUNT(*),
                    COALESCE(SUM(CASE WHEN {ACTIVE} THEN ?1 ELSE COALESCE(length(CAST(body AS BLOB)),0) END),0)
                 FROM task WHERE owner=?2 AND workspace=?3"),
                params![MAX_RESULT as i64, owner, self.workspace],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
            ).map_err(internal)?;
            if active >= MAX_ACTIVE || rows >= MAX_ROWS || reserved + MAX_RESULT as i64 > MAX_RETAINED_BYTES {
                return Err(invalid("Task quota exceeded"));
            }
            let id = new_id()?;
            let deadline = now + ttl;
            let first_read = if operation == "decision" { now + OBSERVER_POLL_MS } else { 0 };
            tx.execute(
                "INSERT INTO task (id,owner,workspace,operation,retry_key,key_until,args,state,created,updated,deadline,body,lease_until)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?11,?12)",
                params![id, owner, self.workspace, operation, key, issued + DAY_MS, args, state, now, deadline, body, first_read],
            ).map_err(internal)?;
            Ok(Record { id, state: state.into(), created: now, updated: now, deadline, body, status_message: None })
        })?;
        record.wire()
    }

    pub(crate) fn get(&self, owner: &str, id: &str) -> Result<DetailedTask, McpError> {
        self.record(owner, id)?.wire()
    }

    fn record(&self, owner: &str, id: &str) -> Result<Record, McpError> {
        let record = self.transact(now_ms(), |tx| {
            tx.query_row(
                &format!("SELECT {COLUMNS} FROM task WHERE id=?1 AND owner=?2 AND workspace=?3"),
                params![id, owner, self.workspace],
                Record::read,
            )
            .optional()
            .map_err(internal)
        })?;
        record.ok_or_else(|| invalid("unknown task"))
    }

    pub(crate) fn cancel(&self, owner: &str, id: &str) -> Result<(), McpError> {
        let now = now_ms();
        let found = self.transact(now, |tx| {
            let state = tx.query_row(
                "SELECT state FROM task WHERE id=?1 AND owner=?2 AND workspace=?3",
                params![id, owner, self.workspace],
                |r| r.get::<_, String>(0),
            ).optional().map_err(internal)?;
            if state.as_deref().is_some_and(|s| matches!(s, "queued" | "running" | "input_required" | "watching")) {
                tx.execute(
                    "UPDATE task SET state='cancelled', updated=?1, body=NULL, status_message=NULL WHERE id=?2 AND owner=?3 AND workspace=?4",
                    params![now, id, owner, self.workspace],
                ).map_err(internal)?;
            }
            Ok(state.is_some())
        })?;
        if found { Ok(()) } else { Err(invalid("unknown task")) }
    }

    pub(crate) fn poll_decision(
        &self,
        owner: &str,
        id: &str,
        paths: &maestro_store::Paths,
    ) -> Result<DetailedTask, McpError> {
        self.poll_decision_with(owner, id, |decision| {
            maestro_flow::kernel::asks::observe_answer(paths, &decision.ledger, &decision.id, &decision.body_hash)
        })
    }

    fn poll_decision_with<E>(
        &self,
        owner: &str,
        id: &str,
        read: impl FnOnce(&Decision) -> Result<Option<String>, E>,
    ) -> Result<DetailedTask, McpError> {
        let record = self.record(owner, id)?;
        if !matches!(record.state.as_str(), "input_required" | "watching") {
            return record.wire();
        }
        let now = now_ms();
        let due = self.transact(now, |tx| {
            tx.execute(
                "UPDATE task SET lease_until=?1 WHERE id=?2 AND owner=?3 AND workspace=?4
                 AND state IN ('input_required','watching') AND lease_until<=?5",
                params![now + OBSERVER_POLL_MS, id, owner, self.workspace, now],
            )
            .map_err(internal)
        })?;
        if due == 0 {
            return self.get(owner, id);
        }
        let decision: Decision = serde_json::from_str(record.body.as_deref().unwrap_or_default()).map_err(internal)?;
        let (result, unavailable) = match read(&decision) {
            Ok(Some(source)) => {
                let result = rmcp::model::CallToolResult::structured(json!({
                    "askHandle": {"ledger": decision.ledger, "id": decision.id},
                    "currentState": "answered", "source": source,
                }));
                (Some(serde_json::to_string(&result).map_err(internal)?), None)
            }
            Ok(None) => (None, None),
            Err(_) => (None, Some("The authoritative answer source could not be read or verified.")),
        };
        self.transact(now_ms(), |tx| {
            tx.execute(
                "UPDATE task SET state=CASE WHEN ?1 IS NULL THEN state ELSE 'completed' END,
                 updated=?2, body=COALESCE(?1,body), status_message=?3
                 WHERE id=?4 AND owner=?5 AND workspace=?6 AND state IN ('input_required','watching')
                 AND lease_until=?7",
                params![result, now_ms(), unavailable, id, owner, self.workspace, now + OBSERVER_POLL_MS],
            )
            .map_err(internal)?;
            Ok(())
        })?;
        self.get(owner, id)
    }

    pub(crate) fn respond(&self, owner: &str, id: &str, responses: InputResponses) -> Result<(), McpError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Response {
            action: rmcp::model::ElicitationAction,
        }
        let response: Response = (responses.len() == 1)
            .then(|| responses.get(DECISIONS_PROMPT))
            .flatten()
            .and_then(|r| serde_json::from_value(r.clone()).ok())
            .ok_or_else(|| invalid("expected only the Decisions URL action"))?;
        let (state, clear) = match response.action {
            rmcp::model::ElicitationAction::Accept => ("watching", false),
            rmcp::model::ElicitationAction::Decline | rmcp::model::ElicitationAction::Cancel => ("cancelled", true),
            _ => return Err(invalid("unknown Decisions URL action")),
        };
        let now = now_ms();
        let updated = self.transact(now, |tx| {
            tx.execute(
                "UPDATE task SET state=?1, updated=?2, body=CASE WHEN ?3 THEN NULL ELSE body END, lease_until=?4, status_message=NULL
                 WHERE id=?5 AND owner=?6 AND workspace=?7 AND state='input_required'",
                params![state, now, clear, now + OBSERVER_POLL_MS, id, owner, self.workspace],
            )
            .map_err(internal)
        })?;
        if updated == 0 {
            return Err(invalid("no outstanding Decisions input for this task"));
        }
        Ok(())
    }

    fn claim(&self, now: i64) -> Result<Option<Lease>, McpError> {
        self.transact(now, |tx| {
            tx.execute(
                "UPDATE task SET state='failed', updated=?1, body='worker attempts exhausted'
                 WHERE workspace=?2 AND state='running' AND lease_until<=?1 AND attempt>=?3",
                params![now, self.workspace, MAX_ATTEMPTS],
            )
            .map_err(internal)?;
            let job = tx
                .query_row(
                    "SELECT id, attempt, args FROM task
                 WHERE workspace=?1 AND operation='report' AND (state='queued' OR (state='running' AND lease_until<=?2))
                   AND attempt<?3 ORDER BY created,id LIMIT 1",
                    params![self.workspace, now, MAX_ATTEMPTS],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?)),
                )
                .optional()
                .map_err(internal)?;
            let Some((id, attempt, args)) = job else { return Ok(None) };
            let section = serde_json::from_str(&args).map_err(internal)?;
            let generation = attempt + 1;
            tx.execute(
                "UPDATE task SET state='running', updated=?1, attempt=?2, lease_until=?3 WHERE id=?4 AND workspace=?5",
                params![now, generation, now + LEASE_MS, id, self.workspace],
            )
            .map_err(internal)?;
            Ok(Some(Lease { id, generation, section }))
        })
    }

    fn finish(&self, lease: &Lease, result: Result<Value, McpError>, now: i64) -> Result<(), McpError> {
        let (state, body) = match result {
            Ok(value) => {
                let body = value.to_string();
                if body.len() <= MAX_RESULT {
                    ("completed", body)
                } else {
                    ("failed", "health report exceeded the Task result limit".to_string())
                }
            }
            Err(error) => ("failed", error.message.into_owned()),
        };
        self.transact(now, |tx| {
            tx.execute(
                "UPDATE task SET state=?1, updated=?2, body=?3
                 WHERE id=?4 AND workspace=?5 AND state='running' AND attempt=?6 AND lease_until>?2",
                params![state, now, body, lease.id, self.workspace, lease.generation],
            )
            .map_err(internal)?;
            Ok(())
        })
    }

    pub(crate) fn run(server: MaestroServer, cancel: CancellationToken) {
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_millis(500));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = timer.tick() => {}
                }
                let server = server.clone();
                let recovery = cancel.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    let Some(tasks) = server.tasks.as_ref() else { return Ok(()) };
                    if let Some(lease) = tasks.claim(now_ms())? {
                        let result = lease.section.read(&server.store()).and_then(|data| {
                            serde_json::to_value(rmcp::model::CallToolResult::structured(data)).map_err(internal)
                        });
                        if !recovery.is_cancelled() {
                            tasks.finish(&lease, result, now_ms())?;
                        }
                    }
                    Ok::<(), McpError>(())
                })
                .await;
                if let Ok(Err(e)) = outcome {
                    eprintln!("maestro: Task worker: {}", e.message);
                }
            }
        });
    }
}

/// Authenticate on every request. The credential stays in HTTP transport headers and is never
/// passed to the model, included in tool args, or persisted with the Task.
pub(crate) fn caller(server: &MaestroServer, ctx: &RequestContext<RoleServer>) -> Result<String, McpError> {
    let headers = &ctx.extensions.get::<Parts>().ok_or_else(|| invalid("Task needs a verified HTTP caller"))?.headers;
    let header = |name| headers.get(name).and_then(|v| v.to_str().ok());
    let session: SessionRef = header("maestro-session")
        .ok_or_else(|| invalid("Task needs a verified HTTP caller"))?
        .parse()
        .map_err(|_| invalid("Task needs a verified HTTP caller"))?;
    let secret = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("Task needs a verified HTTP caller"))?;
    let mut args = json!({"cwd": header("maestro-cwd"), "node": header("maestro-node")});
    if session.surface == Surface::Direct {
        args["token"] = json!(secret);
    } else {
        let credential = URL_SAFE_NO_PAD.decode(secret).map_err(|_| invalid("Task needs a verified HTTP caller"))?;
        args["credential"] =
            serde_json::from_slice(&credential).map_err(|_| invalid("Task needs a verified HTTP caller"))?;
    }
    let verified = verified_caller(server.hosts(), Some(&session), &args, Ambient::Shared, Check::Request)
        .map_err(|_| invalid("Task needs a verified HTTP caller"))?
        .ok_or_else(|| invalid("Task needs a verified HTTP caller"))?;
    let marker = verified.owner_marker().ok_or_else(|| invalid("Task needs a verified HTTP caller"))?;
    Ok(json!([verified.session(), marker]).to_string())
}

pub(crate) fn invalid(message: &'static str) -> McpError {
    McpError::invalid_params(message, None)
}

fn internal(error: impl std::fmt::Display) -> McpError {
    McpError::internal_error(error.to_string(), None)
}

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn retry_key(key: &str, now: i64) -> Result<(Uuid, i64), McpError> {
    let uuid = Uuid::parse_str(key).map_err(|_| invalid("retry_key must be a UUIDv7"))?;
    if uuid.get_version_num() != 7 || uuid.get_variant() != uuid::Variant::RFC4122 {
        return Err(invalid("retry_key must be a UUIDv7"));
    }
    let issued = (uuid.as_u128() >> 80) as i64;
    if issued > now + 60_000 || issued + DAY_MS <= now {
        return Err(invalid("retry_key is outside its acceptance window"));
    }
    Ok((uuid, issued))
}

fn new_id() -> Result<String, McpError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(internal)?;
    Ok(uuid::Uuid::from_bytes(bytes).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(at: i64, nonce: u8) -> String {
        Uuid::from_u128(((at as u128) << 80) | 0x7000_8000_0000_0000_0000 | u128::from(nonce)).to_string()
    }

    #[test]
    fn retries_recovery_and_cancellation_are_durable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tasks.sqlite");
        let open = || TaskStore::open(&path, tmp.path()).unwrap();
        let store = open();
        let now = now_ms();
        let retry = key(now, 1);
        let task = store.report("a", &retry, ReportSection::Census).unwrap();
        let id = &task.task.task_id;
        for _ in 0..20 {
            assert_eq!(store.report("a", &retry, ReportSection::Census).unwrap().task.task_id, *id);
        }
        assert!(store.report("a", &retry, ReportSection::Capacity).is_err());
        assert!(store.report("a", &key(now - DAY_MS, 1), ReportSection::Census).is_err());
        assert!(store.get("b", id).is_err());
        assert!(store.cancel("b", id).is_err());

        let claimed = store.claim(now).unwrap().unwrap();
        assert_eq!(claimed.id, *id);
        drop(store);
        let store = open();
        assert!(store.claim(now + LEASE_MS - 1).unwrap().is_none());
        let recovered = store.claim(now + LEASE_MS).unwrap().unwrap();
        assert_eq!(recovered.generation, 2);
        store.finish(&claimed, Ok(json!({"stale": true})), now + LEASE_MS + 1).unwrap();
        assert_eq!(store.get("a", id).unwrap().payload, TaskPayload::Working);
        store.cancel("a", id).unwrap();
        drop(store);
        let store = open();
        store.finish(&recovered, Ok(json!({"late": true})), now + LEASE_MS + 1).unwrap();
        assert_eq!(store.get("a", id).unwrap().payload, TaskPayload::Cancelled);
        assert!(store.claim(now + 2 * LEASE_MS).unwrap().is_none());

        let retry = key(now, 2);
        let id = store.report("a", &retry, ReportSection::Census).unwrap().task.task_id;
        let pending = now_ms();
        assert_eq!(store.claim(pending).unwrap().unwrap().generation, 1);
        assert_eq!(store.claim(pending + LEASE_MS).unwrap().unwrap().generation, 2);
        assert!(store.claim(pending + 2 * LEASE_MS).unwrap().is_none());
        assert!(matches!(store.get("a", &id).unwrap().payload, TaskPayload::Failed { .. }));

        let other = tmp.path().join("other-workspace");
        std::fs::create_dir(&other).unwrap();
        assert!(TaskStore::open(&path, &other).unwrap().get("a", &id).is_err());
        for nonce in 3..7 {
            store.report("a", &key(now, nonce), ReportSection::Census).unwrap();
        }
        assert!(store.report("a", &key(now, 7), ReportSection::Census).is_err());
        let isolated = TaskStore::open(&path, &other).unwrap();
        assert!(isolated.report("a", &key(now, 7), ReportSection::Census).is_ok());
    }

    #[test]
    fn decision_input_never_writes_an_answer_or_completes_on_unread() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tasks.sqlite");
        let open = || TaskStore::open(&path, tmp.path()).unwrap();
        let store = open();
        let start = || Decision {
            ledger: "intake.yaml".into(),
            id: "fixture".into(),
            body_hash: "expected".into(),
            url: "https://owner.example/decisions/intake.yaml/fixture".into(),
        };
        let key = key(now_ms(), 1);
        let id = store.decision("owner", &key, start()).unwrap().task.task_id;
        assert_eq!(store.decision("owner", &key, start()).unwrap().task.task_id, id);
        assert_eq!(store.decision_retry("owner", &key, "intake.yaml", "fixture").unwrap().unwrap().task.task_id, id);
        assert!(store.decision_retry("owner", &key, "intake.yaml", "other").is_err());
        let TaskPayload::InputRequired { input_requests } = store.get("owner", &id).unwrap().payload else {
            panic!("expected URL prompt")
        };
        let request = serde_json::to_value(&input_requests[DECISIONS_PROMPT]).unwrap();
        assert_eq!(request["method"], "elicitation/create");
        assert_eq!(request["params"]["mode"], "url");
        assert_eq!(request["params"]["url"], start().url);

        let respond = |v| [(DECISIONS_PROMPT.into(), v)].into();
        assert!(store.respond("foreign", &id, respond(json!({"action": "accept"}))).is_err());
        let forged = respond(json!({"action": "accept", "content": {"answer": "yes"}}));
        assert!(store.respond("owner", &id, forged).is_err());
        assert_eq!(store.get("owner", &id).unwrap().task.status, TaskStatus::InputRequired);
        store.respond("owner", &id, respond(json!({"action": "accept"}))).unwrap();
        assert_eq!(store.get("owner", &id).unwrap().task.status, TaskStatus::Working);
        drop(store);

        let store = open();
        let due = || {
            store.conn.lock().unwrap().execute("UPDATE task SET lease_until=0 WHERE id=?1", [&id]).unwrap();
        };
        due();
        let unread = store.poll_decision_with("owner", &id, |_| Err("unread")).unwrap();
        assert_eq!(unread.payload, TaskPayload::Working);
        assert!(unread.task.status_message.is_some());
        for _ in 0..2 {
            due();
            let waiting = store.poll_decision_with("owner", &id, |_| Ok::<_, ()>(None)).unwrap();
            assert_eq!(waiting.payload, TaskPayload::Working);
        }
        due();
        let known = store
            .poll_decision_with("owner", &id, |decision| {
                assert_eq!(decision.body_hash, "expected");
                Ok::<_, ()>(Some(String::from("main@verified")))
            })
            .unwrap();
        assert!(matches!(known.payload, TaskPayload::Completed { .. }));
        assert!(store.respond("owner", &id, respond(json!({"action": "accept"}))).is_err());
    }
}
