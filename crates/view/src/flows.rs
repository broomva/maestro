//! The flow kernels as the Flows pages show them: a plain model the MCP layer fills from the flow
//! ledger (`maestro_mcp::flows_web`), so this crate renders flows without linking the ledger.
//!
//! Every instance pins the IR it started with in its `Started` event, so the ledger itself is the
//! catalog: a kernel is a flow name, a revision is one IR hash of it, and its instances are the
//! runs that pinned that hash. Nothing here reads a definition file.
//!
//! Unread is not empty: a ledger that could not be read is [`Ledger::read`]'s `Err`, and an
//! instance whose log or replay failed is listed in [`Ledger::unread`] or carries its reason in
//! [`Instance::live`], never dropped.

use std::collections::{BTreeMap, BTreeSet};

impl Ledger {
    /// The whole ledger's tally: every run read, and the runs listed but not read as unknown.
    /// `None` when the ledger was not read: no count of it holds.
    pub fn tally(&self) -> Option<Tally> {
        self.read.as_deref().ok().map(|runs| Tally::of(runs, self.unread.len()))
    }
}

/// Where a set of runs sits on its shape: how many open runs are at each node, and how many runs
/// that may be open have no known position (they did not replay), so no node counts them. The
/// per-node counts are a lower bound exactly when `unplaced` is not zero, and [`Heat::caption`]
/// says so in the words the page shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heat {
    pub at: BTreeMap<String, usize>,
    pub unplaced: usize,
}

impl Heat {
    pub fn of<'a>(runs: impl IntoIterator<Item = &'a Instance>) -> Heat {
        let mut heat = Heat::default();
        for i in runs {
            match i.frontier() {
                Some(at) if i.state().open() && !at.is_empty() => {
                    for (id, _) in at {
                        *heat.at.entry(id.to_string()).or_default() += 1;
                    }
                }
                // Open, and at no node: it waits on a repeat's verdict.
                Some(_) if i.state().open() => heat.unplaced += 1,
                Some(_) => {}
                None => heat.unplaced += 1,
            }
        }
        heat
    }

    /// What the counts on the nodes mean, and what they leave out.
    pub fn caption(&self) -> String {
        let mut s = "The count on a node is how many open runs of this revision are there now".to_string();
        match self.unplaced {
            0 => s.push('.'),
            1 => s.push_str("; 1 more run of it is at no node (it did not replay, or waits on a repeat's verdict), so no node counts it."),
            n => s.push_str(&format!(
                "; {n} more runs of it are at no node (they did not replay, or wait on a repeat's verdict), so no node counts them."
            )),
        }
        s
    }
}

/// The one accounting of a set of runs. Every statement a page makes about a set as a whole (its
/// counts, and whether it may say nothing is running) comes from here, so the list, a kernel's
/// card and its page cannot disagree, and a run whose state is unknown is counted as unknown,
/// never as open, finished or absent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Every run, the unknown ones included.
    pub runs: usize,
    /// Known to be able to move: running, waiting on an ask, or queued.
    pub open: usize,
    /// A claim on one of its nodes is held.
    pub running: usize,
    pub done: usize,
    pub stuck: usize,
    /// Retired on the owner's approval, not yet moved out of the ledger.
    pub retired: usize,
    /// Not read, or not replayed: no claim about these holds.
    pub unknown: usize,
}

impl Tally {
    /// The tally of `runs`, with `listed` more runs that were listed but not read.
    pub fn of<'a>(runs: impl IntoIterator<Item = &'a Instance>, listed: usize) -> Tally {
        let mut t = Tally { runs: listed, unknown: listed, ..Tally::default() };
        for i in runs {
            t.runs += 1;
            match i.state() {
                RunState::Unread => t.unknown += 1,
                RunState::Done => t.done += 1,
                RunState::Stuck => t.stuck += 1,
                RunState::Retired => t.retired += 1,
                RunState::Running | RunState::Asking | RunState::Queued => t.open += 1,
            }
            if i.running() {
                t.running += 1;
            }
        }
        t
    }

    /// What the unknown runs are, in the words every page uses next to the counts.
    pub fn unknown_note(&self) -> String {
        let n = self.unknown;
        let (runs, their) = if n == 1 { ("run was", "its state is") } else { ("runs were", "their state is") };
        format!(
            "{n} {runs} not read or not replayed, so {their} unknown; the counts above show them as unknown, never as open, done or absent"
        )
    }

    /// Whether a page may say that nothing in this set is running or open: only when every run
    /// in it is known.
    pub fn may_say_none(&self) -> bool {
        self.unknown == 0
    }

    /// The set in words, the same on every page: `3 runs, 1 running, 2 open, 1 done, 1 unknown`.
    pub fn line(&self) -> String {
        let n = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut parts =
            vec![n(self.runs, "run", "runs"), format!("{} running", self.running), format!("{} open", self.open)];
        for (count, word) in
            [(self.done, "done"), (self.stuck, "stuck"), (self.retired, "retired"), (self.unknown, "unknown")]
        {
            if count > 0 {
                parts.push(format!("{count} {word}"));
            }
        }
        parts.join(", ")
    }
}

