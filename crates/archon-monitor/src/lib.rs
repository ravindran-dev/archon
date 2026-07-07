//! eBPF-based build monitoring: loads the tracepoint programs in
//! `archon-monitor-ebpf`, evaluates observed syscalls against a per-build
//! [`Policy`], stores everything in SQLite, and produces a [`BuildReport`].
//!
//! [`loader`] is the only privileged piece (loading/attaching eBPF
//! programs needs root) and the only one without unit tests for that
//! reason. [`policy`], [`store`], and [`report`] are pure/local and fully
//! covered without needing a live kernel.

pub mod loader;
pub mod policy;
pub mod report;
pub mod store;

pub use loader::{Monitor, MonitorError};
pub use policy::{FlagReason, Policy};
pub use report::{BuildReport, FlaggedEvent};
pub use store::{EventStore, StoreError, StoredEvent};
