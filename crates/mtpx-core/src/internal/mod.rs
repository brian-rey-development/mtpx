//! Crate-private building blocks shared by the planner and the endpoints.

// pub rather than pub(crate) only so the bench-internals seam can re-export it; the module itself is private.
pub mod endpoint;
pub mod local;
pub mod partial;
