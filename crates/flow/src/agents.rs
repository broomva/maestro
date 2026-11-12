//! Specialist agents (flow kernel spec, "L3: specialist nodes"; BRO-3636): one source file per
//! specialist, `agents/<name>.agent.yaml`, rendered for each harness, and the launch spec
//! `flow next` returns for a node whose performer is `agent:<name>`.
//!
//! # The source, `agent@1`
//!
//! One YAML mapping, at most [`MAX_SOURCE`] bytes, with exactly these fields. Any other field, at
//! any level, is refused, and so is a repeated one ([`parse`]), a repeated key inside `outputs`
//! included.
//!
//! | Field | What it holds |
//! |---|---|
//! | `schema` | `"agent@1"` |
//! | `name` | lower-case letters, digits and single dashes, at most 64 bytes; the file is `<name>.agent.yaml` |
//! | `description` | when to use the agent, at most 1024 bytes: what each harness shows its main agent |
//! | `role` | the agent's instructions: the body of every render, and of the Codex brief |
//! | `posture` | the tool posture, `read_only` or `execute` (below) |
//! | `model` | `{claude, codex, gemini, grok}`: the model on each provider, each required (`inherit` where the harness takes it) |
//! | `skills` | the skills it uses, each named as the Agent Skills spec names one; may be empty |
//! | `outputs` | a JSON Schema (`type: object`) for what the driver completes the node with |
//!
//! # What it renders to
//!
//! [`render_all`] gives three agent files per source, and [`Agent::codex`] the Codex brief. Every
//! render starts with YAML frontmatter whose first line is a comment naming its source
//! ([`MARKER`]); every value is a JSON string or a list of them, which YAML reads as written. The
//! body is [`Agent::body`]: the role, the skills, the posture in words, and the output schema.
//!
//! | Harness | File | Frontmatter | Launched by |
//! |---|---|---|---|
//! | Claude Code | `.claude/agents/<name>.md` | `name`, `description`, `tools` (a comma-separated string), `model`, `skills` (when there are any) | the `Agent` tool with `subagent_type: <name>` |
//! | Gemini CLI | `.gemini/agents/<name>.md` | `name`, `description`, `kind: local`, `tools` (a list), `model` | the tool named `<name>` |
//! | Grok | `.grok/agents/<name>.md` | `name`, `description`, `tools` (a list), `model` | `spawn_subagent` with `subagent_type: <name>` |
//! | Codex | none: the brief is embedded in the driver skill and in `flow next`'s launch spec | | `spawn_agent` with `message` (the body) and `model`, and `task_name` where the tool asks for one |
//!
//! Sources, as read on 2026-11-12: Claude Code's subagents page
//! (<https://code.claude.com/docs/en/sub-agents>); Gemini CLI's `docs/core/subagents.md` and
//! `docs/tools/*.md` (0.66.0, as installed); Grok's user guide, `16-subagents.md` and its built-in
//! tools table (1.0.77, as installed); Codex's subagents page
//! (<https://learn.chatgpt.com/docs/agent-configuration/subagents>) and, for `spawn_agent`'s
//! arguments, `codex-rs/core/src/tools/handlers/multi_agents_spec.rs` on main.
//!
//! The posture is mapped to each harness's own tool names:
//!
//! | Posture | Claude Code | Gemini CLI | Grok | Codex |
//! |---|---|---|---|---|
//! | `read_only` | `Read, Grep, Glob` | `read_file`, `list_directory`, `glob`, `grep_search` | `read_file`, `grep_search`, `list_dir` | stated in the brief |
//! | `execute` | the same and `Bash` | the same and `run_shell_command` | the same and `bash` | stated in the brief |
//!
//! Gemini CLI also gets `activate_skill` when the agent names skills, since its subagents load a
//! skill through that tool; Claude Code preloads them through `skills`.
//!
//! **Not confirmed, so not relied on.** No document says Claude Code's `subagent_type` is the
//! agent's `name`; it is inferred from the docs' "identity comes only from `name`". Grok documents
//! `tools`, `disallowedTools`, `model`, `mcpServers`, `mcpInheritance` and `hooks` in an agent's
//! frontmatter and nothing about skills; its built-in agents use `prompt_mode`, `permission_mode`
//! and `agents_md`, which no page defines, so the render sets none of them. Grok also reads
//! `.claude/agents/`, and which of two same-named agents wins is not documented. Codex's
//! `spawn_agent` arguments come from its source, not its docs: `message` and `model` are in both
//! versions of the tool, `task_name` only in the second, and a subagent's sandbox is always its
//! parent's, so the posture is a sentence in the brief, never a setting. Which version a Codex
//! release exposes is not documented.
//!
//! # Drift
//!
//! [`check`] renders every source under a root and compares each render with the file on disk.
//! The first that is missing, differs or is a symbolic link, or a render ([`MARKER`] on its second
//! line) whose source is gone, is a [`Drift`] naming the file; `maestro agents render --check`
//! exits 1 on it, which is what a `bstack doctor` check will call. [`write`] makes the renders and
//! removes renders whose source is gone, and touches no other file.
//!
//! **Links.** A repository can hold a symbolic link where a render goes, so neither writes nor
//! removals follow one. Every folder from the root down to a render folder is checked to be a real
//! folder, never a link (a link, or anything not a folder, is [`AgentError::Unsafe`], and nothing
//! is written through it). Each render is written whole to a new file beside its place and renamed
//! over it, and a rename replaces a link without following it. Only regular files are taken for
//! orphans and removed. This holds against links the tree holds, not against a process changing the
//! tree while a render runs.
//!
//! # The kernel's agents
//!
//! The kernel knows the sources this crate was built with ([`builtin`]: `agents/*.agent.yaml` at the
//! repository's root, compiled in), so a definition that names `agent:<name>` without a source does
//! not compile (`unknown_agent`): an instance that could never be launched is never started, and
//! the check needs no file read, as no other compile rule does. An instance started by a binary
//! that had the agent keeps its pinned IR; `flow next` then gives that node an
//! [`LaunchSpec::UnknownAgent`] spec, never none, so a missing agent is never read as a node that
//! needs no agent.
//!
//! **A running instance takes the current build's agent.** The IR pins a performer's name
//! (`agent:<name>`), never its source: the agent a node is launched with is the source this build
//! has when `flow next` is read, even for an instance started under another (owner decision,
//! BRO-3636, round 1). The launch spec carries that source's digest (`source_sha256`), so a driver,
//! and anyone reading what it did, can see which source applied. The node's verify is the check
//! either way: an agent's instructions do not decide what the kernel accepts.
//!
//! Agent files shape behaviour; they are not a security boundary. A harness's tools list narrows
//! its built-in tools only, and every consequential check is the kernel's verify.

