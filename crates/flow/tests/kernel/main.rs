//! BRO-3633 PR 3b1: the flow kernel's tests in one binary (definitions and the compiler, then the
//! scheduler), so a change to the crate links one test binary for them, not two.

#[path = "../common/mod.rs"]
mod common;

mod compile;
mod schedule;
