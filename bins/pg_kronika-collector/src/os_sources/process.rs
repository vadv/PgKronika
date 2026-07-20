use super::{
    DueSet, Instant, Interner, OsCgroupMapping, OsSources, ProcFs, ProcessError, SourceKind, Ts,
    intern_str, log_cap_degraded, log_collection_finish, log_count_degraded, log_degraded,
    os_max_procs, process_facts, read_process, snapshot_coverage,
};

#[allow(
    clippy::too_many_lines,
    reason = "process sections share procfs enumeration and degradation counters"
)]
pub(super) fn collect_process_sections(
    fs: &ProcFs,
    interner: &mut Interner,
    scope: u8,
    ts: i64,
    due: &DueSet,
    os: &mut OsSources,
) {
    let hot_due = due.has(SourceKind::OsProcesses);
    let status_due = due.has(SourceKind::OsProcessStatus);
    let mapping_due = due.has(SourceKind::OsCgroupMapping);
    if !hot_due && !status_due && !mapping_due {
        return;
    }

    let hot_type_id = 1_100_001_u32;
    let status_type_id = 1_101_001_u32;
    let mapping_type_id = 1_200_001_u32;
    let started = Instant::now();
    let facts = match process_facts(fs) {
        Ok(facts) => facts,
        Err(err) => {
            for type_id in [hot_type_id, status_type_id, mapping_type_id] {
                if (type_id == hot_type_id && hot_due)
                    || (type_id == status_type_id && status_due)
                    || (type_id == mapping_type_id && mapping_due)
                {
                    log_degraded(type_id, "process", &err);
                }
            }
            if hot_due {
                os.snapshot_coverage
                    .push(snapshot_coverage(ts, hot_type_id, 3, 2, 0, 0));
            }
            return;
        }
    };
    let max_procs = usize::try_from(os_max_procs(hot_type_id)).unwrap_or(usize::MAX);
    let capped = match fs.pid_dirs_capped(max_procs) {
        Ok(capped) => capped,
        Err(err) => {
            for type_id in [hot_type_id, status_type_id, mapping_type_id] {
                if (type_id == hot_type_id && hot_due)
                    || (type_id == status_type_id && status_due)
                    || (type_id == mapping_type_id && mapping_due)
                {
                    log_degraded(type_id, "process", &err);
                }
            }
            if hot_due {
                os.snapshot_coverage
                    .push(snapshot_coverage(ts, hot_type_id, 3, 2, 0, 0));
            }
            return;
        }
    };
    let dropped = capped.dropped;
    if dropped > 0 {
        for type_id in [hot_type_id, status_type_id, mapping_type_id] {
            if (type_id == hot_type_id && hot_due)
                || (type_id == status_type_id && status_due)
                || (type_id == mapping_type_id && mapping_due)
            {
                log_cap_degraded(type_id, "process", "process_cap", dropped, max_procs);
            }
        }
    }

    let mut skipped = 0_usize;
    let mut io_nulls = 0_usize;
    let mut mapping_nulls = 0_usize;
    for pid in capped.pids {
        let read = match read_process(fs, pid, facts, ts) {
            Ok(read) => read,
            Err(ProcessError::Gone(_)) => continue,
            Err(_) => {
                skipped = skipped.saturating_add(1);
                continue;
            }
        };
        if hot_due {
            if read.hot.io.is_none() {
                io_nulls = io_nulls.saturating_add(1);
            }
            let Some(comm) = intern_str(interner, hot_type_id, "process", &read.hot.comm) else {
                skipped = skipped.saturating_add(1);
                continue;
            };
            let cmdline = read
                .hot
                .cmdline
                .as_deref()
                .and_then(|value| intern_str(interner, hot_type_id, "process", value));
            os.processes
                .push(kronika_source_os::proc::process::to_hot_section(
                    &read.hot, scope, comm, cmdline,
                ));
        }
        if status_due {
            os.process_status
                .push(kronika_source_os::proc::process::to_status_section(
                    &read.status,
                    scope,
                ));
        }
        if mapping_due {
            if let Some(mapping) = read.cgroup {
                if let Some(cgroup_path) = intern_str(
                    interner,
                    mapping_type_id,
                    "process/cgroup",
                    &mapping.cgroup_path,
                ) {
                    os.cgroup_mapping.push(OsCgroupMapping {
                        ts: Ts(mapping.ts),
                        pid: mapping.pid,
                        starttime: Ts(mapping.starttime),
                        cgroup_path,
                        scope,
                    });
                }
            } else {
                mapping_nulls = mapping_nulls.saturating_add(1);
            }
        }
    }

    if skipped > 0 {
        for type_id in [hot_type_id, status_type_id, mapping_type_id] {
            if (type_id == hot_type_id && hot_due)
                || (type_id == status_type_id && status_due)
                || (type_id == mapping_type_id && mapping_due)
            {
                log_count_degraded(type_id, "process", "process_skipped", skipped);
            }
        }
    }
    if hot_due && io_nulls > 0 {
        log_count_degraded(
            hot_type_id,
            "process/io",
            "process_io_unavailable",
            io_nulls,
        );
    }
    if mapping_due && mapping_nulls > 0 {
        log_count_degraded(
            mapping_type_id,
            "process/cgroup",
            "process_cgroup_unavailable",
            mapping_nulls,
        );
    }
    if hot_due {
        log_collection_finish(hot_type_id, "procfs", os.processes.len(), started.elapsed());
        let source_total = os
            .processes
            .len()
            .saturating_add(skipped)
            .saturating_add(dropped);
        let read_state = if dropped > 0 {
            1
        } else if skipped > 0 {
            4
        } else {
            0
        };
        os.snapshot_coverage.push(snapshot_coverage(
            ts,
            hot_type_id,
            read_state,
            u8::from(io_nulls > 0),
            u64::try_from(source_total).unwrap_or(u64::MAX),
            os.processes.len(),
        ));
    }
    if status_due {
        log_collection_finish(
            status_type_id,
            "procfs",
            os.process_status.len(),
            started.elapsed(),
        );
    }
    if mapping_due {
        log_collection_finish(
            mapping_type_id,
            "procfs",
            os.cgroup_mapping.len(),
            started.elapsed(),
        );
    }
}
