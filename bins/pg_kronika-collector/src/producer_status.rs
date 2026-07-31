//! Collector lifecycle adapter for the revisioned layout status.

use std::path::{Path, PathBuf};

use kronika_layout::{
    ProducerState, ProducerStatus, ProducerStatusError, RetentionStatus, write_producer_status,
};

use crate::config::RetentionConfig;

pub(crate) struct ProducerStatusPublisher {
    root: PathBuf,
    status: ProducerStatus,
}

impl ProducerStatusPublisher {
    pub(crate) fn start(
        root: &Path,
        collector_pid: u32,
        started_at_us: i64,
        retention: Option<RetentionStatus>,
    ) -> Result<Self, ProducerStatusError> {
        let status =
            ProducerStatus::running(collector_pid, started_at_us, started_at_us, retention);
        write_producer_status(root, &status)?;
        Ok(Self {
            root: root.to_owned(),
            status,
        })
    }

    pub(crate) fn heartbeat(&mut self, at_us: i64) -> Result<(), ProducerStatusError> {
        self.status.state = ProducerState::Running;
        self.status.last_status_at_us = at_us;
        write_producer_status(&self.root, &self.status)
    }

    pub(crate) fn stop(&mut self, at_us: i64) -> Result<(), ProducerStatusError> {
        self.status = self.status.stopped(at_us);
        write_producer_status(&self.root, &self.status)
    }
}

pub(crate) fn retention_status(
    retention: Option<RetentionConfig>,
) -> Result<Option<RetentionStatus>, ProducerStatusError> {
    retention
        .map(|retention| match retention {
            RetentionConfig::Fixed(target_bytes) => Ok(RetentionStatus::fixed(target_bytes)),
            RetentionConfig::Auto(target_percent) => RetentionStatus::auto(target_percent),
        })
        .transpose()
}
