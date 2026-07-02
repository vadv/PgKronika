//! Operating-system collectors.
//!
//! This crate reads `/proc`, `/sys`, and cgroup data for the local host.
//! Today it provides the host identity facts for `instance_metadata`
//! (`1_021_001`); per-subsystem OS metrics land here as they are ported.

mod instance;
pub use instance::{OsInstanceFacts, collect_os_instance_facts};
