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
}

impl KernelError {
    pub fn code(&self) -> &'static str {
        match self {
            KernelError::Compile(e) => e.code(),
            KernelError::Flow(e) => e.code(),
            KernelError::InvalidParams { .. } => "invalid_params",
            KernelError::Malformed { .. } => "malformed_instance",
            KernelError::Encode(_) => "encode",
        }
    }
}
