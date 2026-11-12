//! Specialist agents (flow kernel spec, "L3: specialist nodes"; BRO-3636): one source file per
//! specialist, `agents/<name>.agent.yaml`, rendered for each harness, and the launch spec
//! `flow next` returns for a node whose performer is `agent:<name>`.
//!
//! # The source, `agent@1`
//!
//! One YAML mapping, at most [`MAX_SOURCE`] bytes, with exactly these fields. Any other field of
//! the source, at the top and in `model`, is refused, and so is a repeated one ([`parse`]).
//! `outputs` is held to less: its keys are unique at every level and its `type` is `object`, but
//! its keywords are not validated as JSON Schema (the crate has no schema validator), so an
//! unknown keyword inside it is kept as written.
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
//! | Gemini CLI | `.gemini/agents/<name>.md` | `name`, `description`, `kind: local`, `tools` (a list), `model` | `invoke_agent` with `agent_name: <name>` (and `prompt`) |
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
//! **Gemini CLI: its docs and its runtime disagree.** The `docs/core/subagents.md` bundled with
//! Gemini CLI 0.66.0 says each agent is a tool named after it. The 0.66.0 runtime registers one
//! tool for every subagent instead, `invoke_agent` (`AGENT_TOOL_NAME` in the bundle's
//! `chunk-*.js`, the `AgentTool` class), whose required arguments are `agent_name` and `prompt`,
//! and registers no tool per agent. The launch spec follows the runtime ([`GEMINI_AGENT_TOOL`]);
//! this was read from the bundle on the nuc, not from a live run.
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
//! **Links.** A repository can hold a symbolic link where a render goes, so no render, read or
//! removal under the root follows one. The root is the caller's choice and may be reached through
//! a link. Below it, each render folder is opened one component at a time from the folder above
//! (`openat` with `O_NOFOLLOW`), so a link, or anything not a folder, on the way is
//! [`AgentError::Unsafe`] and nothing is written. Everything after that goes through the open
//! folder's descriptor, never its path: a render is written to a new file made there
//! (`O_CREAT | O_EXCL | O_NOFOLLOW`) and renamed over its place in the same folder (`renameat`,
//! which replaces a link and follows none); a render or an orphan is looked at with `statat`
//! (`AT_SYMLINK_NOFOLLOW`) and read with `O_NOFOLLOW`; an orphan is removed with `unlinkat`
//! through the descriptor it was found by, and only if it is still a regular file just before.
//! So a folder moved, or swapped for a link, after it is opened redirects nothing: the write or
//! removal lands in the folder that was opened, wherever that folder has since been moved, and
//! never in a link's target. What is not guaranteed: a process that moves the opened folder itself
//! out of the root before the write still receives the render in it, and the sources under
//! `agents/` are read by path.
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
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read as _;
use std::os::fd::OwnedFd;
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

/// The tool Gemini CLI launches a subagent with, taking `agent_name` and `prompt`: its runtime's
/// `AGENT_TOOL_NAME` (0.66.0), not the tool named after the agent its bundled docs describe.
pub const GEMINI_AGENT_TOOL: &str = "invoke_agent";

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
            claude: NativeLaunch {
                file: file(c),
                tool: "Agent".into(),
                subagent_type: Some(self.name.clone()),
                agent_name: None,
            },
            // Gemini CLI 0.66.0's runtime, not its bundled docs (see the module docs).
            gemini: NativeLaunch {
                file: file(g),
                tool: GEMINI_AGENT_TOOL.into(),
                subagent_type: None,
                agent_name: Some(self.name.clone()),
            },
            grok: NativeLaunch {
                file: file(x),
                tool: "spawn_subagent".into(),
                subagent_type: Some(self.name.clone()),
                agent_name: None,
            },
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

/// How a harness with an agent file launches it: the file, the tool to call, and the argument
/// that names the agent in that tool (`subagent_type` for Claude Code and Grok, `agent_name` for
/// Gemini CLI).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeLaunch {
    pub file: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
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

/// Opens a render folder's components without following a link.
const WALK: OFlags = OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);

/// Opens a render, or a file taken for an orphan: never through a link, never blocking on a FIFO.
const READ: OFlags = OFlags::RDONLY.union(OFlags::NOFOLLOW).union(OFlags::NONBLOCK).union(OFlags::CLOEXEC);

