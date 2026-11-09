//! BRO-3633 PR 3b1: the flow kernel's tests in one binary (definitions and the compiler, then the
//! scheduler), so a change to the crate links one test binary for them, not two.

#[path = "../common/mod.rs"]
mod common;

mod claiming;
mod claims;
mod compile;
mod deciding;
mod schedule;

/// Not a test: the guard evaluator's entry, which the kernel runs when it starts this binary again
/// to evaluate a guard (`maestro_flow::kernel::guard`).
#[test]
fn guard_process_entry() {
    maestro_flow::kernel::guard_entry();
}
