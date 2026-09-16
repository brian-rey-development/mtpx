//! Crate-private building blocks shared by the planner and the endpoints.

// The modules are pub rather than pub(crate) so the internal tree keeps one convention:
// `__bench` re-exports from `partial`, and a pub use out of a pub(crate) module is rejected.
pub mod endpoint;
pub mod executor;
pub mod local;
pub mod mtp;
pub mod partial;