/// A render folder, open. Every check, read, write, rename and removal under it goes through this
/// descriptor (`statat`, `openat`, `renameat`, `unlinkat`), never through its path, which is kept
/// for messages only: once it is open, moving the folder or putting a link where it was changes
/// nothing about where a render goes.
struct Folder {
    fd: OwnedFd,
    path: PathBuf,
}

impl Folder {
    fn io(&self, name: &str, e: impl std::fmt::Display) -> AgentError {
        io(&self.path.join(name), e)
    }

    /// What `name` is, without following it; `None` when nothing is there.
    fn kind(&self, name: &str) -> Result<Option<FileType>, AgentError> {
        match rustix::fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => Ok(Some(FileType::from_raw_mode(st.st_mode as _))),
            Err(Errno::NOENT) => Ok(None),
            Err(e) => Err(self.io(name, e)),
        }
    }

    /// The bytes of the regular file `name`, read to one byte past `limit`; `None` when it is not
    /// there or not a regular file (a link, a FIFO, a folder).
    fn read(&self, name: &str, limit: usize) -> Result<Option<Vec<u8>>, AgentError> {
        let fd = match rustix::fs::openat(&self.fd, name, READ, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR | Errno::NXIO) => return Ok(None),
            Err(e) => return Err(self.io(name, e)),
        };
        let st = rustix::fs::fstat(&fd).map_err(|e| self.io(name, e))?;
        if FileType::from_raw_mode(st.st_mode as _) != FileType::RegularFile {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        std::fs::File::from(fd).take(limit as u64 + 1).read_to_end(&mut bytes).map_err(|e| self.io(name, e))?;
        Ok(Some(bytes))
    }

    /// A new file `name`, made here and never through a link: it fails if anything is there.
    fn create_new(&self, name: &str) -> Result<std::fs::File, AgentError> {
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(&self.fd, name, flags, Mode::from_raw_mode(0o644)).map_err(|e| self.io(name, e))?;
        Ok(fd.into())
    }

    /// Puts `bytes` at `name`: written whole to a new file here, then renamed over `name` in this
    /// same folder. The rename replaces whatever is at `name`, a link included, and follows none.
    fn replace(&self, name: &str, bytes: &[u8]) -> Result<(), AgentError> {
        use std::io::Write as _;
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let tmp = format!(".{name}.{}-{nanos}.tmp", std::process::id());
        let mut file = self.create_new(&tmp)?;
        let done =
            file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|e| self.io(&tmp, e)).and_then(|()| {
                rustix::fs::renameat(&self.fd, tmp.as_str(), &self.fd, name).map_err(|e| self.io(name, e))
            });
        if done.is_err() {
            let _ = rustix::fs::unlinkat(&self.fd, tmp.as_str(), AtFlags::empty());
        }
        done
    }

    /// Removes `name` if it is still a regular file, through this descriptor; `false` when it is
    /// not (a link put there since, say), and then it is left alone.
    fn remove_regular(&self, name: &str) -> Result<bool, AgentError> {
        if self.kind(name)? != Some(FileType::RegularFile) {
            return Ok(false);
        }
        rustix::fs::unlinkat(&self.fd, name, AtFlags::empty()).map_err(|e| self.io(name, e))?;
        Ok(true)
    }

    /// The names here, sorted, read through this descriptor.
    fn names(&self) -> Result<Vec<String>, AgentError> {
        let mut out = Vec::new();
        let entries = rustix::fs::Dir::read_from(&self.fd).map_err(|e| io(&self.path, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| io(&self.path, e))?;
            if let Ok(name) = entry.file_name().to_str()
                && name != "."
                && name != ".."
            {
                out.push(name.to_string());
            }
        }
        out.sort();
        Ok(out)
    }
}

