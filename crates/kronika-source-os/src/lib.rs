//! Operating-system collectors.
//!
//! This crate reads `/proc`, `/sys`, and cgroup data for the local host.
//! It provides the host identity facts for `instance_metadata` (`1_021_001`).

mod fs;
pub use fs::ProcFs;

mod instance;
pub use instance::{OsInstanceFacts, collect_os_instance_facts};

mod scope;
pub use scope::{OsScope, detect_container};
