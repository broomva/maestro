//! Attributed session identity (BRO-3846): the thread variables a claiming process carries, what
//! they resolve to in T3's registry, and the audit facts a flow event records about the process.
//!
//! The variables are read once, by [`crate::SurfaceRegistry::from_env`], from the environment of
//! the process making the claim (the CLI, or the `maestro mcp` server of a stdio harness), and
//! carried in the registry. No verifier reads them with an ad-hoc `std::env::var`, so a test that
//! builds its own registry is hermetic even when it runs inside a T3 shell.

use maestro_core::session::{Ancestor, Attribution, IdentityRefusal, ProcessFacts, RefusalReason};
use std::collections::BTreeMap;
use std::fmt;

/// `T3CODE_THREAD_ID`: the T3 thread id, set by T3 for Claude threads (MCP server, shells, hooks).
pub const T3CODE_THREAD_ID: &str = "T3CODE_THREAD_ID";
/// `CODEX_THREAD_ID`: Codex's native thread id, set in Codex shells; resolved through T3's active
/// provider row.
pub const CODEX_THREAD_ID: &str = "CODEX_THREAD_ID";
/// The variable name a Codex Stop hook's stdin `session_id` is recorded under.
pub const STDIN_SESSION_ID: &str = "stdin:session_id";

/// How `MAESTRO_IDENTITY` configures the verifier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IdentityMode {
    /// Exactly the credential rules that held before BRO-3846: thread variables are ignored.
    Strict,
    /// The default: an unsigned T3 flow write is attributed by its thread variable, and a variable
    /// that contradicts a credentialed claim is refused.
    #[default]
    Attributed,
}

impl IdentityMode {
    /// `strict` or `attributed`; unset or empty is attributed. Any other value is strict, the
    /// mode that adds no path: a typo never widens what is accepted.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            None | Some("") | Some("attributed") => Self::Attributed,
            _ => Self::Strict,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Attributed => "attributed",
        }
    }
}

/// One thread variable: its name, value, and whether it names a provider's native thread
/// (resolved through the active provider row) or a T3 thread row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadVar {
    pub name: String,
    pub value: String,
    pub native: bool,
}

/// The thread variables of the claiming process, captured once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadVars(Vec<ThreadVar>);

impl ThreadVars {
    /// From this process's environment: `T3CODE_THREAD_ID` and `CODEX_THREAD_ID`, when set and
    /// not empty.
    pub fn from_env() -> Self {
        Self::from_pairs(
            [T3CODE_THREAD_ID, CODEX_THREAD_ID].into_iter().filter_map(|k| std::env::var(k).ok().map(|v| (k, v))),
        )
    }

    /// From `(name, value)` pairs, as tests and the Stop gate build them. An empty value is no
    /// variable. `T3CODE_THREAD_ID` names a thread row; any other name a native thread.
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, String)>) -> Self {
        Self(
            pairs
                .into_iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(k, v)| ThreadVar { name: k.to_string(), value: v, native: k != T3CODE_THREAD_ID })
                .collect(),
        )
    }

    /// These variables plus a native one (a Codex Stop hook's stdin `session_id`).
    pub fn with_native(mut self, name: &str, value: Option<&str>) -> Self {
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            self.0.push(ThreadVar { name: name.to_string(), value: v.to_string(), native: true });
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &ThreadVar> {
        self.0.iter()
    }

    /// The names present, for the audit facts.
    pub fn names(&self) -> Vec<String> {
        self.0.iter().map(|v| v.name.clone()).collect()
    }
}

/// What one variable resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VarOutcome {
    /// One live thread: its id and this host's environment id.
    Thread { id: String, environment: Option<String> },
    /// No thread, several, or an archived or deleted one.
    Unresolved(String),
    /// The registry did not read.
    Unreadable(String),
}

/// What the variables together say about the caller, by the spec's precedence:
/// `registry_unavailable`, then `attribution_unresolved`, then `thread_mismatch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attributed {
    /// No variable at all.
    Absent,
    /// Every variable resolved, to one thread.
    Resolved {
        attribution: Attribution,
        environment: Option<String>,
    },
    Refused(IdentityRefusal),
}

