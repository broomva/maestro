//! Maestro over MCP: the same reads as the CLI, plus context as prompts and resources.
//!
//! Every tool here has [`Authority::Read`]. Owner actions are not exposed: a
//! model-visible tool is not proof of the owner, and the BRO-3186 contract
//! forbids a second writer to the ask and work ledgers. When an authenticated
//! owner path exists, owner verbs join this surface behind it.

use maestro_core::ask::AskState;
use maestro_core::authority::Authority;
use maestro_core::context::ContextScope;
use maestro_core::work::WorkState;
use maestro_store::{Store, views};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    AnnotateAble, CallToolRequestParams, CallToolResult, Content, GetPromptRequestParams, GetPromptResult,
    Implementation, ListPromptsResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams, Prompt,
    PromptArgument, PromptMessage, PromptMessageRole, RawResource, ReadResourceRequestParams, ReadResourceResult,
    ResourceContents, ServerCapabilities, ServerInfo, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::future::Future;
use std::sync::Arc;

/// One tool: its name, what it may do, and its argument type.
pub struct ToolSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub authority: Authority,
    schema: fn() -> Value,
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

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ContextArgs {
    /// The session's working directory; project and initiative are inferred from it.
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub initiative: Option<String>,
    /// Maximum holds listed (default 8).
    pub max_needs: Option<usize>,
}

pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "maestro_snapshot",
        title: "Snapshot",
        description: "Counts across the control plane: needs, asks, work by state, loops with drift, live initiatives, fleet. Every section names its source and any read errors; a section with data null was not readable, which is not the same as empty.",
        authority: Authority::Read,
        schema: schema_of::<NoArgs>,
    },
    ToolSpec {
        name: "maestro_needs_list",
        title: "Needs you",
        description: "Holds on the owner: open blocking asks, and queued work only the owner may start. Work paused on a permission prompt is live surface state and joins when a surface adapter reports it. This list is the Needs you count; nothing else is. data null means the asks could not be read: the count is unknown, not zero.",
        authority: Authority::Read,
        schema: schema_of::<NeedsArgs>,
    },
    ToolSpec {
        name: "maestro_asks_list",
        title: "Asks",
        description: "Asks from .control/asks, filtered by state, project and blocking. Read before filing a new ask so you never duplicate one.",
        authority: Authority::Read,
        schema: schema_of::<AsksArgs>,
    },
    ToolSpec {
        name: "maestro_ask_get",
        title: "Ask",
        description: "One ask in full, with its entry hash: the Paseo plugin's entryHash over the raw entry, which its answer path rechecks. Any change to the entry, an answer included, changes the hash.",
        authority: Authority::Read,
        schema: schema_of::<AskGetArgs>,
    },
    ToolSpec {
        name: "maestro_work_list",
        title: "Work",
        description: "Work items with their canonical state (Queued, Running, Stuck, Needs you, Done, Standing), labelled as the Paseo plugin labels them. Work at the gate is labelled Needs you with finished: true; it is not a need unless it appears in maestro_needs_list.",
        authority: Authority::Read,
        schema: schema_of::<WorkArgs>,
    },
    ToolSpec {
        name: "maestro_loops_get",
        title: "Loops",
        description: "Standing loops from .broomva/loops joined with their tick receipts. drift holds plugin drift classes the ticks ledger proves (tick_unclosed); notices holds Maestro's own signals (review_overdue, no_tick_receipts). Substrate drift needs surface adapters and is not claimed. Ticks are read from the ledger's tail, as the plugin reads it (its STREAM_TAIL: the last 2,097,152 UTF-16 units, from the first whole line); ticks_recorded counts those.",
        authority: Authority::Read,
        schema: schema_of::<LoopsArgs>,
    },
    ToolSpec {
        name: "maestro_initiatives_list",
        title: "Initiatives",
        description: "Initiatives from .control/initiatives: intent, status, driver and next action. Live ones by default.",
        authority: Authority::Read,
        schema: schema_of::<InitiativesArgs>,
    },
    ToolSpec {
        name: "maestro_fleet_get",
        title: "Fleet",
        description: "The latest fleet-reconcile report per scope, condensed. The fleet skill acts; Maestro only reads.",
        authority: Authority::Read,
        schema: schema_of::<NoArgs>,
    },
    ToolSpec {
        name: "maestro_context",
        title: "Context",
        description: "The governance context for a session: holds in its scope, its initiative, loops needing attention, unread sources and standing rules, as markdown plus structured data.",
        authority: Authority::Read,
        schema: schema_of::<ContextArgs>,
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
            let file_at = format!("{}/{}", maestro_store::ASKS_DIR, a.ledger);
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
        "maestro_context" => {
            let a: ContextArgs = parse(args)?;
            let packet =
                views::context(store, ContextScope { cwd: a.cwd, project: a.project, initiative: a.initiative });
            let markdown = packet.render_markdown(a.max_needs.unwrap_or(8));
            Ok(json!({ "markdown": markdown, "packet": packet }))
        }
        other => Err(McpError::invalid_params(format!("unknown tool `{other}`"), None)),
    }
}