/// What one read of the flow ledger found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ledger {
    /// The flows root read (or that would have been read).
    pub root: String,
    /// The host the ledger is bound to, when it was read.
    pub host: Option<String>,
    /// Every instance read, or why the ledger was not read.
    pub read: Result<Vec<Instance>, String>,
    /// Instances listed but not read, each with its reason.
    pub unread: Vec<Unread>,
    /// The clock the read replayed at: what the pages say they were read at.
    pub read_at: Option<jiff::Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unread {
    pub instance: String,
    pub reason: String,
}

/// One flow instance: its pinned kernel, its owner, its replayed state and its log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub id: String,
    /// The flow's name, without its version.
    pub flow: String,
    pub version: u32,
    pub ir_hash: String,
    /// When `Started` was appended (RFC 3339).
    pub started_at: String,
    /// When the last event was appended.
    pub last_at: String,
    pub seq: u64,
    /// The session that started the instance, as `surface:environment/id`.
    pub starter: Option<String>,
    pub params: Vec<(String, String)>,
    pub shape: Shape,
    /// The replay at the read's clock, or why it failed.
    pub live: Result<Live, String>,
    /// The log, oldest first.
    pub steps: Vec<Step>,
}

/// A kernel's structure, from its IR.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shape {
    pub ir_version: u32,
    /// Param name, whether it is required, and its declared type.
    pub params: Vec<(String, bool, Option<String>)>,
    pub workdir: Option<String>,
    pub nodes: Vec<Node>,
    pub repeats: Vec<Repeat>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    /// `pure`, `observe`, `work`, `choice`, `effect` or `human`.
    pub class: String,
    pub needs: Vec<Need>,
    /// `(on, case)`: the node runs only when `on` took `case`.
    pub switch: Option<(String, String)>,
    pub performer: Option<String>,
    pub skill: Option<String>,
    pub routing: Vec<(String, String)>,
    pub retry: Option<u32>,
    pub cancel_after: Option<u64>,
    /// `(external, heartbeat seconds)`.
    pub wait: Option<(String, u64)>,
    /// The class's own settings, as label and text: commands, guards, edges, exits, the ask.
    pub facts: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NeedKind {
    /// The node is done.
    Node,
    /// The repeat finished because its `until` held.
    Repeat,
    /// The repeat ran out of iterations.
    Exhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Need {
    pub on: String,
    pub kind: NeedKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Repeat {
    pub id: String,
    pub body: Vec<String>,
    pub max: u32,
    pub until: Option<String>,
    pub on_exhausted: Option<String>,
}

/// The instance as replay sees it now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Live {
    pub terminal: bool,
    /// The ask whose approval retired it.
    pub retired: Option<String>,
    pub nodes: BTreeMap<String, NodeLive>,
    /// Repeat id, its state and its iteration.
    pub repeats: Vec<(String, String, u32)>,
    /// What the instance's owner can do now, one line each.
    pub work: Vec<String>,
    /// Whom it waits on.
    pub waiting: Vec<Wait>,
    pub proposals: Vec<String>,
    /// The session that owns it now, as replay reports it (its starter, or the last adoption
    /// replay accepted).
    pub owner: Option<String>,
    /// Repeats waiting on their `until` verdict: open work at no node.
    pub verdicts: Vec<String>,
    /// Events replay set aside.
    pub rejected: usize,
    /// Stops the stop gate let through with work left.
    pub stops_let_through: usize,
}

impl Live {
    /// The nodes the run is at now: running first, then waiting, ready, stale.
    pub fn frontier(&self) -> Vec<(&str, NodeState)> {
        let mut at: Vec<(&str, NodeState)> =
            self.nodes.iter().filter(|(_, n)| n.state.frontier()).map(|(id, n)| (id.as_str(), n.state)).collect();
        at.sort_by_key(|(id, s)| (*s, *id));
        at
    }

    /// Nodes done.
    pub fn done(&self) -> usize {
        self.nodes.values().filter(|n| n.state == NodeState::Done).count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wait {
    /// The filed ask it waits on, as `(ledger, ask id)`: the Decisions page is where whether it is
    /// answered, and by whom, is read. `None` for a wait on something else (a former owner's lease).
    pub ask: Option<(String, String)>,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeLive {
    pub state: NodeState,
    /// Why it is not done or not ready, one line each.
    pub unmet: Vec<String>,
    /// `review=2` inside a repeat, empty outside.
    pub iteration: String,
    /// Who holds its claim, since when and until when, while it runs.
    pub claim: Option<String>,
}

/// A node's state on the pages: replay's, with `Running` for a node whose claim is held.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeState {
    Running,
    Waiting,
    Ready,
    Stale,
    #[default]
    Blocked,
    Failed,
    Exhausted,
    Done,
}

impl NodeState {
    pub fn name(self) -> &'static str {
        match self {
            NodeState::Running => "running",
            NodeState::Waiting => "waiting",
            NodeState::Ready => "ready",
            NodeState::Stale => "stale",
            NodeState::Blocked => "blocked",
            NodeState::Failed => "failed",
            NodeState::Exhausted => "exhausted",
            NodeState::Done => "done",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NodeState::Running => "Running",
            NodeState::Waiting => "Waiting",
            NodeState::Ready => "Ready",
            NodeState::Stale => "Stale",
            NodeState::Blocked => "Not reached",
            NodeState::Failed => "Failed",
            NodeState::Exhausted => "Exhausted",
            NodeState::Done => "Done",
        }
    }

    /// Where an instance is: a node in one of these states is its frontier.
    pub fn frontier(self) -> bool {
        matches!(self, NodeState::Running | NodeState::Waiting | NodeState::Ready | NodeState::Stale)
    }
}

/// One event of an instance's log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Step {
    pub seq: u64,
    pub at: String,
    pub kind: String,
    /// The node or repeat it names.
    pub subject: Option<String>,
    pub note: String,
    /// Why replay set it aside, when it did: it changed nothing.
    pub rejected: Option<String>,
}

/// An instance's state in one word: `Waiting on an ask` while it waits on a filed ask (said even
/// while a claim runs elsewhere in the run; whether the ask is answered is the Decisions page's to
/// say, so these pages never say `Needs you`), else `Running` while a claim is held, `Queued` with
/// work nobody holds, `Done` when nothing can be done and nothing failed, `Stuck` when nothing can
/// be done and something failed. Declared in the order the lists sort by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RunState {
    Asking,
    Running,
    Queued,
    Stuck,
    Done,
    Retired,
    Unread,
}

