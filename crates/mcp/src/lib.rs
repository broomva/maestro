//! Maestro over MCP: the same reads as the CLI, plus context as prompts and resources.
//! Served on stdio ([`serve_stdio`]) and over Streamable HTTP ([`http`]), both through
//! [`MaestroServer`], which dispatches through [`call`] as the CLI does.
//!
//! Tools have [`Authority::Read`] or [`Authority::AgentRequest`] (BRO-3267): an agent may
//! file a validated ask and record a request, and nothing it requests starts anything (A8).
//! Owner actions are not exposed: a model-visible tool is not proof of the owner. Every
//! MCP caller is [`Principal::Unverified`] until a surface adapter verifies sessions
//! (BRO-3268), so its requests are recorded as unverified and, but for an ask, held.

use maestro_core::ask::AskState;
use maestro_core::authority::{Authority, Principal};
use maestro_core::context::ContextScope;
use maestro_core::request::RequestKind;
use maestro_core::work::WorkState;
use maestro_store::{Store, requests, views};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams, GetPromptResponse,
    GetPromptResult, Implementation, ListPromptsResult, ListResourcesResult, ListToolsResult, MetaObject,
    PaginatedRequestParams, Prompt, PromptArgument, PromptMessage, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents, Role, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::future::Future;
use std::sync::Arc;

pub mod http;

/// One tool: its name, what it may do, and its argument type.
pub struct ToolSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub authority: Authority,
    /// The agent request a tool makes; `None` for a read.
    pub request: Option<RequestKind>,
    /// What the tool may change: `read` for reads; for a request, the one place it writes.
    pub scope: &'static str,
    /// Where the tool shows in an MCP Apps host.
    pub ui: Ui,
    schema: fn() -> Value,
}

/// A tool's place in an MCP Apps host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ui {
    /// A plain tool.
    None,
    /// The host opens the App with this tool's result; the model and the App may call it.
    Opens,
    /// Meant for the App only: `_meta.ui.visibility` is `["app"]`. A host that honours it hides
    /// the tool from the model; plain `tools/list` still lists it, so it must stay read-only.
    AppOnly,
}

fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| json!({"type": "object"}))
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NoArgs {}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NeedsArgs {
    /// Only holds for this project.
    pub project: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AsksArgs {
    /// `open` (default), `answered`, `archived` or `all`.
    pub state: Option<String>,
    pub project: Option<String>,
    /// Only asks that block a lane.
    #[serde(default)]
    pub blocking: bool,
    /// Maximum asks returned (default 50).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskGetArgs {
    /// Ledger file name, e.g. `bro-2827-answers-landing.yaml`.
    pub ledger: String,
    /// Ask `uid` or `id`.
    pub id: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WorkArgs {
    /// Canonical state label: `Queued`, `Running`, `Stuck`, `Needs you`, `Done`, `Standing`.
    pub state: Option<String>,
    /// Maximum items returned (default 50).
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct LoopsArgs {
    /// One loop id; omit for all.
    pub id: Option<String>,
    /// Only loops with drift or notices.
    #[serde(default)]
    pub drifting: bool,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct InitiativesArgs {
    /// Include parked, done and killed initiatives.
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UiRenderArgs {
    /// The block id to start from.
    pub root: String,
    /// Blocks by id: `{ type, props?, children? }`, types from `maestro_ui_catalog`.
    pub blocks: serde_json::Map<String, Value>,
    /// `app` (default) for an MCP Apps host; `snapshot` returns a standalone HTML document as
    /// the text result, for a host without MCP Apps (T3's `html_render` in Claude threads).
    pub format: Option<String>,
    /// `light` or `dark`, for a snapshot.
    pub theme: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UiPollArgs {
    /// The spec the last render placed (its `spec`).
    pub spec: Value,
    /// The revision the App holds; an equal one returns no html.
    pub revision: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ContextArgs {
    /// The session's working directory; project and initiative are inferred from it.
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub initiative: Option<String>,
    /// Maximum holds listed (default 8).
    pub max_needs: Option<usize>,
}

/// Every agent request names the session making it.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskFileArgs {
    /// Your session as `surface:environment/id`, e.g. `t3:local/6ff25f7c` or `paseo:-/ae7d9f21`.
    pub session: String,
    /// Ledger file name in the asks folder, e.g. `bro-3261-maestro-production.yaml`. It must exist: a ledger is opened with its arc in a workspace change.
    pub ledger: String,
    /// The ask, as ask_ledger.py's schema and the decision layer (`.control/asks/DECISION-LAYER.md`)
    /// want it: `id`, `ask`, `class`, `gates`, `exhausted`, `blocking`, `preanswerable`, `default`,
    /// `headline`, `stakes`, `why_you`, `options`, `tradeoffs`, `sources`. Never an answer, a status
    /// or an origin: Maestro writes `asked_at` (when absent) and `origin`.
    pub ask: serde_json::Map<String, Value>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WorkQueueArgs {
    pub session: String,
    pub title: String,
    /// What the work is and how its done is checked.
    pub brief: String,
    pub project: Option<String>,
    pub initiative: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ActionRequestArgs {
    pub session: String,
    /// The one exact action, e.g. a command line or a permission.
    pub action: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SessionCloseArgs {
    pub session: String,
    /// The evidence the session's work is finished: PRs, merge SHAs, checks run.
    pub receipt: String,
    pub summary: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct HandbackArgs {
    pub session: String,
    pub summary: String,
    /// What the next owner of the work should do first.
    pub next: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct EscalateArgs {
    pub session: String,
    /// Why the owner's attention is needed now.
    pub reason: String,
}

pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "maestro_snapshot",
        title: "Snapshot",
        description: "Counts across the control plane: needs, asks, work by state, loops with drift, live initiatives, fleet. Every section names its source and any read errors; a section with data null was not readable, which is not the same as empty.",
        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::None,
        schema: schema_of::<NoArgs>,
    },
    ToolSpec {
        name: "maestro_needs_list",
        title: "Needs you",
        description: "Holds on the owner: open blocking asks, and queued work only the owner may start. Work paused on a permission prompt is live surface state and joins when a surface adapter reports it. This list is the Needs you count; nothing else is. data null means the asks could not be read: the count is unknown, not zero.",
        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::None,
        schema: schema_of::<NeedsArgs>,
    },
    ToolSpec {
        name: "maestro_asks_list",
        title: "Asks",
        description: "Asks from .control/asks, filtered by state, project and blocking. Read before filing a new ask so you never duplicate one.",
        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::None,
        schema: schema_of::<AsksArgs>,
    },
    ToolSpec {
        name: "maestro_ask_get",
        title: "Ask",
        description: "One ask in full, with its entry hash: the Paseo plugin's entryHash over the raw entry, which its answer path rechecks. Any change to the entry, an answer included, changes the hash.",
        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::None,
        schema: schema_of::<AskGetArgs>,
    },
    ToolSpec {
        name: "maestro_work_list",
        title: "Work",
        description: "Work items with their canonical state (Queued, Running, Stuck, Needs you, Done, Standing), labelled as the Paseo plugin labels them. Work at the gate is labelled Needs you with finished: true; it is not a need unless it appears in maestro_needs_list.",
        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::None,
        schema: schema_of::<WorkArgs>,
    },
    ToolSpec {
        name: "maestro_loops_get",
        title: "Loops",
        description: "Standing loops from .broomva/loops joined with their tick receipts. drift holds plugin drift classes the ticks ledger proves (tick_unclosed); notices holds Maestro's own signals (review_overdue, no_tick_receipts). Substrate drift needs surface adapters and is not claimed. Ticks are read from the ledger's tail, as the plugin reads it (its STREAM_TAIL: the last 2,097,152 UTF-16 units, from the first whole line); ticks_recorded counts those, and no_tick_receipts is raised only when the tail is the whole ledger.",
        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::None,
        schema: schema_of::<LoopsArgs>,
    },
    ToolSpec {
        name: "maestro_initiatives_list",
        title: "Initiatives",
        description: "Initiatives from .control/initiatives: intent, status, driver and next action. Live ones by default.",
        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::None,
        schema: schema_of::<InitiativesArgs>,
    },
    ToolSpec {
        name: "maestro_fleet_get",
        title: "Fleet",
        description: "The latest fleet-reconcile report per scope, condensed. The fleet skill acts; Maestro only reads.",
        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::None,
        schema: schema_of::<NoArgs>,
    },
    ToolSpec {
        name: "maestro_context",
        title: "Context",
        description: "The governance context for a session: holds in its scope, its initiative, loops needing attention, unread sources and standing rules, as markdown plus structured data.",
        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::None,
        schema: schema_of::<ContextArgs>,
    },
    ToolSpec {
        name: "maestro_ask_file",
        title: "File an ask",
        description: "File one ask into a ledger in the asks folder, through the workspace's writer of record (ask_ledger.py file), which holds the ledger's lock. Read maestro_asks_list first so you never duplicate one. Nothing is written unless the ledger reads back as its asks plus exactly this one with no new validator error. Until sessions are verified your origin is recorded as unverified, and answers are not routed back into your session.",
        authority: Authority::AgentRequest,
        request: Some(RequestKind::AskFile),
        scope: "asks: appends one ask to one ledger",
        ui: Ui::None,
        schema: schema_of::<AskFileArgs>,
    },
    ToolSpec {
        name: "maestro_work_queue",
        title: "Queue work",
        description: "Queue work for the owner to dispatch. It is recorded and held: nothing starts until the owner dispatches it (A8).",
        authority: Authority::AgentRequest,
        request: Some(RequestKind::WorkQueue),
        scope: "requests journal: appends one held request",
        ui: Ui::None,
        schema: schema_of::<WorkQueueArgs>,
    },
    ToolSpec {
        name: "maestro_action_request",
        title: "Request an action",
        description: "Request one exact action the owner must allow, such as a command or a permission. It is recorded and held for the owner's decision; nothing runs.",
        authority: Authority::AgentRequest,
        request: Some(RequestKind::ActionRequest),
        scope: "requests journal: appends one held request",
        ui: Ui::None,
        schema: schema_of::<ActionRequestArgs>,
    },
    ToolSpec {
        name: "maestro_session_close",
        title: "Close with a receipt",
        description: "Record that your session's work is finished, with the receipt that shows it. It closes nothing by itself: work reaches Done only through gates, checks or the owner (A1).",
        authority: Authority::AgentRequest,
        request: Some(RequestKind::SessionClose),
        scope: "requests journal: appends one request",
        ui: Ui::None,
        schema: schema_of::<SessionCloseArgs>,
    },
    ToolSpec {
        name: "maestro_handback",
        title: "Hand back",
        description: "Hand your work back to the owner with a summary and the first next step. Recorded for the owner.",
        authority: Authority::AgentRequest,
        request: Some(RequestKind::Handback),
        scope: "requests journal: appends one request",
        ui: Ui::None,
        schema: schema_of::<HandbackArgs>,
    },
    ToolSpec {
        name: "maestro_escalate",
        title: "Escalate",
        description: "Escalate to the owner: something needs their attention now. Recorded for the owner.",
        authority: Authority::AgentRequest,
        request: Some(RequestKind::Escalate),
        scope: "requests journal: appends one request",
        ui: Ui::None,
        schema: schema_of::<EscalateArgs>,
    },
    ToolSpec {
        name: "maestro_ui_catalog",
        title: "UI catalog",
        description: "The block types maestro_ui_render accepts, with their props. Blocks carry types and ledger handles only; Maestro reads every value shown.",
        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::None,
        schema: schema_of::<NoArgs>,
    },
    ToolSpec {
        name: "maestro_ui_render",
        title: "Show in Maestro",
        description: "Render a screen from catalog blocks: { root, blocks: { id: { type, props?, children? } } }. Maestro validates the spec, reads every value from the ledgers at call time, and reports each dropped block with its reason. In an MCP Apps host the result opens Maestro's App, which keeps it current; format snapshot returns a standalone HTML document instead.",
        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::Opens,
        schema: schema_of::<UiRenderArgs>,
    },
    ToolSpec {
        name: "maestro_ui_poll",
        title: "Refresh view",
        description: "For Maestro's App: render a placed spec again and return html only when its revision changed.",
        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::AppOnly,
        schema: schema_of::<UiPollArgs>,
    },
];

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, McpError> {
    serde_json::from_value(args).map_err(|e| McpError::invalid_params(e.to_string(), None))
}

fn parse_work_state(label: &str) -> Result<WorkState, McpError> {
    serde_json::from_value(Value::String(label.to_string()))
        .map_err(|_| McpError::invalid_params(format!("unknown state `{label}`"), None))
}

/// Run one tool. Pure dispatch over the store, so tests need no transport.
pub fn call(store: &Store, name: &str, args: Value) -> Result<Value, McpError> {
    let args = if args.is_null() { json!({}) } else { args };
    match name {
        "maestro_snapshot" => Ok(json!(views::snapshot(store))),
        "maestro_needs_list" => {
            let a: NeedsArgs = parse(args)?;
            let snap = views::snapshot(store);
            let mut section = snap.needs;
            if let (Some(p), Some(list)) = (a.project.as_deref(), section.data.as_mut()) {
                list.retain(|n| n.project.as_deref() == Some(p));
            }
            Ok(json!(section))
        }
        "maestro_asks_list" => {
            let a: AsksArgs = parse(args)?;
            let want = match a.state.as_deref().unwrap_or("open") {
                "open" => Some(AskState::Open),
                "answered" => Some(AskState::Answered),
                "archived" => Some(AskState::Archived),
                "all" => None,
                other => return Err(McpError::invalid_params(format!("unknown ask state `{other}`"), None)),
            };
            let section = store.asks();
            let unlanded = views::unlanded(store, section.data.as_deref().unwrap_or_default());
            let waiting = unlanded.data.clone().unwrap_or_default();
            let limit = a.limit.unwrap_or(50);
            let matching: Vec<_> = section
                .data
                .iter()
                .flatten()
                .flat_map(|lf| lf.ledger.asks.iter().map(move |ask| (lf, ask)))
                .filter(|(_, ask)| want.is_none_or(|w| ask.state() == w))
                .filter(|(_, ask)| !a.blocking || ask.blocking)
                .filter(|(_, ask)| a.project.is_none() || ask.project == a.project)
                .collect();
            let total = matching.len();
            let rows: Vec<Value> = matching
                .into_iter()
                .take(limit)
                .map(|(lf, ask)| {
                    json!({
                        "ledger": lf.file,
                        "id": ask.handle(),
                        "title": ask.title(),
                        "state": ask.state(),
                        "class": ask.class,
                        "blocking": ask.blocking,
                        "project": ask.project,
                        "asked_at": ask.asked_at,
                        "recommended": ask.recommended().map(|o| &o.id),
                        "hash": ask.hash(),
                        "waiting_to_land": waiting.iter().any(|r| r.ledger == lf.file && r.id == ask.handle()),
                    })
                })
                .collect();
            let mut errors = section.errors;
            errors.extend(unlanded.errors);
            Ok(json!({
                "source": section.source,
                "errors": errors,
                "readable": section.data.is_some(),
                "total": total,
                "truncated": total > rows.len(),
                "asks": rows,
            }))
        }
        "maestro_ask_get" => {
            let a: AskGetArgs = parse(args)?;
            let section = store.asks();
            // A uid names one ask; a bare id names one only when no other uid-less ask shares it.
            let matches: Vec<_> = section
                .data
                .iter()
                .flatten()
                .filter(|lf| lf.file == a.ledger)
                .flat_map(|lf| lf.ledger.asks.iter())
                .filter(|ask| ask.uid.as_deref() == Some(&a.id) || (ask.uid.is_none() && ask.id == a.id))
                .collect();
            // A ledger that did not parse, or an entry in it that did not, may be the one asked for:
            // that is "unreadable", which is not the same as "no such ask".
            // Errors name the folder the registry chose; an unresolved one names no file.
            let file_at = match store.rel(maestro_store::ASKS) {
                Ok(dir) => format!("{dir}/{}", a.ledger),
                Err(_) => section.source.clone(),
            };
            let unread: Vec<_> = section
                .errors
                .iter()
                .filter(|e| e.at == file_at || e.at.starts_with(&format!("{file_at}#")) || e.at == section.source)
                .cloned()
                .collect();
            match matches.as_slice() {
                [ask] => Ok(
                    json!({ "source": section.source, "state": ask.state(), "hash": ask.hash(), "ask": ask, "row": ask.row }),
                ),
                [] if !unread.is_empty() => Ok(json!({
                    "source": section.source,
                    "state": "unreadable",
                    "detail": format!("{}#{} was not found, and part of {} could not be read", a.ledger, a.id, a.ledger),
                    "errors": unread,
                })),
                [] => {
                    Err(McpError::invalid_params(format!("no ask {}#{} in {}", a.ledger, a.id, section.source), None))
                }
                _ => Err(McpError::invalid_params(
                    format!("{}#{} names {} asks; address it by uid", a.ledger, a.id, matches.len()),
                    None,
                )),
            }
        }
        "maestro_work_list" => {
            let a: WorkArgs = parse(args)?;
            let state = a.state.as_deref().map(parse_work_state).transpose()?;
            let section = store.work();
            let items: Vec<_> = views::work_by_state(section.data.as_deref().unwrap_or_default(), state)
                .into_iter()
                .take(a.limit.unwrap_or(50))
                .cloned()
                .collect();
            Ok(
                json!({ "source": section.source, "errors": section.errors, "readable": section.data.is_some(), "items": items }),
            )
        }
        "maestro_loops_get" => {
            let a: LoopsArgs = parse(args)?;
            let mut section = views::snapshot(store).loops;
            if let Some(rows) = section.data.as_mut() {
                rows.retain(|r| {
                    a.id.as_deref().is_none_or(|id| r.id == id)
                        && (!a.drifting || !r.drift.is_empty() || !r.notices.is_empty())
                });
            }
            Ok(json!(section))
        }
        "maestro_initiatives_list" => {
            let a: InitiativesArgs = parse(args)?;
            let section =
                store.initiatives().map(|v| v.into_iter().filter(|i| a.all || i.status.is_live()).collect::<Vec<_>>());
            Ok(json!(section))
        }
        "maestro_fleet_get" => Ok(json!(store.fleet())),
        "maestro_ui_catalog" => Ok(maestro_view::catalog()),
        "maestro_ui_render" => {
            let a: UiRenderArgs = parse(args)?;
            let snapshot = match a.format.as_deref().unwrap_or("app") {
                "app" => false,
                "snapshot" => true,
                other => return Err(McpError::invalid_params(format!("unknown format `{other}`"), None)),
            };
            let theme = match a.theme.as_deref() {
                None => None,
                Some(t @ ("light" | "dark")) => Some(t),
                Some(other) => return Err(McpError::invalid_params(format!("unknown theme `{other}`"), None)),
            };
            let spec = json!({ "root": a.root, "blocks": a.blocks });
            let v = maestro_view::validate(&spec).map_err(|e| McpError::invalid_params(e, None))?;
            let r = maestro_view::render(store, &v);
            let mut out = json!(r);
            out["text"] = if snapshot {
                Value::String(maestro_view::page::snapshot(&r.html, theme))
            } else {
                Value::String(render_summary(&r))
            };
            Ok(out)
        }
        "maestro_ui_poll" => {
            let a: UiPollArgs = parse(args)?;
            let v = maestro_view::validate(&a.spec).map_err(|e| McpError::invalid_params(e, None))?;
            let r = maestro_view::render(store, &v);
            if a.revision.as_deref() == Some(r.revision.as_str()) {
                Ok(json!({ "revision": r.revision, "changed": false, "tree": r.tree }))
            } else {
                let mut out = json!(r);
                out["changed"] = Value::Bool(true);
                Ok(out)
            }
        }
        "maestro_context" => {
            let a: ContextArgs = parse(args)?;
            let packet =
                views::context(store, ContextScope { cwd: a.cwd, project: a.project, initiative: a.initiative });
            let markdown = packet.render_markdown(a.max_needs.unwrap_or(8));
            Ok(json!({ "markdown": markdown, "packet": packet }))
        }
        other => match request_of(other) {
            Some(kind) => agent_request(store, kind, args),
            None => Err(McpError::invalid_params(format!("unknown tool `{other}`"), None)),
        },
    }
}

fn request_of(tool: &str) -> Option<RequestKind> {
    TOOLS.iter().find(|t| t.name == tool).and_then(|t| t.request)
}

fn session_of(text: &str) -> Result<maestro_core::session::SessionRef, McpError> {
    text.parse().map_err(|e: String| McpError::invalid_params(format!("`session`: {e}"), None))
}

/// One agent request from an unverified caller (every caller until BRO-3268): an ask is filed,
/// anything else recorded and held. Each is journalled with its disposition.
pub fn agent_request(store: &Store, kind: RequestKind, args: Value) -> Result<Value, McpError> {
    let caller = Principal::Unverified;
    let session_text = args.get("session").and_then(Value::as_str).unwrap_or_default().to_string();
    let payload = match kind {
        RequestKind::AskFile => {
            let a: AskFileArgs = parse(args.clone())?;
            let session = session_of(&a.session)?;
            // Writes go to the working tree, never to a git revision.
            requests::compose_ask(&a.ask).map_err(|e| McpError::invalid_params(e.to_string(), None))?;
            let disk = Store::new(store.paths.clone(), maestro_store::TreeSource::WorkingTree);
            let rel = disk.rel(maestro_store::ASKS).map_err(|e| McpError::invalid_params(e, None))?;
            let dir = disk.paths.workspace.join(rel);
            let filed =
                requests::file_ask(&disk.paths, &dir, &a.ledger, &a.ask, &session, &caller).map_err(file_error)?;
            let mut record =
                requests::new_record(kind, session, &caller, json!({ "ledger": a.ledger, "id": filed.id }));
            record.result = Some(json!({ "ledger": filed.ledger, "id": filed.id }));
            // The ask is written, so the request failed only if its journal line was not: say both.
            if let Err(e) = requests::record(&disk.paths, &record) {
                return Err(unjournalled(&filed.ledger, &filed.id, &e));
            }
            return Ok(json!({ "request": record, "validator": filed.validator }));
        }
        RequestKind::WorkQueue => json!(parse::<WorkQueueArgs>(args)?),
        RequestKind::ActionRequest => json!(parse::<ActionRequestArgs>(args)?),
        RequestKind::SessionClose => json!(parse::<SessionCloseArgs>(args)?),
        RequestKind::Handback => json!(parse::<HandbackArgs>(args)?),
        RequestKind::Escalate => json!(parse::<EscalateArgs>(args)?),
    };
    let session = session_of(&session_text)?;
    let record = requests::new_record(kind, session, &caller, payload);
    requests::record(&store.paths, &record).map_err(file_error)?;
    Ok(json!({ "request": record, "journal": requests::journal(&store.paths) }))
}

/// An ask filed whose journal line was not written. Never busy, even when the journal's lock was:
/// the ask is in the ledger, so a retry is refused as a duplicate.
pub fn unjournalled(ledger: &std::path::Path, id: &str, e: &requests::FileError) -> McpError {
    McpError::internal_error(
        format!(
            "filed `{id}` in {}, but its requests journal line was not written: {}. The ask stands; filing it again is refused as a duplicate.",
            ledger.display(),
            e.to_string().trim_end_matches('.')
        ),
        Some(json!({ "filed": { "ledger": ledger, "id": id } })),
    )
}

fn is_busy(e: &requests::FileError) -> bool {
    matches!(e, requests::FileError::Lock(maestro_store::locks::LockError::Busy { .. }))
}

/// A refusal is the caller's to fix; a busy lock is to retry (`busy: true`; the CLI exits 75).
fn file_error(e: requests::FileError) -> McpError {
    match e {
        requests::FileError::Refused(m) => McpError::invalid_params(m, None),
        other => {
            let busy = is_busy(&other);
            McpError::internal_error(other.to_string(), busy.then(|| json!({ "busy": true })))
        }
    }
}

/// What a render placed and dropped, for the model; the App shows the html itself.
fn render_summary(r: &maestro_view::Rendered) -> String {
    let mut out = format!("Rendered from {} (revision {}).", r.tree, r.revision.get(..12).unwrap_or_default());
    if r.drops.is_empty() {
        out.push_str(" Nothing was dropped.");
    } else {
        out.push_str(&format!(" {} dropped:", r.drops.len()));
        for d in &r.drops {
            out.push_str(&format!("\n- `{}`: {}", d.block, d.reason));
        }
    }
    out
}

pub const RESOURCES: &[(&str, &str, &str)] = &[
    ("maestro://snapshot", "snapshot", "Control-plane snapshot (JSON)"),
    ("maestro://needs", "needs", "Holds on the owner (JSON)"),
    ("maestro://context", "context", "Governance context for this server's workspace (markdown)"),
    (
        maestro_view::page::APP_URI,
        "view",
        "Maestro's MCP App: renders maestro_ui_render results and keeps them current",
    ),
];

/// A long-lived server. It keeps where to read, never what it read: each request
/// builds its own [`Store`], so a git revision resolves anew per call and the server
/// follows `origin/main` as it moves instead of answering from the commit it started on.
#[derive(Clone)]
pub struct MaestroServer {
    paths: Arc<maestro_store::Paths>,
    source: maestro_store::TreeSource,
}

impl MaestroServer {
    pub fn new(store: Store) -> Self {
        Self { paths: Arc::new(store.paths), source: store.tree.source }
    }

    /// The store one request reads: resolved now, consistent within the request.
    pub fn store(&self) -> Store {
        Store::new((*self.paths).clone(), self.source.clone())
    }

    /// Run one tool against a store resolved for this call.
    pub fn call(&self, name: &str, args: Value) -> Result<Value, McpError> {
        call(&self.store(), name, args)
    }

    pub fn tools() -> Vec<Tool> {
        TOOLS
            .iter()
            .map(|spec| {
                let schema = (spec.schema)();
                let object = schema.as_object().cloned().unwrap_or_default();
                let tool =
                    Tool::new(spec.name, spec.description, Arc::new(object)).with_title(spec.title).with_annotations(
                        ToolAnnotations::with_title(spec.title)
                            .read_only(spec.authority == Authority::Read)
                            .destructive(false)
                            .idempotent(spec.authority == Authority::Read)
                            .open_world(false),
                    );
                match ui_meta(spec.ui) {
                    Some(meta) => tool.with_meta(meta),
                    None => tool,
                }
            })
            .collect()
    }

    fn read_resource_text(&self, uri: &str) -> Result<(String, &'static str), McpError> {
        let store = &self.store();
        match uri {
            "maestro://snapshot" => Ok((json!(views::snapshot(store)).to_string(), "application/json")),
            "maestro://needs" => Ok((json!(views::snapshot(store).needs).to_string(), "application/json")),
            "maestro://context" => {
                let packet = views::context(store, ContextScope::default());
                Ok((packet.render_markdown(8), "text/markdown"))
            }
            maestro_view::page::APP_URI => Ok((maestro_view::page::app(), maestro_view::page::APP_MIME)),
            other => Err(McpError::resource_not_found(format!("no resource {other}"), None)),
        }
    }
}

/// The MCP Apps `_meta` for a tool: `ui.resourceUri` and `ui.visibility`, plus the flat
/// `ui/resourceUri` key older hosts read.
fn ui_meta(ui: Ui) -> Option<MetaObject> {
    let visibility = match ui {
        Ui::None => return None,
        Ui::Opens => json!(["model", "app"]),
        Ui::AppOnly => json!(["app"]),
    };
    let uri = maestro_view::page::APP_URI;
    let mut meta = MetaObject::new();
    meta.insert("ui".into(), json!({ "resourceUri": uri, "visibility": visibility }));
    meta.insert("ui/resourceUri".into(), json!(uri));
    Some(meta)
}

fn prompt_arg(name: &str, description: &str) -> PromptArgument {
    let mut arg = PromptArgument::default();
    arg.name = name.to_string();
    arg.description = Some(description.to_string());
    arg.required = Some(false);
    arg
}

impl ServerHandler for MaestroServer {
    fn get_info(&self) -> ServerConfig {
        let mut implementation = Implementation::new("maestro", env!("CARGO_PKG_VERSION"));
        implementation.title = Some("Maestro".to_string());
        implementation.description = Some("Governance and orchestration metalayer for agent fleets".to_string());
        let mut info = ServerConfig::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().enable_prompts().enable_resources().build();
        info.server_info = implementation;
        info.instructions = Some(
            "Maestro reports the governance state of the workspace: what needs the owner, open asks, \
             work states, loop health, initiatives and the fleet. Call maestro_context at the start of \
             substantive work and before filing an ask. Agent tools file an ask (maestro_ask_file) or \
             record a request for the owner (queue work, request an action, close with a receipt, hand \
             back, escalate); none of them starts anything. Answering asks, approving work and dispatch \
             are owner actions taken in the owner's app."
                .to_string(),
        );
        info
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(Self::tools())))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + Send + '_ {
        let this = self.clone();
        async move {
            let name = request.name.to_string();
            let args = Value::Object(request.arguments.unwrap_or_default());
            // Reads shell out to git; keep them off the async workers.
            let outcome = tokio::task::spawn_blocking(move || this.call(&name, args))
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match outcome {
                Ok(value) => {
                    if let Some(md) = value.get("markdown").or_else(|| value.get("text")).and_then(|m| m.as_str()) {
                        let mut result = CallToolResult::structured(value.clone());
                        result.content = vec![ContentBlock::text(md.to_string())];
                        Ok(result.into())
                    } else {
                        Ok(CallToolResult::structured(value).into())
                    }
                }
                Err(e) if e.code == rmcp::model::ErrorCode::INVALID_PARAMS => {
                    Ok(CallToolResult::error(vec![ContentBlock::text(e.message.to_string())]).into())
                }
                Err(e) => Err(e),
            }
        }
    }

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + Send + '_ {
        let prompt = Prompt::new(
            "maestro_context",
            Some("Load the governance context for a session before substantive work."),
            Some(vec![
                prompt_arg("cwd", "Working directory of the session"),
                prompt_arg("project", "Project name, when not inferable from cwd"),
                prompt_arg("initiative", "Initiative id to scope to"),
            ]),
        );
        std::future::ready(Ok(ListPromptsResult::with_all_items(vec![prompt])))
    }

    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetPromptResponse, McpError>> + Send + '_ {
        let this = self.clone();
        async move {
            if request.name != "maestro_context" {
                return Err(McpError::invalid_params(format!("unknown prompt `{}`", request.name), None));
            }
            let args = request.arguments.unwrap_or_default();
            let get = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let scope = ContextScope { cwd: get("cwd"), project: get("project"), initiative: get("initiative") };
            let packet = tokio::task::spawn_blocking(move || views::context(&this.store(), scope))
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            let mut result = GetPromptResult::new(vec![PromptMessage::new_text(Role::User, packet.render_markdown(8))]);
            result.description = Some("Maestro governance context".to_string());
            Ok(result.into())
        }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        let resources = RESOURCES
            .iter()
            .map(|(uri, name, description)| {
                let mut resource = Resource::new(*uri, *name);
                resource.description = Some(description.to_string());
                resource
            })
            .collect();
        std::future::ready(Ok(ListResourcesResult::with_all_items(resources)))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, McpError>> + Send + '_ {
        let this = self.clone();
        async move {
            let uri = request.uri.clone();
            let (text, mime) = tokio::task::spawn_blocking(move || this.read_resource_text(&uri))
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))??;
            let mut contents = ResourceContents::text(text, request.uri);
            if let ResourceContents::TextResourceContents { mime_type, .. } = &mut contents {
                *mime_type = Some(mime.to_string());
            }
            Ok(ReadResourceResult::new(vec![contents]).into())
        }
    }
}

/// Serve over stdio until the client disconnects.
pub async fn serve_stdio(store: Store) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_io(store, rmcp::transport::io::stdio()).await
}

/// Serve over any byte stream pair until the peer disconnects; stdio is one such pair.
pub async fn serve_io<R, W>(store: Store, io: (R, W)) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    use rmcp::ServiceExt;
    let service = MaestroServer::new(store).serve(io).await?;
    service.waiting().await?;
    Ok(())
}