/// The render folder `rel` under `root`, open ([`Folder`]). The root is the caller's choice and
/// may be reached through a link; below it, each folder is opened from the one above with
/// `O_NOFOLLOW`, so a link, or anything that is not a folder, is refused
/// ([`AgentError::Unsafe`]). A missing folder is made with `mkdirat` when `create`, and is
/// otherwise `None`.
fn render_dir(root: &Path, rel: &str, create: bool) -> Result<Option<Folder>, AgentError> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut fd = rustix::fs::open(root, flags, Mode::empty()).map_err(|e| match e {
        Errno::NOTDIR => unsafe_path(root, "is not a folder"),
        e => io(root, e),
    })?;
    let mut path = root.to_path_buf();
    for part in rel.split('/') {
        path.push(part);
        let opened = match rustix::fs::openat(&fd, part, WALK, Mode::empty()) {
            Err(Errno::NOENT) if create => {
                match rustix::fs::mkdirat(&fd, part, Mode::from_raw_mode(0o755)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(e) => return Err(io(&path, e)),
                }
                rustix::fs::openat(&fd, part, WALK, Mode::empty())
            }
            other => other,
        };
        fd = match opened {
            Ok(next) => next,
            Err(Errno::NOENT) => return Ok(None),
            // Linux says `ELOOP` and macOS `ENOTDIR` for a link opened this way: ask the entry.
            Err(Errno::LOOP | Errno::NOTDIR) => {
                let link = rustix::fs::statat(&fd, part, AtFlags::SYMLINK_NOFOLLOW)
                    .is_ok_and(|st| FileType::from_raw_mode(st.st_mode as _) == FileType::Symlink);
                return Err(unsafe_path(&path, if link { "is a symbolic link" } else { "is not a folder" }));
            }
            Err(e) => return Err(io(&path, e)),
        };
    }
    Ok(Some(Folder { fd, path }))
}

fn unsafe_path(path: &Path, what: &str) -> AgentError {
    AgentError::Unsafe { path: path.display().to_string(), detail: what.to_string() }
}

/// A render's folder (one of [`RENDER_DIRS`]) and file name.
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// The renders in one render folder whose source is gone, and the folder they were found in.
struct Orphans {
    /// The folder, relative to the root.
    dir: &'static str,
    folder: Folder,
    names: Vec<String>,
}

/// The renders under `root` whose source is gone: in each render folder that exists, the `*.md`
/// regular files (never a link) whose second line is [`MARKER`] and whose name no source has, each
/// found and read through the folder's descriptor. Only such files: a hand-written agent, and
/// anything that is not a regular file, is left alone.
fn orphans(root: &Path, agents: &[Agent]) -> Result<Vec<Orphans>, AgentError> {
    let mut out = Vec::new();
    for dir in RENDER_DIRS {
        let Some(folder) = render_dir(root, dir, false)? else { continue };
        let mut names = Vec::new();
        for name in folder.names()? {
            let Some(stem) = name.strip_suffix(".md") else { continue };
            if agents.iter().any(|a| a.name == stem) {
                continue;
            }
            let generated = folder.read(&name, MAX_SOURCE * 2)?.is_some_and(|text| {
                text.split(|b| *b == b'\n').nth(1).is_some_and(|line| line.starts_with(MARKER.as_bytes()))
            });
            if generated {
                names.push(name);
            }
        }
        out.push(Orphans { dir, folder, names });
    }
    Ok(out)
}

/// Removes the orphans [`orphans`] found, each through the descriptor of the folder it was found
/// in, and only while it is still a regular file; the paths removed, relative to the root.
fn remove(found: Vec<Orphans>) -> Result<Vec<String>, AgentError> {
    let mut removed = Vec::new();
    for o in found {
        for name in o.names {
            if o.folder.remove_regular(&name)? {
                removed.push(format!("{}/{name}", o.dir));
            }
        }
    }
    Ok(removed)
}

/// The first render under `root` that drifts from its source, in [`render_all`]'s order, then
/// the orphans; `None` when every render matches. Each render is looked at and read through its
/// folder's descriptor. A render folder that is a link is an error ([`AgentError::Unsafe`]); a
/// render that is a link is drift, however its target reads.
pub fn check(root: &Path) -> Result<Option<Drift>, AgentError> {
    let agents = load(root)?;
    for r in render_all(&agents) {
        let (dir, file) = split(&r.path);
        let kind = match render_dir(root, dir, false)? {
            None => DriftKind::Missing,
            Some(folder) => match folder.kind(file)? {
                None => DriftKind::Missing,
                Some(FileType::Symlink) => DriftKind::Link,
                Some(FileType::RegularFile) => match folder.read(file, r.text.len())? {
                    None => DriftKind::Missing,
                    Some(bytes) if bytes != r.text.as_bytes() => DriftKind::Differs,
                    Some(_) => continue,
                },
                Some(_) => DriftKind::Differs,
            },
        };
        return Ok(Some(Drift { file: r.path, kind }));
    }
    let first = orphans(root, &agents)?.into_iter().find_map(|o| o.names.first().map(|n| format!("{}/{n}", o.dir)));
    Ok(first.map(|file| Drift { file, kind: DriftKind::Orphan }))
}

/// What [`write`] did, each path relative to the root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Written {
    pub written: Vec<String>,
    pub removed: Vec<String>,
}