use crate::kernel::ir::Launch;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The source schema.
pub const SCHEMA: &str = "agent@1";

/// The most bytes a source may have.
pub const MAX_SOURCE: usize = 64 << 10;

/// The folder the sources are in, under a root.
pub const SOURCES: &str = "agents";

/// A source's file name is `<name>` and this.
pub const SUFFIX: &str = ".agent.yaml";

/// A performer naming an agent is this and the agent's name.
pub const PERFORMER: &str = "agent:";

/// Each render's second line starts with this, then the source's path.
pub const MARKER: &str = "# Generated by maestro agents render from ";

/// The longest `name`, and the longest skill name.
pub const MAX_NAME: usize = 64;

/// The longest `description`.
pub const MAX_DESCRIPTION: usize = 1024;

/// The longest model id.
pub const MAX_MODEL: usize = 128;

/// The sources compiled into the kernel: every `agents/*.agent.yaml` at the repository's root
/// (a test holds this list to that folder).
const BUILTIN: &[(&str, &str)] = &[("verifier.agent.yaml", include_str!("../../../agents/verifier.agent.yaml"))];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    schema: String,
    name: String,
    description: String,
    role: String,
    posture: Posture,
    model: Models,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(deserialize_with = "strict_json")]
    outputs: Value,
}

/// JSON read from YAML with every mapping's keys checked unique, at every level: a plain
/// `serde_json::Value` keeps the last of two equal keys and drops the other without a word.
fn strict_json<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
    d.deserialize_any(Strict)
}

