//! Operating-system collectors.
//!
//! This crate reads `/proc`, `/sys`, and cgroup data for the local host.
//! It provides the host identity facts for `instance_metadata` (`1_021_001`).

mod fs;
pub use fs::{FsSpace, MAX_PROC_FILE_BYTES, ProcFs, space_from_raw, statvfs};

mod instance;
pub use instance::{OsInstanceFacts, collect_os_instance_facts};

pub mod mount;
pub use mount::{
    MountEntry, container_device_set, device_map, display_path, is_k8s_infra_mount, mount_row,
    parse_mountinfo,
};

pub mod proc;
pub use proc::stat::{CpuRow, ParseError, parse_cpu};

mod scope;
pub use scope::{OsScope, detect_container};