/// Combines each variable's outcome (see [`Attributed`]).
pub fn combine(each: &[(ThreadVar, VarOutcome)]) -> Attributed {
    if each.is_empty() {
        return Attributed::Absent;
    }
    if let Some((v, VarOutcome::Unreadable(why))) = each.iter().find(|(_, o)| matches!(o, VarOutcome::Unreadable(_))) {
        return Attributed::Refused(IdentityRefusal::new(
            RefusalReason::RegistryUnavailable,
            format!("{} could not be resolved: T3's registry did not read ({why}).", v.name),
        ));
    }
    if let Some((v, VarOutcome::Unresolved(why))) = each.iter().find(|(_, o)| matches!(o, VarOutcome::Unresolved(_))) {
        return Attributed::Refused(IdentityRefusal::new(
            RefusalReason::AttributionUnresolved,
            format!("{} does not name one live T3 thread: {why}.", v.name),
        ));
    }
    let threads: Vec<(&ThreadVar, &String, &Option<String>)> = each
        .iter()
        .filter_map(|(v, o)| match o {
            VarOutcome::Thread { id, environment } => Some((v, id, environment)),
            _ => None,
        })
        .collect();
    let Some((first, id, environment)) = threads.first() else { return Attributed::Absent };
    if let Some((other, other_id, _)) = threads.iter().find(|(_, t, _)| t != id) {
        return Attributed::Refused(IdentityRefusal::new(
            RefusalReason::ThreadMismatch,
            format!("{} names thread {id}, but {} names thread {other_id}.", first.name, other.name),
        ));
    }
    Attributed::Resolved {
        attribution: Attribution { variable: first.name.clone(), thread: (*id).clone() },
        environment: (*environment).clone(),
    }
}

/// What the claiming process's facts are read from: `/proc` on Linux, `ps(1)` elsewhere (macOS
/// reads `kinfo_proc` through `sysctl` inside `ps`; the workspace forbids `unsafe`, so Maestro does
/// not call `sysctl` itself). Tests inject their own.
pub trait ProcessReader: fmt::Debug + Send + Sync {
    /// One process: its parent, command name, start time (RFC 3339) and real uid.
    fn read(&self, pid: u32) -> Result<ProcStat, String>;
}

/// One process, as a [`ProcessReader`] reads it. A part that did not read is `Err` with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcStat {
    pub ppid: u32,
    pub command: String,
    pub start_time: Result<String, String>,
    pub uid: Result<u32, String>,
}

/// The most ancestors the audit facts name.
pub const MAX_ANCESTORS: usize = 8;

/// The audit facts of process `pid`: its start time and uid, up to [`MAX_ANCESTORS`] ancestors
/// with their command names, and the names of the thread variables present. An unread fact is
/// `null`, and `unread` says why.
pub fn process_facts(reader: &dyn ProcessReader, pid: u32, variables: Vec<String>) -> ProcessFacts {
    let mut unread = BTreeMap::new();
    let mut facts =
        ProcessFacts { pid, start_time: None, uid: None, ancestors: None, variables, unread: BTreeMap::new() };
    let me = match reader.read(pid) {
        Ok(me) => me,
        Err(why) => {
            for fact in ["start_time", "uid", "ancestors"] {
                unread.insert(fact.to_string(), why.clone());
            }
            facts.unread = unread;
            return facts;
        }
    };
    match me.start_time {
        Ok(t) => facts.start_time = Some(t),
        Err(why) => {
            unread.insert("start_time".to_string(), why);
        }
    }
    match me.uid {
        Ok(u) => facts.uid = Some(u),
        Err(why) => {
            unread.insert("uid".to_string(), why);
        }
    }
    let mut ancestors = Vec::new();
    let mut next = me.ppid;
    while next != 0 && ancestors.len() < MAX_ANCESTORS {
        match reader.read(next) {
            Ok(stat) => {
                ancestors.push(Ancestor { pid: next, command: Some(stat.command) });
                next = if stat.ppid == next { 0 } else { stat.ppid };
            }
            Err(why) => {
                ancestors.push(Ancestor { pid: next, command: None });
                unread.insert("ancestors".to_string(), format!("process {next}: {why}"));
                break;
            }
        }
    }
    facts.ancestors = Some(ancestors);
    facts.unread = unread;
    facts
}