struct Strict;

impl<'de> serde::de::DeserializeSeed<'de> for Strict {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(Strict)
    }
}

impl<'de> serde::de::Visitor<'de> for Strict {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(v).map(Value::Number).ok_or_else(|| E::custom(format!("{v} is not a JSON number")))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut out = Vec::new();
        while let Some(v) = seq.next_element_seed(Strict)? {
            out.push(v);
        }
        Ok(Value::Array(out))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value_seed(Strict)?;
            if out.insert(key.clone(), value).is_some() {
                return Err(serde::de::Error::custom(format!("outputs repeats the key {key:?}")));
            }
        }
        Ok(Value::Object(out))
    }
}

/// What an agent may do with its tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Posture {
    /// Read and search files; no command, no write.
    ReadOnly,
    /// Read and search files, and run shell commands; no edit tool.
    Execute,
}

impl Posture {
    fn claude(self) -> &'static [&'static str] {
        match self {
            Posture::ReadOnly => &["Read", "Grep", "Glob"],
            Posture::Execute => &["Read", "Grep", "Glob", "Bash"],
        }
    }

    fn gemini(self) -> &'static [&'static str] {
        match self {
            Posture::ReadOnly => &["read_file", "list_directory", "glob", "grep_search"],
            Posture::Execute => &["read_file", "list_directory", "glob", "grep_search", "run_shell_command"],
        }
    }

    fn grok(self) -> &'static [&'static str] {
        match self {
            Posture::ReadOnly => &["read_file", "grep_search", "list_dir"],
            Posture::Execute => &["read_file", "grep_search", "list_dir", "bash"],
        }
    }

    fn words(self) -> &'static str {
        match self {
            Posture::ReadOnly => "Read only: read and search files. Run no command and write no file.",
            Posture::Execute => {
                "Read and search files, and run shell commands. Use no edit tool: write a file only from the shell, and only where your role says."
            }
        }
    }
}

/// The model on each provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Models {
    pub claude: String,
    pub codex: String,
    pub gemini: String,
    pub grok: String,
}

/// A source that parsed. Only [`parse`] makes one.
#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub name: String,
    pub description: String,
    pub role: String,
    pub posture: Posture,
    pub model: Models,
    pub skills: Vec<String>,
    pub outputs: Value,
    /// `agents/<name>.agent.yaml`.
    pub source: String,
    /// `sha256:<hex>` of the source's bytes.
    pub source_sha256: String,
}

/// Why a source does not parse, or a folder cannot be read or written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    #[error("{file} is not a valid agent source: {detail}.")]
    Invalid { file: String, detail: String },
    #[error("{path} could not be read or written: {detail}.")]
    Io { path: String, detail: String },
    /// A render folder that is a symbolic link or not a folder: nothing is written through it.
    #[error("{path} {detail}, so no render is written or removed through it.")]
    Unsafe { path: String, detail: String },
}