impl RunState {
    pub fn name(self) -> &'static str {
        match self {
            RunState::Running => "running",
            RunState::Asking => "asking",
            RunState::Queued => "queued",
            RunState::Stuck => "stuck",
            RunState::Done => "done",
            RunState::Retired => "retired",
            RunState::Unread => "unread",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RunState::Running => "Running",
            RunState::Asking => "Waiting on an ask",
            RunState::Queued => "Queued",
            RunState::Stuck => "Stuck",
            RunState::Done => "Done",
            RunState::Retired => "Retired",
            RunState::Unread => "Unknown",
        }
    }

    /// Not finished: it can still move.
    pub fn open(self) -> bool {
        matches!(self, RunState::Running | RunState::Asking | RunState::Queued)
    }
}

impl Instance {
    pub fn state(&self) -> RunState {
        let Ok(live) = &self.live else { return RunState::Unread };
        if live.retired.is_some() {
            return RunState::Retired;
        }
        if live.terminal {
            // A repeat that ran out is a failure only when its definition gave no route for it.
            let routed = |id: &str| {
                self.shape.repeats.iter().any(|r| r.on_exhausted.is_some() && r.body.iter().any(|b| b == id))
            };
            let failed = live
                .nodes
                .iter()
                .any(|(id, n)| n.state == NodeState::Failed || (n.state == NodeState::Exhausted && !routed(id)));
            return if failed { RunState::Stuck } else { RunState::Done };
        }
        // A wait on an ask is said first: a claim running elsewhere in the run does not hide it.
        if live.waiting.iter().any(|w| w.ask.is_some()) {
            RunState::Asking
        } else if live.nodes.values().any(|n| n.state == NodeState::Running) {
            RunState::Running
        } else {
            RunState::Queued
        }
    }

