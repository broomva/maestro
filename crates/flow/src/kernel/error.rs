//! Refusals from the flow kernel, each with a stable machine code ([`KernelError::code`],
//! [`CompileError::code`]). Callers match on the code, never on the message.

use crate::error::FlowError;

/// Why a definition does not compile. Nothing is written for a definition that fails.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompileError {
    /// The YAML does not parse or does not fit the definition schema.
    #[error("The definition is not valid: {detail}.")]
    Invalid { detail: String },
    /// A phase 2 feature: subflows, traits and templates.
    #[error("{feature} is not supported in phase 1.")]
    Unsupported { feature: String },
    /// Two entries share an id, after forks and repeats are counted with the nodes.
    #[error("The {kind} id {id} is used more than once.")]
    DuplicateId { kind: &'static str, id: String },
    /// A reference names nothing the definition declares.
    #[error("{from} names {name}, which the definition does not declare.")]
    UnknownReference { from: String, name: String },
    /// A loop with no bound: a repeat without a usable `max`, or a cycle through `needs`.
    #[error("The definition has a loop with no bound: {detail}.")]
    UnboundedLoop { detail: String },
    /// A switch case that its subject does not declare: an edge a choice node lacks, or a value
    /// an observe node's `exit_map` lacks.
    #[error("Node {node} switches on {on} = {value}, which {on} does not declare.")]
    UndeclaredEdge { node: String, on: String, value: String },
    /// An effect must declare how it is reconciled after a crash, since it is never replayed.
    #[error("Effect node {node} has no reconcile command.")]
    EffectWithoutReconcile { node: String },
    /// A jq guard that does not parse, or that uses a filter the kernel does not allow.
    #[error("The guard at {at} is not valid: {detail}.")]
    InvalidGuard { at: String, detail: String },
}

impl CompileError {
    pub fn code(&self) -> &'static str {
        match self {
            CompileError::Invalid { .. } => "definition_invalid",
            CompileError::Unsupported { .. } => "unsupported",
            CompileError::DuplicateId { .. } => "duplicate_id",
            CompileError::UnknownReference { .. } => "unknown_reference",
            CompileError::UnboundedLoop { .. } => "unbounded_loop",
            CompileError::UndeclaredEdge { .. } => "undeclared_edge",
            CompileError::EffectWithoutReconcile { .. } => "effect_without_reconcile",
            CompileError::InvalidGuard { .. } => "invalid_guard",
        }
    }

    pub(crate) fn invalid(detail: impl Into<String>) -> Self {
        CompileError::Invalid { detail: detail.into() }
    }
}

