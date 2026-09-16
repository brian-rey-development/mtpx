//! Crate-private building blocks shared by the planner and the endpoints.

// The modules are pub rather than pub(crate) so the internal tree keeps one convention:
// `__bench` re-exports from `partial`, and a pub use out of a pub(crate) module is rejected.
pub mod endpoint;
pub mod local;
#[cfg_attr(
    all(test, not(feature = "virtual-device")),
    expect(
        dead_code,
        reason = "only virtual-device tests exercise the MTP endpoint until the Phase 6 device facade gives it its first non-test caller"
    )
)]
pub mod mtp;
pub mod partial;