    /// The nodes it is at now (see [`Live::frontier`]); `None` when it did not replay, so where it
    /// is is unknown, never nowhere.
    pub fn frontier(&self) -> Option<Vec<(&str, NodeState)>> {
        self.live.as_ref().ok().map(Live::frontier)
    }

    /// A claim on one of its nodes is known to be held now, whatever else it waits on. A run that
    /// did not replay is not known to be running; the counts show it as unknown beside this.
    pub fn running(&self) -> bool {
        self.live.as_ref().is_ok_and(|l| l.nodes.values().any(|n| n.state == NodeState::Running))
    }

    /// Nodes done, and nodes in all; `None` when it did not replay, so how far it got is unknown,
    /// never none.
    pub fn progress(&self) -> Option<(usize, usize)> {
        self.live.as_ref().ok().map(|l| (l.done(), self.shape.nodes.len()))
    }

    /// `name@version`.
    pub fn kernel(&self) -> String {
        format!("{}@{}", self.flow, self.version)
    }
}

/// One flow name and every revision of it the ledger holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kernel<'a> {
    pub name: String,
    /// Oldest first by first run.
    pub revisions: Vec<Revision<'a>>,
}

/// One IR of a kernel: a version and the hash its runs pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision<'a> {
    pub version: u32,
    pub ir_hash: &'a str,
    /// The shape of a run of it that replayed, so the kernel accepted the pinned IR as this hash;
    /// `None` when no run of it replayed, so its shape is unverified.
    pub shape: Option<&'a Shape>,
    /// Newest first.
    pub runs: Vec<&'a Instance>,
}

impl Revision<'_> {
    pub fn first_at(&self) -> &str {
        self.runs.iter().map(|i| i.started_at.as_str()).min().unwrap_or_default()
    }

    pub fn last_at(&self) -> &str {
        self.runs.iter().map(|i| i.last_at.as_str()).max().unwrap_or_default()
    }
}

impl<'a> Kernel<'a> {
    pub fn latest(&self) -> &Revision<'a> {
        self.revisions.last().expect("a kernel has at least one revision")
    }

    /// Every run of every revision, newest start first.
    pub fn runs(&self) -> Vec<&'a Instance> {
        let mut runs: Vec<&Instance> = self.revisions.iter().flat_map(|r| r.runs.iter().copied()).collect();
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at).then_with(|| a.id.cmp(&b.id)));
        runs
    }

    pub fn last_at(&self) -> &str {
        self.revisions.iter().map(Revision::last_at).max().unwrap_or_default()
    }
}

/// The ledger's instances as kernels: grouped by flow name, then by (version, IR hash). Kernels
/// are ordered by their latest activity, newest first; revisions by version then first run.
pub fn kernels(instances: &[Instance]) -> Vec<Kernel<'_>> {
    let mut by_name: BTreeMap<&str, BTreeMap<(u32, &str), Vec<&Instance>>> = BTreeMap::new();
    for i in instances {
        by_name.entry(&i.flow).or_default().entry((i.version, &i.ir_hash)).or_default().push(i);
    }
    let mut out: Vec<Kernel> = by_name
        .into_iter()
        .map(|(name, revs)| {
            let mut revisions: Vec<Revision> = revs
                .into_iter()
                .map(|((version, ir_hash), mut runs)| {
                    runs.sort_by(|a, b| b.started_at.cmp(&a.started_at).then_with(|| a.id.cmp(&b.id)));
                    let shape = runs.iter().find(|i| i.live.is_ok()).map(|i| &i.shape);
                    Revision { version, ir_hash, shape, runs }
                })
                .collect();
            revisions.sort_by(|a, b| (a.version, a.first_at()).cmp(&(b.version, b.first_at())));
            Kernel { name: name.to_string(), revisions }
        })
        .collect();
    out.sort_by(|a, b| b.last_at().cmp(a.last_at()).then_with(|| a.name.cmp(&b.name)));
    out
}