/// Why a kernel call did nothing.
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error(transparent)]
    Compile(#[from] CompileError),
    #[error(transparent)]
    Flow(#[from] FlowError),
    /// The params passed to `start` do not fit the flow's declared params.
    #[error("The params do not fit flow {flow}: {detail}, so no instance was started.")]
    InvalidParams { flow: String, detail: String },
    /// The instance's log verified but its events do not replay as a flow. Never read as empty.
    #[error("Instance {instance} does not replay at event {seq}: {detail}.")]
    Malformed { instance: String, seq: u64, detail: String },
    /// An event body that does not serialize.
    #[error("An event does not serialize: {0}.")]
    Encode(String),
    /// A write verb refused before anything was run or written.
    #[error(transparent)]
    Refused(#[from] Refusal),
}

/// Why a write verb (`start`, `claim`, `heartbeat`, `complete`, `choose`) wrote nothing. A refusal
/// is a normal result: the code says why, and the caller decides what to do next.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The session carries no owner marker (or an empty one), so it can own and write no flow.
    #[error("The session has no owner marker, so it writes no flow.")]
    NoOwnerMarker,
    /// The instance's `Started` records no owner, so no session may write it.
    #[error("Instance {instance} records no owner, so no session writes it.")]
    NoOwner { instance: String },
    /// Another session, or a replacement of the owner with a new marker.
    #[error("This session does not own instance {instance}; it may start an instance of its own.")]
    NotOwner { instance: String },
    #[error("The token is not a flow token.")]
    TokenMalformed,
    #[error("No claim on instance {instance} has this token.")]
    UnknownToken { instance: String },
    /// The claim's attempt was completed; its token is spent.
    #[error("The claim on node {node} was completed, so its token is spent.")]
    TokenSpent { node: String },
    /// The lease ended (or a later claim took the node): the node is released, and work goes on
    /// only through a new claim.
    #[error("The claim on node {node} lost its lease at {lease_until}; claim the node again.")]
    LeaseExpired { node: String, lease_until: String },
    /// The claim's iteration is no longer the node's current one.
    #[error("The claim on node {node} is for an iteration that is no longer current.")]
    NotCurrentIteration { node: String },
    /// A receipt the claim was bound to has changed, so the claim no longer holds the node.
    #[error("A receipt node {node} consumes has changed since it was claimed; claim it again.")]
    InputsChanged { node: String },
    #[error("Node {node} is held by a claim until {lease_until}.")]
    AlreadyClaimed { node: String, lease_until: String },
    #[error("The flow declares no node {node}.")]
    UnknownNode { node: String },
    /// A human node waits for the owner's answer; nothing claims it.
    #[error("Node {node} is a {class} node, which is not claimed with claim and complete.")]
    NotClaimable { node: String, class: String },
    /// `choose` takes a choice node's claim, and `complete` every other class's.
    #[error("Node {node} is a {class} node: use {verb} for it.")]
    WrongVerb { node: String, class: String, verb: &'static str },
    /// The node's `retry.attempts` have all ended without a receipt, so it takes no more claims.
    #[error("Node {node} has used its {attempts} attempt(s) without a receipt.")]
    AttemptsExhausted { node: String, attempts: u32 },
    /// The attempt ran past its node's `cancel.after`: it is ended, heartbeats or not.
    #[error("The claim on node {node} was cancelled at {cancel_at}; claim the node again.")]
    Cancelled { node: String, cancel_at: String },
    /// Out of order, or something it consumes is not current and fresh now.
    #[error("Node {node} is not enabled now ({state}).")]
    NotEnabled { node: String, state: String },
    /// One more attempt would overflow the attempt counter.
    #[error("Node {node} has no attempt number left.")]
    AttemptOverflow { node: String },
    #[error("The command limits are not allowed: {detail}.")]
    InvalidLimits { detail: String },
    /// The clock plus a lease is past the last time there is.
    #[error("The clock is out of range.")]
    ClockOutOfRange,
    /// The wall clock reads earlier than the instance's last event: it stepped back. Nothing is
    /// written until it passes that event.
    #[error("The clock ({now}) is earlier than the instance's last event ({at}); try again once it is past it.")]
    ClockBehind { now: String, at: String },
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::NoOwnerMarker => "no_owner_marker",
            Refusal::NoOwner { .. } => "no_owner",
            Refusal::NotOwner { .. } => "not_owner",
            Refusal::TokenMalformed => "token_malformed",
            Refusal::UnknownToken { .. } => "unknown_token",
            Refusal::TokenSpent { .. } => "token_spent",
            Refusal::LeaseExpired { .. } => "lease_expired",
            Refusal::NotCurrentIteration { .. } => "not_current_iteration",
            Refusal::InputsChanged { .. } => "inputs_changed",
            Refusal::AlreadyClaimed { .. } => "already_claimed",
            Refusal::UnknownNode { .. } => "unknown_node",
            Refusal::NotClaimable { .. } => "not_claimable",
            Refusal::WrongVerb { .. } => "wrong_verb",
            Refusal::AttemptsExhausted { .. } => "attempts_exhausted",
            Refusal::Cancelled { .. } => "cancelled",
            Refusal::NotEnabled { .. } => "not_enabled",
            Refusal::AttemptOverflow { .. } => "attempt_overflow",
            Refusal::InvalidLimits { .. } => "invalid_limits",
            Refusal::ClockOutOfRange => "clock_out_of_range",
            Refusal::ClockBehind { .. } => "clock_behind",
        }
    }
}

impl KernelError {
    pub fn code(&self) -> &'static str {
        match self {
            KernelError::Compile(e) => e.code(),
            KernelError::Flow(e) => e.code(),
            KernelError::InvalidParams { .. } => "invalid_params",
            KernelError::Malformed { .. } => "malformed_instance",
            KernelError::Encode(_) => "encode",
            KernelError::Refused(r) => r.code(),
        }
    }
}
