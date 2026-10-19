//! Maestro's typed domain.
//!
//! Every type here names something that already exists on disk or in the
//! Paseo-era plugin (`apps/maestro-paseo/shared/*.ts`), so a harness adapter,
//! the CLI and the MCP server all speak one vocabulary. Field names follow the
//! on-disk contracts verbatim; enums carry an `Other` arm where the source
//! format is open-ended, so an unknown value is reported, never dropped.
//!
//! The crate does no I/O. Reading the control plane lives in `maestro-store`.

pub mod ask;
pub mod authority;
pub mod context;
pub mod fleet;
pub mod grant;
pub mod initiative;
pub mod js;
pub mod loops;
pub mod need;
pub mod section;
pub mod session;
pub mod tick;
pub mod work;

pub use section::{Section, SourceError};