/// What changed from one revision's shape to the next, one line each. Empty when the shapes
/// agree on everything this model holds.
pub fn diff(before: &Shape, after: &Shape) -> Vec<String> {
    let mut out = Vec::new();
    if before.ir_version != after.ir_version {
        out.push(format!("IR version {} to {}", before.ir_version, after.ir_version));
    }
    let names = |s: &Shape| s.nodes.iter().map(|n| n.id.clone()).collect::<BTreeSet<_>>();
    let (b, a) = (names(before), names(after));
    let list = |set: Vec<&String>| set.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
    let added: Vec<&String> = a.difference(&b).collect();
    let removed: Vec<&String> = b.difference(&a).collect();
    if !added.is_empty() {
        out.push(format!("Added {}", list(added)));
    }
    if !removed.is_empty() {
        out.push(format!("Removed {}", list(removed)));
    }
    for n in &after.nodes {
        let Some(old) = before.nodes.iter().find(|o| o.id == n.id) else { continue };
        if old.class != n.class {
            out.push(format!("{} became {} (was {})", n.id, n.class, old.class));
        }
        let sorted = |n: &Node| n.needs.iter().cloned().collect::<BTreeSet<_>>();
        if sorted(old) != sorted(n) || old.switch != n.switch {
            out.push(format!("{} has new dependencies", n.id));
        }
        fn settings(n: &Node) -> impl PartialEq + '_ {
            (&n.facts, &n.performer, &n.skill, &n.routing, n.retry, n.cancel_after, &n.wait)
        }
        if settings(old) != settings(n) {
            out.push(format!("{} changed its commands or settings", n.id));
        }
    }
    let params = |s: &Shape| s.params.iter().map(|p| p.0.clone()).collect::<BTreeSet<_>>();
    let (pb, pa) = (params(before), params(after));
    let added: Vec<&String> = pa.difference(&pb).collect();
    let removed: Vec<&String> = pb.difference(&pa).collect();
    if !added.is_empty() {
        out.push(format!("New params {}", list(added)));
    }
    if !removed.is_empty() {
        out.push(format!("Dropped params {}", list(removed)));
    }
    for (name, required, ty) in &after.params {
        if let Some((_, was_required, was_ty)) = before.params.iter().find(|p| p.0 == *name)
            && (was_required, was_ty) != (required, ty)
        {
            out.push(format!("Param {name} changed its requiredness or type"));
        }
    }
    if before.repeats != after.repeats {
        out.push("Repeats changed".into());
    }
    if before.workdir != after.workdir {
        out.push("Workdir changed".into());
    }
    out
}

/// A shape laid out left to right: each node's column is its longest chain of dependencies, and
/// inside a column nodes sit near the nodes they depend on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    /// Node ids by column, then row.
    pub columns: Vec<Vec<String>>,
    /// `(from, to, kind, label)`: `to` depends on `from`.
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeKind {
    Needs,
    /// The node runs only on one case of `from`.
    Switch,
    /// Needs a repeat to finish.
    Repeat,
    /// Runs when a repeat ran out.
    Exhausted,
}

impl EdgeKind {
    pub fn name(self) -> &'static str {
        match self {
            EdgeKind::Needs => "needs",
            EdgeKind::Switch => "switch",
            EdgeKind::Repeat => "repeat",
            EdgeKind::Exhausted => "exhausted",
        }
    }
}

impl Layout {
    /// `(column, row)` of a node.
    pub fn place(&self, id: &str) -> Option<(usize, usize)> {
        self.columns.iter().enumerate().find_map(|(c, col)| col.iter().position(|n| n == id).map(|r| (c, r)))
    }

    pub fn rows(&self) -> usize {
        self.columns.iter().map(Vec::len).max().unwrap_or(0)
    }
}