/// Writes every render under `root`, each into its folder opened by [`render_dir`] and through
/// that descriptor ([`Folder::replace`]), and removes the renders whose source is gone (regular
/// files only, through the descriptor they were found by).
pub fn write(root: &Path) -> Result<Written, AgentError> {
    let agents = load(root)?;
    let mut written = Vec::new();
    for r in render_all(&agents) {
        let (dir, file) = split(&r.path);
        let folder = render_dir(root, dir, true)?.ok_or_else(|| unsafe_path(&root.join(dir), "was not made"))?;
        folder.replace(file, r.text.as_bytes())?;
        written.push(r.path);
    }
    let removed = remove(orphans(root, &agents)?)?;
    Ok(Written { written, removed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const VERIFIER: &str = include_str!("../../../agents/verifier.agent.yaml");

    /// A root holding the verifier's source, and a folder outside it.
    fn root() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(root.join("agents")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("agents/verifier.agent.yaml"), VERIFIER).unwrap();
        (tmp, root, outside)
    }

    #[test]
    fn a_folder_swapped_for_a_link_after_it_is_opened_redirects_no_render() {
        let (_tmp, root, outside) = root();
        let folder = render_dir(&root, ".claude/agents", true).unwrap().unwrap();
        // The checked folder moves away and a link to a folder outside takes its place.
        std::fs::rename(root.join(".claude/agents"), root.join(".claude/moved")).unwrap();
        symlink(&outside, root.join(".claude/agents")).unwrap();
        folder.replace("verifier.md", b"render\n").unwrap();
        // The render lands in the real folder that was opened, now `.claude/moved`, never outside.
        assert!(!outside.join("verifier.md").exists(), "a render went through the link");
        assert_eq!(std::fs::read(root.join(".claude/moved/verifier.md")).unwrap(), b"render\n");
        let leftovers: Vec<_> = std::fs::read_dir(root.join(".claude/moved")).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temporary file is left");
        // A write that opens the folder afresh finds the link and refuses it.
        assert_eq!(write(&root).unwrap_err().code(), "render_path_unsafe");
        assert!(!outside.join("verifier.md").exists());
    }

    #[test]
    fn an_orphan_folder_swapped_for_a_link_after_the_scan_removes_nothing_outside() {
        let (_tmp, root, outside) = root();
        let checker = VERIFIER.replace("name: verifier", "name: checker");
        std::fs::write(root.join("agents/checker.agent.yaml"), &checker).unwrap();
        write(&root).unwrap();
        std::fs::remove_file(root.join("agents/checker.agent.yaml")).unwrap();
        let agents = load(&root).unwrap();
        let found = orphans(&root, &agents).unwrap();
        assert_eq!(found.iter().map(|o| o.names.len()).sum::<usize>(), 3);
        // After the scan, the folder moves away and a link to a folder outside takes its place,
        // where a file of the same name waits.
        std::fs::rename(root.join(".claude/agents"), root.join(".claude/moved")).unwrap();
        symlink(&outside, root.join(".claude/agents")).unwrap();
        std::fs::write(outside.join("checker.md"), "outside, keep me\n").unwrap();
        // And in another folder, the orphan itself is replaced by a link after the scan.
        let target = outside.join("victim.md");
        std::fs::write(&target, "keep me too\n").unwrap();
        std::fs::remove_file(root.join(".gemini/agents/checker.md")).unwrap();
        symlink(&target, root.join(".gemini/agents/checker.md")).unwrap();

        let removed = remove(found).unwrap();
        assert_eq!(removed, [".claude/agents/checker.md", ".grok/agents/checker.md"]);
        assert_eq!(std::fs::read_to_string(outside.join("checker.md")).unwrap(), "outside, keep me\n");
        assert!(!root.join(".claude/moved/checker.md").exists(), "the orphan scanned is the one removed");
        let link = std::fs::symlink_metadata(root.join(".gemini/agents/checker.md")).unwrap();
        assert!(link.file_type().is_symlink(), "a link put in an orphan's place is left alone");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me too\n");
    }

    #[test]
    fn a_temporary_file_is_made_new_never_through_a_link() {
        let (_tmp, root, outside) = root();
        let folder = render_dir(&root, ".claude/agents", true).unwrap().unwrap();
        let target = outside.join("victim");
        std::fs::write(&target, "keep\n").unwrap();
        symlink(&target, root.join(".claude/agents/.x.tmp")).unwrap();
        assert!(folder.create_new(".x.tmp").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep\n");
    }
}