impl AgentError {
    pub fn code(&self) -> &'static str {
        match self {
            AgentError::Invalid { .. } => "agent_invalid",
            AgentError::Io { .. } => "agent_io",
            AgentError::Unsafe { .. } => "render_path_unsafe",
        }
    }
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// A name as the Agent Skills spec names a skill: lower-case letters and digits in runs joined by
/// single dashes, at most [`MAX_NAME`] bytes. Every harness takes it as an agent name.
fn plain(name: &str) -> bool {
    name.len() <= MAX_NAME
        && !name.is_empty()
        && name
            .split('-')
            .all(|run| !run.is_empty() && run.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

/// Parses one source, `file` being its file name (`<name>.agent.yaml`).
pub fn parse(file: &str, text: &str) -> Result<Agent, AgentError> {
    let invalid = |detail: String| AgentError::Invalid { file: file.to_string(), detail };
    if text.len() > MAX_SOURCE {
        return Err(invalid(format!("it is {} bytes, more than {MAX_SOURCE}", text.len())));
    }
    let s: Source = serde_yaml::from_str(text).map_err(|e| invalid(e.to_string()))?;
    if s.schema != SCHEMA {
        return Err(invalid(format!("its schema is {:?}, not {SCHEMA:?}", s.schema)));
    }
    if !plain(&s.name) {
        return Err(invalid(format!(
            "the name {:?} must be lower-case letters and digits joined by single dashes, at most {MAX_NAME} bytes",
            s.name
        )));
    }
    if file != format!("{}{SUFFIX}", s.name) {
        return Err(invalid(format!("an agent named {} lives in {}{SUFFIX}", s.name, s.name)));
    }
    if s.description.trim().is_empty() || s.description.len() > MAX_DESCRIPTION {
        return Err(invalid(format!("the description must be 1 to {MAX_DESCRIPTION} bytes")));
    }
    if s.role.trim().is_empty() {
        return Err(invalid("the role is empty".into()));
    }
    let models =
        [("claude", &s.model.claude), ("codex", &s.model.codex), ("gemini", &s.model.gemini), ("grok", &s.model.grok)];
    for (provider, model) in models {
        let ok = model.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b));
        if model.is_empty() || model.len() > MAX_MODEL || !ok {
            return Err(invalid(format!(
                "the {provider} model {model:?} must be 1 to {MAX_MODEL} letters, digits and ._:/-"
            )));
        }
    }
    for (i, skill) in s.skills.iter().enumerate() {
        if !plain(skill) {
            return Err(invalid(format!("the skill {skill:?} is not a skill name")));
        }
        if s.skills[..i].contains(skill) {
            return Err(invalid(format!("the skill {skill} is named twice")));
        }
    }
    if s.outputs.get("type") != Some(&Value::from("object")) {
        return Err(invalid("outputs must be a JSON Schema of type object".into()));
    }
    Ok(Agent {
        source: format!("{SOURCES}/{file}"),
        source_sha256: sha256(text.as_bytes()),
        name: s.name,
        description: s.description,
        role: s.role,
        posture: s.posture,
        model: s.model,
        skills: s.skills,
        outputs: s.outputs,
    })
}

/// The sources this crate was built with, parsed once. A test holds them to parse.
pub fn builtin() -> Result<&'static [Agent], &'static AgentError> {
    static PARSED: OnceLock<Result<Vec<Agent>, AgentError>> = OnceLock::new();
    PARSED.get_or_init(|| BUILTIN.iter().map(|(file, text)| parse(file, text)).collect()).as_ref().map(Vec::as_slice)
}

/// The built-in agent named `name`.
pub fn find(name: &str) -> Option<&'static Agent> {
    builtin().ok()?.iter().find(|a| a.name == name)
}

/// The agent a performer names: `agent:<name>` gives `<name>`, anything else nothing.
pub fn performer_agent(performer: &str) -> Option<&str> {
    performer.strip_prefix(PERFORMER)
}

/// One harness's agent file, its path relative to the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Render {
    pub path: String,
    pub text: String,
}

/// The folders the renders go in, one per harness, under a root.
pub const RENDER_DIRS: [&str; 3] = [".claude/agents", ".gemini/agents", ".grok/agents"];

/// A value YAML reads as written: a JSON string.
fn quoted(s: &str) -> String {
    Value::from(s).to_string()
}

fn quoted_list(items: &[&str]) -> String {
    let items: Vec<String> = items.iter().map(|i| quoted(i)).collect();
    format!("[{}]", items.join(", "))
}

impl Agent {
    /// What every render and the Codex brief say: the role, the skills, the posture and the output
    /// schema.
    pub fn body(&self) -> String {
        let mut out = self.role.trim_end().to_string();
        out.push_str("\n\n## Skills\n\n");
        if self.skills.is_empty() {
            out.push_str("None.\n");
        } else {
            let names: Vec<String> = self.skills.iter().map(|s| format!("`{s}`")).collect();
            out.push_str(&format!("Use these skills: {}.\n", names.join(", ")));
        }
        out.push_str("\n## Tools\n\n");
        out.push_str(self.posture.words());
        out.push_str("\n\n## What you return\n\nReturn exactly one JSON object that matches this JSON Schema, and nothing else. The driver completes the flow node with it.\n\n```json\n");
        out.push_str(&serde_json::to_string_pretty(&self.outputs).unwrap_or_default());
        out.push_str("\n```\n");
        out
    }