/// The edges a shape draws: each need (a repeat's need from each of its body's nodes) and each
/// switch's subject when the node does not already need it.
pub fn edges(shape: &Shape) -> Vec<Edge> {
    let known: BTreeSet<&str> = shape.nodes.iter().map(|n| n.id.as_str()).collect();
    let mut out = BTreeSet::new();
    for n in &shape.nodes {
        for need in &n.needs {
            match need.kind {
                NeedKind::Node if known.contains(need.on.as_str()) => {
                    let label = n.switch.as_ref().filter(|(on, _)| *on == need.on).map(|(_, case)| case.clone());
                    let kind = if label.is_some() { EdgeKind::Switch } else { EdgeKind::Needs };
                    out.insert(Edge { from: need.on.clone(), to: n.id.clone(), kind, label });
                }
                NeedKind::Node => {}
                NeedKind::Repeat | NeedKind::Exhausted => {
                    let kind = if need.kind == NeedKind::Repeat { EdgeKind::Repeat } else { EdgeKind::Exhausted };
                    let body =
                        shape.repeats.iter().find(|r| r.id == need.on).map(|r| r.body.clone()).unwrap_or_default();
                    for b in body.iter().filter(|b| known.contains(b.as_str()) && **b != n.id) {
                        let label =
                            Some(if kind == EdgeKind::Repeat { "until".to_string() } else { "exhausted".into() });
                        out.insert(Edge { from: b.clone(), to: n.id.clone(), kind, label });
                    }
                }
            }
        }
        if let Some((on, case)) = &n.switch
            && known.contains(on.as_str())
            && !n.needs.iter().any(|x| x.kind == NeedKind::Node && x.on == *on)
        {
            out.insert(Edge { from: on.clone(), to: n.id.clone(), kind: EdgeKind::Switch, label: Some(case.clone()) });
        }
    }
    out.into_iter().collect()
}

/// Lay a shape out. A cycle (which the IR's validation refuses) is cut where it is found, so a
/// malformed shape still draws.
pub fn layout(shape: &Shape) -> Layout {
    let edges = edges(shape);
    let mut deps: BTreeMap<&str, Vec<&str>> = shape.nodes.iter().map(|n| (n.id.as_str(), vec![])).collect();
    for e in &edges {
        if let Some(d) = deps.get_mut(e.to.as_str()) {
            d.push(e.from.as_str());
        }
    }
    fn depth<'a>(
        id: &'a str,
        deps: &BTreeMap<&'a str, Vec<&'a str>>,
        memo: &mut BTreeMap<&'a str, usize>,
        visiting: &mut BTreeSet<&'a str>,
    ) -> usize {
        if let Some(d) = memo.get(id) {
            return *d;
        }
        if !visiting.insert(id) {
            return 0;
        }
        let d = deps.get(id).map_or(0, |ds| ds.iter().map(|p| depth(p, deps, memo, visiting) + 1).max().unwrap_or(0));
        visiting.remove(id);
        memo.insert(id, d);
        d
    }
    let mut memo = BTreeMap::new();
    let mut columns: Vec<Vec<String>> = Vec::new();
    for n in &shape.nodes {
        let d = depth(&n.id, &deps, &mut memo, &mut BTreeSet::new());
        if columns.len() <= d {
            columns.resize(d + 1, vec![]);
        }
        columns[d].push(n.id.clone());
    }
    // Order each column by the mean row of what its nodes depend on (one pass, left to right).
    let mut row: BTreeMap<String, f64> = BTreeMap::new();
    for col in columns.iter_mut() {
        let key = |id: &String| {
            let ds = deps.get(id.as_str()).map(Vec::as_slice).unwrap_or_default();
            let rows: Vec<f64> = ds.iter().filter_map(|d| row.get(*d).copied()).collect();
            if rows.is_empty() { f64::MAX } else { rows.iter().sum::<f64>() / rows.len() as f64 }
        };
        col.sort_by(|a, b| key(a).total_cmp(&key(b)).then_with(|| a.cmp(b)));
        for (r, id) in col.iter().enumerate() {
            row.insert(id.clone(), r as f64);
        }
    }
    Layout { columns, edges }
}