/// The running system's processes (see [`ProcessReader`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProcesses;

impl ProcessReader for SystemProcesses {
    fn read(&self, pid: u32) -> Result<ProcStat, String> {
        if std::path::Path::new("/proc/self/stat").exists() { read_proc(pid) } else { read_ps(pid) }
    }
}

/// Linux: `/proc/<pid>/stat` (comm, ppid, start ticks since boot), `/proc/<pid>/status` (real
/// uid), `/proc/stat` (`btime`), and the clock tick rate.
fn read_proc(pid: u32) -> Result<ProcStat, String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| format!("/proc/{pid}/stat: {e}"))?;
    let open = stat.find('(').ok_or("stat has no command")?;
    let close = stat.rfind(')').ok_or("stat has no command")?;
    let command = stat.get(open + 1..close).unwrap_or_default().to_string();
    let rest: Vec<&str> = stat.get(close + 1..).unwrap_or_default().split_whitespace().collect();
    // After the command: state (0), ppid (1), ... starttime is field 22 of stat, index 19 here.
    let ppid = rest.get(1).and_then(|p| p.parse().ok()).ok_or("stat has no ppid")?;
    let start_time =
        rest.get(19).and_then(|t| t.parse::<u64>().ok()).ok_or_else(|| "stat has no start time".to_string()).and_then(
            |ticks| {
                let btime = std::fs::read_to_string("/proc/stat")
                    .map_err(|e| format!("/proc/stat: {e}"))?
                    .lines()
                    .find_map(|l| l.strip_prefix("btime ").and_then(|b| b.trim().parse::<u64>().ok()))
                    .ok_or("/proc/stat has no btime")?;
                let hz = rustix::param::clock_ticks_per_second();
                if hz == 0 {
                    return Err("the clock tick rate is 0".to_string());
                }
                Ok(rfc3339(btime + ticks / hz))
            },
        );
    let uid = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|e| format!("/proc/{pid}/status: {e}"))
        .and_then(|status| {
            status
                .lines()
                .find_map(|l| l.strip_prefix("Uid:").and_then(|u| u.split_whitespace().next()?.parse().ok()))
                .ok_or_else(|| "status has no Uid".to_string())
        });
    Ok(ProcStat { ppid, command, start_time, uid })
}

/// Elsewhere (macOS): `ps -o ppid=,uid=,lstart=,comm= -p <pid>` in the C locale.
fn read_ps(pid: u32) -> Result<ProcStat, String> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "ppid=,uid=,lstart=,comm=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("ps: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let words: Vec<&str> = text.split_whitespace().collect();
    // ppid uid Www Mmm dd hh:mm:ss yyyy comm...
    if !out.status.success() || words.len() < 8 {
        return Err(format!("ps found no process {pid}"));
    }
    let ppid = words[0].parse().map_err(|_| format!("ps ppid {:?}", words[0]))?;
    let uid = words[1].parse().map_err(|_| format!("ps uid {:?}", words[1]));
    let start_time = Ok(words[2..7].join(" "));
    let command = words[7..].join(" ");
    let command = command.rsplit('/').next().unwrap_or_default().to_string();
    Ok(ProcStat { ppid, command, start_time, uid })
}

/// `secs` since the epoch as RFC 3339 UTC, without a date library in this crate.
fn rfc3339(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // Civil from days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_formats_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_763_420_400), "2025-11-17T23:00:00Z");
    }

    #[test]
    fn this_process_reads_its_own_facts() {
        let facts = process_facts(&SystemProcesses, std::process::id(), vec![]);
        assert!(facts.uid.is_some() && facts.start_time.is_some(), "{facts:?}");
        assert!(facts.ancestors.as_ref().is_some_and(|a| !a.is_empty()), "{facts:?}");
    }
}