    fn frontmatter(&self, fields: &[(&str, String)]) -> String {
        let mut out = format!("---\n{MARKER}{}. Edit the source, then render again.\n", self.source);
        for (key, value) in fields {
            out.push_str(&format!("{key}: {value}\n"));
        }
        out.push_str("---\n\n");
        out.push_str(&self.body());
        out
    }

    /// The Claude Code, Gemini CLI and Grok agent files, in that order.
    pub fn renders(&self) -> Vec<Render> {
        let (name, description) = (quoted(&self.name), quoted(&self.description));
        let mut claude = vec![
            ("name", name.clone()),
            ("description", description.clone()),
            ("tools", quoted(&self.posture.claude().join(", "))),
            ("model", quoted(&self.model.claude)),
        ];
        if !self.skills.is_empty() {
            let skills: Vec<&str> = self.skills.iter().map(String::as_str).collect();
            claude.push(("skills", quoted_list(&skills)));
        }
        let mut gemini_tools = self.posture.gemini().to_vec();
        if !self.skills.is_empty() {
            gemini_tools.push("activate_skill");
        }
        let gemini = [
            ("name", name.clone()),
            ("description", description.clone()),
            ("kind", quoted("local")),
            ("tools", quoted_list(&gemini_tools)),
            ("model", quoted(&self.model.gemini)),
        ];
        let grok = [
            ("name", name),
            ("description", description),
            ("tools", quoted_list(self.posture.grok())),
            ("model", quoted(&self.model.grok)),
        ];
        let [c, g, x] = RENDER_DIRS;
        vec![
            Render { path: format!("{c}/{}.md", self.name), text: self.frontmatter(&claude) },
            Render { path: format!("{g}/{}.md", self.name), text: self.frontmatter(&gemini) },
            Render { path: format!("{x}/{}.md", self.name), text: self.frontmatter(&grok) },
        ]
    }

    /// The canonical Codex `spawn_agent` brief.
    pub fn codex(&self) -> CodexBrief {
        CodexBrief {
            tool: "spawn_agent".into(),
            task_name: self.name.replace('-', "_"),
            model: self.model.codex.clone(),
            message: self.body(),
        }
    }

    /// The launch spec for a node this agent performs with `skill`.
    pub fn launch(&self, skill: Option<&str>) -> AgentLaunch {
        let [c, g, x] = RENDER_DIRS;
        let file = |dir: &str| format!("{dir}/{}.md", self.name);
        AgentLaunch {
            agent: self.name.clone(),
            skill: skill.map(str::to_string),
            source: self.source.clone(),
            source_sha256: self.source_sha256.clone(),
            outputs: self.outputs.clone(),
            claude: NativeLaunch { file: file(c), tool: "Agent".into(), subagent_type: Some(self.name.clone()) },
            gemini: NativeLaunch { file: file(g), tool: self.name.clone(), subagent_type: None },
            grok: NativeLaunch { file: file(x), tool: "spawn_subagent".into(), subagent_type: Some(self.name.clone()) },
            codex: self.codex(),
        }
    }
}

/// The arguments of Codex's `spawn_agent` for one agent: `message` (the brief, to which the driver
/// appends the node's task) and `model`, and `task_name` where the tool asks for one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodexBrief {
    pub tool: String,
    pub task_name: String,
    pub model: String,
    pub message: String,
}

/// How a harness with an agent file launches it: the file, the tool to call and, where the tool
/// takes one, the agent's type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeLaunch {
    pub file: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
}

/// What `flow next` returns for an enabled node whose performer is `agent:<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentLaunch {
    pub agent: String,
    /// The node's own skill.
    pub skill: Option<String>,
    pub source: String,
    pub source_sha256: String,
    /// The JSON Schema of the outputs the node completes with.
    pub outputs: Value,
    pub claude: NativeLaunch,
    pub gemini: NativeLaunch,
    pub grok: NativeLaunch,
    pub codex: CodexBrief,
}