/// How the six node classes read, for the pages' glossary.
pub const CLASSES: [(&str, &str); 6] = [
    ("pure", "A command the kernel runs; its output depends only on its inputs."),
    ("observe", "A command that reads the world; stale after its freshness window or when an input changes."),
    ("work", "Done by a performer (an agent or a person) and accepted when its verify command passes."),
    ("choice", "A performer picks one edge; each edge has a guard that must hold."),
    ("effect", "An action on the world, run at most once per iteration, with a reconcile to learn its outcome."),
    ("human", "Waits for an answer to an ask, filed through the asks ledger."),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, class: &str, needs: &[&str]) -> Node {
        Node {
            id: id.into(),
            class: class.into(),
            needs: needs.iter().map(|n| Need { on: n.to_string(), kind: NeedKind::Node }).collect(),
            ..Node::default()
        }
    }

    fn instance(id: &str, flow: &str, version: u32, hash: &str, started: &str, shape: Shape, live: Live) -> Instance {
        Instance {
            id: id.into(),
            flow: flow.into(),
            version,
            ir_hash: hash.into(),
            started_at: started.into(),
            last_at: started.into(),
            seq: 1,
            starter: None,
            params: vec![],
            shape,
            live: Ok(live),
            steps: vec![],
        }
    }

    fn live(states: &[(&str, NodeState)]) -> Live {
        Live {
            nodes: states
                .iter()
                .map(|(id, s)| (id.to_string(), NodeLive { state: *s, ..NodeLive::default() }))
                .collect(),
            ..Live::default()
        }
    }

    #[test]
    fn a_fork_and_join_lays_out_in_columns_by_longest_chain() {
        let shape = Shape {
            nodes: vec![
                node("a", "pure", &[]),
                node("b", "work", &["a"]),
                node("c", "work", &["a"]),
                node("d", "pure", &["b", "c"]),
                node("e", "pure", &["a", "d"]),
            ],
            ..Shape::default()
        };
        let l = layout(&shape);
        assert_eq!(l.columns, vec![vec!["a"], vec!["b", "c"], vec!["d"], vec!["e"]]);
        assert_eq!(l.place("e"), Some((3, 0)));
        assert_eq!(l.edges.len(), 6);
    }

    #[test]
    fn a_switch_and_a_repeat_draw_their_own_edges_and_a_cycle_still_lays_out() {
        let mut ship = node("ship", "pure", &[]);
        ship.switch = Some(("pick".into(), "ship".into()));
        let mut after = node("after", "pure", &[]);
        after.needs = vec![Need { on: "loop".into(), kind: NeedKind::Exhausted }];
        let shape = Shape {
            nodes: vec![node("pick", "choice", &[]), ship, node("fix", "work", &["pick"]), after],
            repeats: vec![Repeat { id: "loop".into(), body: vec!["fix".into()], max: 3, ..Repeat::default() }],
            ..Shape::default()
        };
        let e = edges(&shape);
        assert!(e.contains(&Edge {
            from: "pick".into(),
            to: "ship".into(),
            kind: EdgeKind::Switch,
            label: Some("ship".into())
        }));
        assert!(e.iter().any(|e| e.from == "fix" && e.to == "after" && e.kind == EdgeKind::Exhausted));
        assert_eq!(layout(&shape).place("after"), Some((2, 0)));

        let cyclic = Shape { nodes: vec![node("x", "pure", &["y"]), node("y", "pure", &["x"])], ..Shape::default() };
        assert_eq!(layout(&cyclic).columns.iter().map(Vec::len).sum::<usize>(), 2, "every node is placed");
    }

    #[test]
    fn run_state_says_running_only_on_a_held_claim_and_asking_only_on_an_ask() {
        let shape = Shape { nodes: vec![node("a", "work", &[]), node("b", "human", &["a"])], ..Shape::default() };
        let mk = |l: Live| instance("fi_1", "f", 1, "h", "2026-11-26T00:00:00Z", shape.clone(), l);
        assert_eq!(mk(live(&[("a", NodeState::Running), ("b", NodeState::Blocked)])).state(), RunState::Running);
        assert_eq!(mk(live(&[("a", NodeState::Ready), ("b", NodeState::Blocked)])).state(), RunState::Queued);
        let mut waiting = live(&[("a", NodeState::Done), ("b", NodeState::Waiting)]);
        assert_eq!(mk(waiting.clone()).state(), RunState::Queued, "a wait on no ask is not asking");
        waiting.waiting.push(Wait { ask: None, text: "a former owner's lease".into() });
        assert_eq!(mk(waiting.clone()).state(), RunState::Queued, "a lease is not an ask");
        waiting.waiting.push(Wait { ask: Some(("arc.yaml".into(), "a1".into())), text: "answer".into() });
        assert_eq!(mk(waiting).state(), RunState::Asking);
        let mut done = live(&[("a", NodeState::Done), ("b", NodeState::Done)]);
        done.terminal = true;
        assert_eq!(mk(done).state(), RunState::Done);
        let mut stuck = live(&[("a", NodeState::Failed), ("b", NodeState::Blocked)]);
        stuck.terminal = true;
        assert_eq!(mk(stuck).state(), RunState::Stuck);
        let mut unread = mk(Live::default());
        unread.live = Err("torn tail".into());
        assert_eq!(unread.state(), RunState::Unread, "an unread replay is never shown as done or queued");
    }

    #[test]
    fn kernels_group_by_name_and_revision_and_diff_their_shapes() {
        let v1 =
            Shape { ir_version: 3, nodes: vec![node("a", "pure", &[]), node("b", "work", &["a"])], ..Shape::default() };
        let v2 = Shape {
            ir_version: 4,
            nodes: vec![node("a", "observe", &[]), node("c", "work", &["a"])],
            params: vec![("repo".into(), true, None)],
            ..Shape::default()
        };
        let runs = vec![
            instance("fi_1", "k", 1, "h1", "2026-11-01T00:00:00Z", v1.clone(), Live::default()),
            instance("fi_2", "k", 2, "h2", "2026-11-02T00:00:00Z", v2.clone(), Live::default()),
            instance("fi_3", "k", 1, "h1", "2026-11-03T00:00:00Z", v1.clone(), Live::default()),
            instance("fi_4", "other", 1, "h9", "2026-10-01T00:00:00Z", v1.clone(), Live::default()),
        ];
        let ks = kernels(&runs);
        assert_eq!(ks.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(), ["k", "other"], "newest activity first");
        let k = &ks[0];
        assert_eq!(k.revisions.iter().map(|r| (r.version, r.ir_hash)).collect::<Vec<_>>(), [(1, "h1"), (2, "h2")]);
        assert_eq!(k.revisions[0].runs.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["fi_3", "fi_1"]);
        assert_eq!(k.runs().len(), 3);
        let d = diff(&v1, &v2);
        assert_eq!(d, ["IR version 3 to 4", "Added c", "Removed b", "a became observe (was pure)", "New params repo"]);
        assert!(diff(&v1, &v1).is_empty());
    }

    #[test]
    fn every_setting_the_pages_show_is_compared_and_need_order_is_not() {
        let base = Shape {
            nodes: vec![node("a", "pure", &[]), node("b", "pure", &[]), node("w", "work", &["a", "b"])],
            params: vec![("repo".into(), true, None)],
            ..Shape::default()
        };
        assert!(diff(&base, &base.clone()).is_empty(), "the unchanged control");
        let changed = |f: &dyn Fn(&mut Shape)| {
            let mut s = base.clone();
            f(&mut s);
            diff(&base, &s)
        };
        assert_eq!(changed(&|s| s.nodes[2].cancel_after = Some(60)), ["w changed its commands or settings"]);
        assert_eq!(
            changed(&|s| s.nodes[2].routing = vec![("provider".into(), "codex".into())]),
            ["w changed its commands or settings"]
        );
        assert_eq!(changed(&|s| s.params[0].1 = false), ["Param repo changed its requiredness or type"]);
        assert_eq!(
            changed(&|s| s.params[0].2 = Some("sha256".into())),
            ["Param repo changed its requiredness or type"]
        );
        assert!(changed(&|s| s.nodes[2].needs.reverse()).is_empty(), "the same needs in another order");
    }

    #[test]
    fn an_ask_wait_outranks_a_running_claim_and_a_routed_exhaustion_is_done() {
        let shape = Shape { nodes: vec![node("a", "work", &[]), node("b", "human", &[])], ..Shape::default() };
        let mut both = live(&[("a", NodeState::Running), ("b", NodeState::Waiting)]);
        both.waiting.push(Wait { ask: Some(("arc.yaml".into(), "a1".into())), text: "answer".into() });
        let i = instance("fi_1", "f", 1, "h", "2026-11-26T00:00:00Z", shape, both);
        assert_eq!(i.state(), RunState::Asking);

        let mut routed =
            Shape { nodes: vec![node("try", "work", &[]), node("escalate", "human", &[])], ..Shape::default() };
        routed.repeats = vec![Repeat {
            id: "r".into(),
            body: vec!["try".into()],
            max: 2,
            on_exhausted: Some("escalate".into()),
            ..Repeat::default()
        }];
        let mut ended = live(&[("try", NodeState::Exhausted), ("escalate", NodeState::Done)]);
        ended.terminal = true;
        let mut i = instance("fi_2", "f", 1, "h", "2026-11-26T00:00:00Z", routed, ended);
        assert_eq!(i.state(), RunState::Done, "the definition's own route for running out");
        i.shape.repeats[0].on_exhausted = None;
        assert_eq!(i.state(), RunState::Stuck, "running out with no route is stuck");
    }
}