pub const RESOURCES: &[(&str, &str, &str)] = &[
    ("maestro://snapshot", "snapshot", "Control-plane snapshot (JSON)"),
    ("maestro://needs", "needs", "Holds on the owner (JSON)"),
    ("maestro://context", "context", "Governance context for this server's workspace (markdown)"),
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
                Tool::new(spec.name, spec.description, Arc::new(object)).with_title(spec.title).with_annotations(
                    ToolAnnotations::with_title(spec.title)
                        .read_only(spec.authority == Authority::Read)
                        .destructive(false)
                        .idempotent(true)
                        .open_world(false),
                )
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
            other => Err(McpError::resource_not_found(format!("no resource {other}"), None)),
        }
    }
}

fn prompt_arg(name: &str, description: &str) -> PromptArgument {
    let mut arg = PromptArgument::default();
    arg.name = name.to_string();
    arg.description = Some(description.to_string());
    arg.required = Some(false);
    arg
}

impl ServerHandler for MaestroServer {
    fn get_info(&self) -> ServerInfo {
        let mut implementation = Implementation::new("maestro", env!("CARGO_PKG_VERSION"));
        implementation.title = Some("Maestro".to_string());
        implementation.description = Some("Governance and orchestration metalayer for agent fleets".to_string());
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().enable_prompts().enable_resources().build();
        info.server_info = implementation;
        info.instructions = Some(
            "Maestro reports the governance state of the workspace: what needs the owner, open asks, \
             work states, loop health, initiatives and the fleet. Call maestro_context at the start of \
             substantive work and before filing an ask. All tools are read-only; answering asks, \
             approving work and dispatch are owner actions taken in the owner's app."
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
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
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
                    if let Some(md) = value.get("markdown").and_then(|m| m.as_str()) {
                        let mut result = CallToolResult::structured(value.clone());
                        result.content = vec![Content::text(md.to_string())];
                        Ok(result)
                    } else {
                        Ok(CallToolResult::structured(value))
                    }
                }
                Err(e) if e.code == rmcp::model::ErrorCode::INVALID_PARAMS => {
                    Ok(CallToolResult::error(vec![Content::text(e.message.to_string())]))
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
    ) -> impl Future<Output = Result<GetPromptResult, McpError>> + Send + '_ {
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
            let mut result =
                GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, packet.render_markdown(8))]);
            result.description = Some("Maestro governance context".to_string());
            Ok(result)
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
                let mut raw = RawResource::new(*uri, *name);
                raw.description = Some(description.to_string());
                raw.no_annotation()
            })
            .collect();
        std::future::ready(Ok(ListResourcesResult::with_all_items(resources)))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResult, McpError>> + Send + '_ {
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
            Ok(ReadResourceResult::new(vec![contents]))
        }
    }
}

/// Serve over stdio until the client disconnects.
pub async fn serve_stdio(store: Store) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use rmcp::ServiceExt;
    let service = MaestroServer::new(store).serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