/// A node's launch spec, by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchSpec {
    Agent(Box<AgentLaunch>),
    /// The pinned definition names an agent this binary has no source for.
    UnknownAgent {
        agent: String,
        reason: String,
    },
}

/// The launch spec of a node with this launch hint: `None` unless its performer is `agent:<name>`.
pub fn launch_spec(launch: &Launch) -> Option<LaunchSpec> {
    let name = performer_agent(launch.performer.as_deref()?)?;
    Some(match find(name) {
        Some(agent) => LaunchSpec::Agent(Box::new(agent.launch(launch.skill.as_deref()))),
        None => LaunchSpec::UnknownAgent {
            agent: name.to_string(),
            reason: format!("this maestro has no agent source {SOURCES}/{name}{SUFFIX}"),
        },
    })
}

fn io(path: &Path, e: impl std::fmt::Display) -> AgentError {
    AgentError::Io { path: path.display().to_string(), detail: e.to_string() }
}

/// A file's bytes, read to one byte past `limit`; `None` when there is no file.
fn read(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, AgentError> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(path, e)),
    };
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes).map_err(|e| io(path, e))?;
    Ok(Some(bytes))
}

/// Every source under `root` (`root/agents/*.agent.yaml`), by file name. A root with no
/// `agents` folder is an error, never an empty set.
pub fn load(root: &Path) -> Result<Vec<Agent>, AgentError> {
    let dir = root.join(SOURCES);
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| io(&dir, e))? {
        let name = entry.map_err(|e| io(&dir, e))?.file_name().to_string_lossy().into_owned();
        if name.ends_with(SUFFIX) {
            files.push(name);
        }
    }
    files.sort();
    let mut agents = Vec::new();
    for file in files {
        let path = dir.join(&file);
        let bytes = read(&path, MAX_SOURCE)?.ok_or_else(|| io(&path, "it went away"))?;
        let text = String::from_utf8(bytes).map_err(|e| io(&path, e))?;
        agents.push(parse(&file, &text)?);
    }
    Ok(agents)
}

/// Every render of `agents`, by agent, then harness.
pub fn render_all(agents: &[Agent]) -> Vec<Render> {
    agents.iter().flat_map(Agent::renders).collect()
}

/// How a render drifted from its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftKind {
    /// No file where the render goes.
    Missing,
    /// The file is not what the source renders to.
    Differs,
    /// A symbolic link where the render goes, wherever it points: [`write`] replaces it.
    Link,
    /// A render whose source is gone.
    Orphan,
}

impl DriftKind {
    fn words(self) -> &'static str {
        match self {
            DriftKind::Missing => "it is missing",
            DriftKind::Differs => "it is not what its source renders to",
            DriftKind::Link => "it is a symbolic link",
            DriftKind::Orphan => "its source is gone",
        }
    }
}

/// The first render that does not match its source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{file} drifts from its source, since {}: run maestro agents render", kind.words())]
pub struct Drift {
    /// The render's path, relative to the root.
    pub file: String,
    pub kind: DriftKind,
}

/// The render folder `rel` under `root`, every folder below the root checked to be a real
/// folder, never a symbolic link (`symlink_metadata`), so no render or removal is redirected out
/// of the root by a link the repository holds. The root is the caller's choice and may be reached
/// through one. A missing folder is made when `create`, and is otherwise `None`.
fn render_dir(root: &Path, rel: &str, create: bool) -> Result<Option<PathBuf>, AgentError> {
    if !std::fs::metadata(root).map_err(|e| io(root, e))?.is_dir() {
        return Err(unsafe_path(root, "is not a folder"));
    }
    let mut dir = root.to_path_buf();
    for part in Path::new(rel).components() {
        dir.push(part);
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.file_type().is_symlink() => return Err(unsafe_path(&dir, "is a symbolic link")),
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(unsafe_path(&dir, "is not a folder")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
                // `create_dir` makes nothing where anything, a link included, already is.
                std::fs::create_dir(&dir).map_err(|e| io(&dir, e))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(&dir, e)),
        }
    }
    Ok(Some(dir))
}

fn unsafe_path(path: &Path, what: &str) -> AgentError {
    AgentError::Unsafe { path: path.display().to_string(), detail: what.to_string() }
}

/// A render's folder (one of [`RENDER_DIRS`]) and file name.
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// Puts `bytes` at `dir/file`: written whole to a new file beside it, then renamed over it. The
/// rename replaces whatever is at `dir/file`, a symbolic link included, and never follows one.
fn replace(dir: &Path, file: &str, bytes: &[u8]) -> Result<(), AgentError> {
    use std::io::Write as _;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!(".{file}.{}-{nanos}.tmp", std::process::id()));
    let dest = dir.join(file);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, &dest));
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(&dest, e)
    })
}

/// The renders under `root` whose source is gone: `*.md` regular files (never a link) in a
/// verified render folder, whose second line is [`MARKER`] and whose name no source has. Only such
/// files: a hand-written agent, and anything that is not a regular file, is left alone.
fn orphans(root: &Path, agents: &[Agent]) -> Result<Vec<String>, AgentError> {
    let mut out = Vec::new();
    for dir in RENDER_DIRS {
        let Some(path) = render_dir(root, dir, false)? else { continue };
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&path).map_err(|e| io(&path, e))? {
            names.push(entry.map_err(|e| io(&path, e))?.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        for name in names {
            let Some(stem) = name.strip_suffix(".md") else { continue };
            if agents.iter().any(|a| a.name == stem) {
                continue;
            }
            let file = path.join(&name);
            if !std::fs::symlink_metadata(&file).map_err(|e| io(&file, e))?.is_file() {
                continue;
            }
            let generated = read(&file, MAX_SOURCE * 2)?.is_some_and(|text| {
                text.split(|b| *b == b'\n').nth(1).is_some_and(|line| line.starts_with(MARKER.as_bytes()))
            });
            if generated {
                out.push(format!("{dir}/{name}"));
            }
        }
    }
    Ok(out)
}

/// The first render under `root` that drifts from its source, in [`render_all`]'s order, then
/// the orphans; `None` when every render matches. A render folder that is a link is an error
/// ([`AgentError::Unsafe`]); a render that is a link is drift, however its target reads.
pub fn check(root: &Path) -> Result<Option<Drift>, AgentError> {
    let agents = load(root)?;
    for r in render_all(&agents) {
        let (dir, file) = split(&r.path);
        let kind = match render_dir(root, dir, false)? {
            None => DriftKind::Missing,
            Some(dir) => {
                let path = dir.join(file);
                match std::fs::symlink_metadata(&path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => DriftKind::Missing,
                    Err(e) => return Err(io(&path, e)),
                    Ok(m) if m.file_type().is_symlink() => DriftKind::Link,
                    Ok(m) if !m.is_file() => DriftKind::Differs,
                    Ok(_) => match read(&path, r.text.len())? {
                        None => DriftKind::Missing,
                        Some(bytes) if bytes != r.text.as_bytes() => DriftKind::Differs,
                        Some(_) => continue,
                    },
                }
            }
        };
        return Ok(Some(Drift { file: r.path, kind }));
    }
    Ok(orphans(root, &agents)?.into_iter().next().map(|file| Drift { file, kind: DriftKind::Orphan }))
}

/// What [`write`] did, each path relative to the root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Written {
    pub written: Vec<String>,
    pub removed: Vec<String>,
}

/// Writes every render under `root`, each through [`replace`] into a verified folder
/// ([`render_dir`]), and removes the renders whose source is gone (regular files only).
pub fn write(root: &Path) -> Result<Written, AgentError> {
    let agents = load(root)?;
    let mut written = Vec::new();
    for r in render_all(&agents) {
        let (dir, file) = split(&r.path);
        let dir = render_dir(root, dir, true)?.ok_or_else(|| unsafe_path(&root.join(dir), "was not made"))?;
        replace(&dir, file, r.text.as_bytes())?;
        written.push(r.path);
    }
    let removed = orphans(root, &agents)?;
    for file in &removed {
        let path = root.join(file);
        std::fs::remove_file(&path).map_err(|e| io(&path, e))?;
    }
    Ok(Written { written, removed })
}
