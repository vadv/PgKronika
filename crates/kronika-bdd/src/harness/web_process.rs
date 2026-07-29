//! Deterministic lifecycle harness for the real `pg_kronika-web` binary.
//!
//! Every case copies one sealed collector segment into a fresh owned directory,
//! binds the server on an ephemeral port, waits for an explicit post-bind
//! announcement, and drives HTTP/1.1 over a real TCP socket. Qualification-only
//! Unix-socket barriers stop publication after the temporary OVF is synced and
//! before its atomic rename; no timing sleeps or polling retries participate in
//! readiness, crash, contention, or shutdown assertions.

use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use axum::body::Bytes;
use axum::http::{HeaderMap, Request, header};
use http_body_util::{BodyExt as _, Empty};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use kronika_format::crc32c;
use kronika_layout::{DataRoot, FileKind, LayoutLimits, SegmentAddress, TemporaryKind};
use kronika_reader::{
    FactFile, LIMIT, PgmUnit, QUALIFICATION_PUBLISH_BARRIER_ENV,
    QUALIFICATION_PUBLISH_BARRIER_READY, QUALIFICATION_PUBLISH_BARRIER_RELEASE, SegmentFacts,
};
use kronika_writer::{Journal, JournalConfig};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use pg_kronika_web::qualification::PROCESS_READY_PREFIX;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader, Lines};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CAPTURED_STDERR_BYTES: usize = 1_048_576;
const OVF_HEADER_LEN: usize = 192;
const OVF_HEADER_CRC_OFFSET: usize = 188;

type ChildLines = Lines<BufReader<ChildStdout>>;

/// One captured real HTTP response.
#[derive(Debug)]
pub(crate) struct HttpResponse {
    pub(crate) status: u16,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Vec<u8>,
}

impl HttpResponse {
    /// Parse a JSON response with the raw body retained in the error context.
    pub(crate) fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.body).with_context(|| {
            format!(
                "parse status {} response as JSON: {}",
                self.status,
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    /// Parse a UTF-8 response.
    pub(crate) fn text(&self) -> Result<&str> {
        std::str::from_utf8(&self.body).context("response body is not UTF-8")
    }

    /// Assert the status and JSON content type, then parse the body.
    pub(crate) fn json_status(&self, expected: u16) -> Result<Value> {
        ensure!(
            self.status == expected,
            "HTTP status {}, expected {expected}: {}",
            self.status,
            String::from_utf8_lossy(&self.body)
        );
        let media_type = self
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        ensure!(
            media_type.starts_with("application/json")
                || media_type.starts_with("application/problem+json"),
            "status {expected} used unexpected content type {media_type:?}"
        );
        self.json()
    }
}

/// Copyable client for one running process.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WebClient {
    address: SocketAddr,
}

impl WebClient {
    /// Send one GET over a new TCP connection.
    pub(crate) async fn get(self, target: &str) -> Result<HttpResponse> {
        ensure!(target.starts_with('/'), "HTTP target must start with '/'");
        let request = async {
            let stream = TcpStream::connect(self.address)
                .await
                .with_context(|| format!("connect to real web process at {}", self.address))?;
            let io = TokioIo::new(stream);
            let (mut sender, connection) = http1::handshake(io)
                .await
                .context("perform HTTP/1.1 client handshake")?;
            let connection_task = tokio::spawn(connection);
            let request = Request::builder()
                .method("GET")
                .uri(target)
                .header(header::HOST, self.address.to_string())
                .header(header::CONNECTION, "close")
                .body(Empty::<Bytes>::new())
                .context("build lifecycle HTTP request")?;
            let response = sender
                .send_request(request)
                .await
                .with_context(|| format!("GET {target}"))?;
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let body = response
                .into_body()
                .collect()
                .await
                .with_context(|| format!("read GET {target} response"))?
                .to_bytes()
                .to_vec();
            drop(sender);
            connection_task.abort();
            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        };
        timeout(HTTP_TIMEOUT, request)
            .await
            .with_context(|| format!("GET {target} timed out after {HTTP_TIMEOUT:?}"))?
    }

    /// Send one successful JSON GET.
    pub(crate) async fn get_json(self, target: &str) -> Result<Value> {
        self.get(target).await?.json_status(200)
    }

    /// Read the real process's Prometheus exposition.
    pub(crate) async fn metrics(self) -> Result<String> {
        let response = self.get("/metrics").await?;
        ensure!(
            response.status == 200,
            "/metrics returned {}: {}",
            response.status,
            String::from_utf8_lossy(&response.body)
        );
        Ok(response.text()?.to_owned())
    }
}

/// A running qualification-enabled `pg_kronika-web` child.
pub(crate) struct WebProcess {
    child: Child,
    client: WebClient,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_task: JoinHandle<()>,
}

impl std::fmt::Debug for WebProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebProcess")
            .field("pid", &self.child.id())
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

impl WebProcess {
    /// Spawn the packaged real binary over `data_dir`.
    pub(crate) async fn spawn(data_dir: &Path, extra_env: &[(&str, &str)]) -> Result<Self> {
        let binary = std::env::var("KRONIKA_WEB_BIN").context("KRONIKA_WEB_BIN is not set")?;
        let mut command = Command::new(&binary);
        command
            .env("KRONIKA_WEB_DIR", data_dir)
            .env("KRONIKA_WEB_ADDR", "127.0.0.1:0")
            .env("KRONIKA_WEB_LOG", "warn")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for &(key, value) in extra_env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn real web binary {binary}"))?;
        let stdout = child.stdout.take().context("web stdout is not piped")?;
        let stderr = child.stderr.take().context("web stderr is not piped")?;
        let stderr_bytes = Arc::new(Mutex::new(Vec::new()));
        let stderr_task = spawn_stderr_drain(stderr, Arc::clone(&stderr_bytes));
        let mut lines = BufReader::new(stdout).lines();
        let ready = timeout(PROCESS_TIMEOUT, next_ready_line(&mut lines))
            .await
            .with_context(|| {
                format!(
                    "real web process did not announce readiness in {PROCESS_TIMEOUT:?}: {}",
                    captured_stderr(&stderr_bytes)
                )
            })??;
        let address = ready
            .strip_prefix(PROCESS_READY_PREFIX)
            .context("web readiness line has the wrong prefix")?
            .parse::<SocketAddr>()
            .with_context(|| format!("parse web readiness address from {ready:?}"))?;
        Ok(Self {
            child,
            client: WebClient { address },
            stderr: stderr_bytes,
            stderr_task,
        })
    }

    pub(crate) const fn client(&self) -> WebClient {
        self.client
    }

    /// Gracefully terminate and require a successful process exit.
    pub(crate) async fn stop(mut self) -> Result<()> {
        let pid = child_pid(&self.child)?;
        kill(pid, Signal::SIGTERM).context("send SIGTERM to real web process")?;
        let status = timeout(PROCESS_TIMEOUT, self.child.wait())
            .await
            .with_context(|| {
                format!(
                    "real web process did not stop in {PROCESS_TIMEOUT:?}: {}",
                    captured_stderr(&self.stderr)
                )
            })?
            .context("wait for real web process")?;
        self.stderr_task.abort();
        ensure!(
            status.success(),
            "real web process exited {status}: {}",
            captured_stderr(&self.stderr)
        );
        Ok(())
    }

    /// Simulate an abrupt process stop and require SIGKILL termination.
    pub(crate) async fn crash(mut self) -> Result<()> {
        let pid = child_pid(&self.child)?;
        kill(pid, Signal::SIGKILL).context("send SIGKILL to real web process")?;
        let status = timeout(PROCESS_TIMEOUT, self.child.wait())
            .await
            .context("crashed web process did not exit")?
            .context("wait for crashed web process")?;
        self.stderr_task.abort();
        ensure!(
            status.signal() == Some(Signal::SIGKILL as i32),
            "crashed web process exited {status}, expected SIGKILL: {}",
            captured_stderr(&self.stderr)
        );
        Ok(())
    }
}

fn child_pid(child: &Child) -> Result<Pid> {
    let raw = child.id().context("web process has no PID")?;
    let raw = i32::try_from(raw).context("web process PID does not fit i32")?;
    Ok(Pid::from_raw(raw))
}

async fn next_ready_line(lines: &mut ChildLines) -> Result<String> {
    match lines.next_line().await {
        Ok(Some(line)) => Ok(line),
        Ok(None) => bail!("real web process closed stdout before readiness"),
        Err(error) => Err(error).context("read real web process stdout"),
    }
}

fn spawn_stderr_drain(mut stderr: ChildStderr, sink: Arc<Mutex<Vec<u8>>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buffer = [0_u8; 4_096];
        loop {
            match stderr.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(length) => {
                    if let Ok(mut bytes) = sink.lock() {
                        append_captured_stderr(&mut bytes, &buffer[..length]);
                    }
                }
            }
        }
    })
}

fn append_captured_stderr(bytes: &mut Vec<u8>, chunk: &[u8]) {
    let available = MAX_CAPTURED_STDERR_BYTES.saturating_sub(bytes.len());
    bytes.extend_from_slice(&chunk[..chunk.len().min(available)]);
}

fn captured_stderr(bytes: &Mutex<Vec<u8>>) -> String {
    let bytes = bytes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut captured = String::from_utf8_lossy(&bytes).into_owned();
    let capped = bytes.len() == MAX_CAPTURED_STDERR_BYTES;
    drop(bytes);
    if capped {
        captured.push_str("\n[stderr capture capped at 1048576 bytes]");
    }
    captured
}

/// One fresh, exclusively owned data directory derived from a sealed fixture.
#[derive(Debug)]
pub(crate) struct WebCase {
    #[allow(dead_code, reason = "keeps the owned scenario tree alive")]
    root: TempDir,
    data_root: DataRoot,
    address: SegmentAddress,
    from_us: i64,
    to_us: i64,
    sources_before: SourceArtifacts,
}

impl WebCase {
    /// Copy one sealed PGM and an optional version-1 journal into a fresh tree.
    pub(crate) fn from_segment(
        segment: &crate::collector::SealedSegment,
        label: &str,
    ) -> Result<Self> {
        let source_root =
            DataRoot::open(segment.data_root()).context("open lifecycle fixture data root")?;
        let mut source_pgm = source_root
            .open_pgm(segment.address())
            .context("open lifecycle fixture PGM")?;
        let source_journal = source_root
            .open_active_journal()
            .context("open lifecycle fixture journal")?;
        let root = tempfile::Builder::new()
            .prefix(&format!("pgkronika-web-{label}-"))
            .tempdir()
            .context("create owned web lifecycle directory")?;
        let data_dir = root.path().join("data");
        fs::create_dir(&data_dir).context("create owned web data directory")?;
        let destination = DataRoot::open(&data_dir).context("open owned web data root")?;
        let owner = destination
            .acquire_writer(LayoutLimits::default())
            .context("acquire lifecycle fixture writer")?;
        let mut temporary = owner
            .create_pgm_temp(segment.address())
            .context("create lifecycle PGM temporary")?;
        std::io::copy(&mut source_pgm, temporary.file_mut())
            .context("copy lifecycle PGM through layout capabilities")?;
        temporary
            .file_mut()
            .sync_all()
            .context("sync lifecycle PGM fixture")?;
        temporary
            .publish()
            .context("publish lifecycle PGM fixture")?;
        drop(temporary);
        if let Some(mut source_journal) = source_journal {
            let (mut target, _created) = owner
                .open_or_create_journal()
                .context("create lifecycle journal fixture")?;
            target
                .set_len(0)
                .context("truncate lifecycle journal target")?;
            target
                .seek(SeekFrom::Start(0))
                .context("rewind lifecycle journal target")?;
            std::io::copy(&mut source_journal, &mut target).context("copy lifecycle journal v1")?;
            target.sync_all().context("sync lifecycle journal v1")?;
            drop(target);
            Journal::open(&owner, JournalConfig::default())
                .context("validate copied lifecycle journal v1")?;
        }
        drop(owner);
        let data_root = DataRoot::open(&data_dir).context("reopen owned web data root")?;
        let unit = PgmUnit::open(
            data_root
                .open_pgm(segment.address())
                .context("open copied PGM")?,
        )
        .context("open copied PGM catalog")?;
        let from_us = unit.catalog().min_ts;
        let to_us = unit
            .catalog()
            .max_ts
            .checked_add(1)
            .context("fixture maximum timestamp cannot form a half-open range")?;
        ensure!(from_us < to_us, "fixture timeline range is empty");
        let sources_before = source_artifacts(&data_root, segment.address())?;
        Ok(Self {
            root,
            data_root,
            address: segment.address(),
            from_us,
            to_us,
            sources_before,
        })
    }

    pub(crate) fn data_dir(&self) -> &Path {
        self.data_root.diagnostic_path()
    }

    pub(crate) fn sidecar(&self) -> PathBuf {
        self.data_root
            .diagnostic_file_path(self.address, FileKind::Ovf)
    }

    pub(crate) const fn range_start_us(&self) -> i64 {
        self.from_us
    }

    pub(crate) const fn to_us(&self) -> i64 {
        self.to_us
    }

    pub(crate) async fn spawn(&self, extra_env: &[(&str, &str)]) -> Result<WebProcess> {
        WebProcess::spawn(self.data_dir(), extra_env).await
    }

    pub(crate) fn seed_sidecar(&self, bytes: &[u8]) -> Result<()> {
        let owner = self
            .data_root
            .acquire_overview(LayoutLimits::default())
            .context("acquire lifecycle overview fixture owner")?;
        let mut temporary = owner
            .create_ovf_temp(self.address)
            .context("create lifecycle OVF temporary")?;
        temporary
            .file_mut()
            .write_all(bytes)
            .context("write lifecycle OVF")?;
        temporary
            .file_mut()
            .sync_all()
            .context("sync lifecycle OVF")?;
        temporary.publish().context("publish lifecycle OVF fixture")
    }

    /// Require all collector-owned source bytes to remain exactly unchanged.
    pub(crate) fn assert_sources_preserved(&self) -> Result<()> {
        ensure!(
            source_artifacts(&self.data_root, self.address)? == self.sources_before,
            "web lifecycle changed a PGM or active.parts source artifact"
        );
        Ok(())
    }

    /// Validate the committed sibling through the production reader admission.
    #[allow(
        clippy::verbose_file_reads,
        reason = "the harness must read the sidecar through the descriptor-relative layout API"
    )]
    pub(crate) fn admitted_sidecar(&self) -> Result<Vec<u8>> {
        let pgm = PgmUnit::open(
            self.data_root
                .open_pgm(self.address)
                .context("open lifecycle PGM")?,
        )
        .context("open lifecycle PGM catalog")?;
        let (identity, lineage) =
            SegmentFacts::provenance(&pgm).context("derive lifecycle PGM provenance")?;
        let mut sidecar = self
            .data_root
            .open_ovf(self.address)
            .context("open lifecycle sibling OVF")?
            .context("lifecycle sibling OVF is absent")?;
        let mut bytes = Vec::new();
        sidecar
            .read_to_end(&mut bytes)
            .context("read lifecycle sibling OVF")?;
        FactFile::admit(&bytes, &identity, &lineage, &LIMIT)
            .context("admit lifecycle sibling OVF")?;
        Ok(bytes)
    }

    pub(crate) fn publisher_artifacts(&self) -> Result<Vec<PathBuf>> {
        let snapshot = self
            .data_root
            .scan(LayoutLimits::default())
            .context("scan lifecycle data directory")?;
        let mut artifacts = snapshot
            .temporaries
            .iter()
            .filter(|temporary| {
                temporary.address == self.address && temporary.kind == TemporaryKind::Ovf
            })
            .map(|temporary| self.day_dir().join(temporary.file_name()))
            .collect::<Vec<_>>();
        artifacts.sort();
        Ok(artifacts)
    }

    pub(crate) fn seed_temporary(&self, bytes: &[u8]) -> Result<PathBuf> {
        let name = format!("{}.ovf.{}.999999.tmp", self.address.id, std::process::id());
        let path = self.temporary_path(&name)?;
        fs::write(&path, bytes).context("seed lifecycle OVF temporary")?;
        Ok(path)
    }

    pub(crate) fn temporary_path(&self, name: &str) -> Result<PathBuf> {
        ensure!(
            canonical_ovf_temporary_address(name) == Some(self.address),
            "publication barrier named an invalid layout temporary {name:?}"
        );
        Ok(self.day_dir().join(name))
    }

    fn day_dir(&self) -> PathBuf {
        self.data_root
            .diagnostic_path()
            .join(self.address.day.year_component())
            .join(self.address.day.month_component())
            .join(self.address.day.day_component())
    }

    pub(crate) fn control_path(&self, name: &str) -> Result<PathBuf> {
        let control = self.root.path().join("control");
        fs::create_dir_all(&control).context("create lifecycle control directory")?;
        Ok(control.join(name))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct SourceArtifacts {
    pgm: Vec<u8>,
    journal: Option<Vec<u8>>,
}

#[allow(
    clippy::verbose_file_reads,
    reason = "source preservation must read the exact files opened by the descriptor-relative layout API"
)]
fn source_artifacts(root: &DataRoot, address: SegmentAddress) -> Result<SourceArtifacts> {
    let mut pgm = Vec::new();
    root.open_pgm(address)
        .context("open lifecycle source PGM")?
        .read_to_end(&mut pgm)
        .context("read lifecycle source PGM")?;
    let journal = root
        .open_active_journal()
        .context("open lifecycle source journal")?
        .map(|mut journal| {
            let mut bytes = Vec::new();
            journal
                .read_to_end(&mut bytes)
                .context("read lifecycle source journal")?;
            Ok::<_, anyhow::Error>(bytes)
        })
        .transpose()?;
    Ok(SourceArtifacts { pgm, journal })
}

/// Stable Linux identity used to prove a restart did not rewrite a sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileFingerprint {
    device: u64,
    inode: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    length: u64,
    sha256: [u8; 32],
}

pub(crate) fn fingerprint(path: &Path) -> Result<FileFingerprint> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("stat lifecycle artifact {}", path.display()))?;
    let bytes =
        fs::read(path).with_context(|| format!("hash lifecycle artifact {}", path.display()))?;
    Ok(FileFingerprint {
        device: metadata.dev(),
        inode: metadata.ino(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        length: metadata.len(),
        sha256: Sha256::digest(bytes).into(),
    })
}

/// Qualification barrier accepted from a real publication worker.
pub(crate) struct PublishBarrier {
    listener: UnixListener,
    socket_path: PathBuf,
}

impl std::fmt::Debug for PublishBarrier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishBarrier")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl PublishBarrier {
    pub(crate) fn bind(socket_path: PathBuf) -> Result<Self> {
        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("bind publication barrier {}", socket_path.display()))?;
        Ok(Self {
            listener,
            socket_path,
        })
    }

    pub(crate) fn environment(&self) -> (&'static str, &str) {
        (
            QUALIFICATION_PUBLISH_BARRIER_ENV,
            self.socket_path
                .to_str()
                .expect("temporary barrier path is UTF-8"),
        )
    }

    pub(crate) async fn arrive(&self) -> Result<PublishBarrierLease> {
        let accepted = async {
            let (stream, _address) = self
                .listener
                .accept()
                .await
                .context("accept publication barrier")?;
            PublishBarrierLease::receive(stream).await
        };
        timeout(PROCESS_TIMEOUT, accepted)
            .await
            .context("publication did not reach the before-commit barrier")?
    }
}

/// Held publication barrier. Dropping it cancels the blocked publication;
/// [`release`](Self::release) lets the atomic commit continue.
pub(crate) struct PublishBarrierLease {
    stream: UnixStream,
    pub(crate) temporary_name: String,
}

impl PublishBarrierLease {
    async fn receive(stream: UnixStream) -> Result<Self> {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .context("read publication barrier message")?;
        let temporary_name = line
            .trim_end()
            .strip_prefix(&format!("{QUALIFICATION_PUBLISH_BARRIER_READY} "))
            .context("publication barrier sent the wrong message")?
            .to_owned();
        ensure!(
            canonical_ovf_temporary_address(&temporary_name).is_some(),
            "publication barrier named an invalid layout temporary {temporary_name:?}"
        );
        Ok(Self {
            stream: reader.into_inner(),
            temporary_name,
        })
    }

    pub(crate) async fn release(mut self) -> Result<()> {
        self.stream
            .write_all(QUALIFICATION_PUBLISH_BARRIER_RELEASE.as_bytes())
            .await
            .context("release publication barrier")?;
        self.stream
            .shutdown()
            .await
            .context("close publication barrier")
    }
}

fn canonical_ovf_temporary_address(name: &str) -> Option<SegmentAddress> {
    let components = name.split('.').collect::<Vec<_>>();
    let [id, "ovf", raw_pid, raw_sequence, "tmp"] = components.as_slice() else {
        return None;
    };
    let segment_id = id.parse::<i64>().ok()?;
    if segment_id.to_string() != *id {
        return None;
    }
    let pid = raw_pid.parse::<u64>().ok()?;
    if pid.to_string() != *raw_pid {
        return None;
    }
    let sequence = raw_sequence.parse::<u64>().ok()?;
    if sequence.to_string() != *raw_sequence {
        return None;
    }
    SegmentAddress::new(kronika_layout::SegmentId::new(segment_id).ok()?).ok()
}

/// Header identity classes used by stale-sidecar lifecycle cases.
#[derive(Debug, Clone, Copy)]
pub(crate) enum SidecarMismatch {
    Descriptor,
    FactSchema,
    Extractor,
    Registry,
    Lineage,
}

impl SidecarMismatch {
    pub(crate) const ALL: [Self; 5] = [
        Self::Descriptor,
        Self::FactSchema,
        Self::Extractor,
        Self::Registry,
        Self::Lineage,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Descriptor => "descriptor",
            Self::FactSchema => "fact_schema",
            Self::Extractor => "extractor",
            Self::Registry => "registry",
            Self::Lineage => "lineage",
        }
    }

    pub(crate) const fn rebuild_reason(self) -> &'static str {
        match self {
            Self::Descriptor | Self::Lineage => "wrong_source",
            Self::FactSchema | Self::Extractor | Self::Registry => "incompatible",
        }
    }
}

/// Produce a physically framed candidate whose named identity class cannot be
/// admitted for the current PGM.
pub(crate) fn mismatched_sidecar(canonical: &[u8], mismatch: SidecarMismatch) -> Result<Vec<u8>> {
    ensure!(
        canonical.len() >= OVF_HEADER_LEN,
        "canonical sidecar has no complete header"
    );
    let mut bytes = canonical.to_vec();
    match mismatch {
        SidecarMismatch::FactSchema => increment_u32(&mut bytes, 16),
        SidecarMismatch::Extractor => increment_u32(&mut bytes, 20),
        SidecarMismatch::Registry => increment_u32(&mut bytes, 24),
        SidecarMismatch::Descriptor => bytes[64] ^= 0x80,
        SidecarMismatch::Lineage => bytes[128] ^= 0x80,
    }
    reseal_header(&mut bytes);
    Ok(bytes)
}

pub(crate) fn corrupt_sidecar(canonical: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        canonical.len() > OVF_HEADER_LEN,
        "canonical sidecar has no body to corrupt"
    );
    let mut bytes = canonical.to_vec();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x80;
    Ok(bytes)
}

fn increment_u32(bytes: &mut [u8], offset: usize) {
    let value = u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("fixed OVF header field"),
    );
    bytes[offset..offset + 4].copy_from_slice(&value.wrapping_add(1).to_le_bytes());
}

fn reseal_header(bytes: &mut [u8]) {
    bytes[OVF_HEADER_CRC_OFFSET..OVF_HEADER_LEN].fill(0);
    let checksum = crc32c(&bytes[..OVF_HEADER_LEN]);
    bytes[OVF_HEADER_CRC_OFFSET..OVF_HEADER_LEN].copy_from_slice(&checksum.to_le_bytes());
}

/// Exact sample lookup from Prometheus text exposition.
pub(crate) fn metric(
    exposition: &str,
    name: &str,
    required_labels: &[(&str, &str)],
) -> Result<f64> {
    let mut matches = Vec::new();
    for line in exposition.lines().filter(|line| !line.starts_with('#')) {
        let Some((series, raw_value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let metric_name = series.split_once('{').map_or(series, |(name, _)| name);
        if metric_name != name {
            continue;
        }
        if required_labels.iter().all(|(key, value)| {
            series.contains(&format!("{key}=\"{}\"", escape_prometheus_label(value)))
        }) {
            let value = raw_value
                .split_ascii_whitespace()
                .next()
                .context("Prometheus sample has no value")?
                .parse::<f64>()
                .with_context(|| format!("parse Prometheus sample {line:?}"))?;
            matches.push(value);
        }
    }
    match matches.as_slice() {
        [value] => Ok(*value),
        [] => bail!("metric {name} with labels {required_labels:?} is absent:\n{exposition}"),
        _ => bail!("metric {name} with labels {required_labels:?} is ambiguous: {matches:?}"),
    }
}

fn escape_prometheus_label(value: &str) -> String {
    value
        .replace('\\', r"\\")
        .replace('"', r#"\""#)
        .replace('\n', r"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_stderr_capture_has_a_hard_byte_bound() {
        let mut bytes = vec![b'a'; MAX_CAPTURED_STDERR_BYTES - 2];
        append_captured_stderr(&mut bytes, b"bcdef");
        append_captured_stderr(&mut bytes, b"ignored");

        assert_eq!(bytes.len(), MAX_CAPTURED_STDERR_BYTES);
        assert!(bytes.ends_with(b"bc"));
        assert!(
            captured_stderr(&Mutex::new(bytes))
                .ends_with("[stderr capture capped at 1048576 bytes]")
        );
    }

    #[test]
    fn publication_barrier_accepts_only_closed_layout_temporary_names() {
        let address = canonical_ovf_temporary_address("1722470400000000.ovf.42.7.tmp")
            .expect("canonical OVF temporary");
        assert_eq!(address.id.get(), 1_722_470_400_000_000);
        for invalid in [
            ".pgkronika-overview.tmp-42",
            "1722470400000000.ovf.042.7.tmp",
            "1722470400000000.ovf.probe.42.7.tmp",
            "1722470400000000.pgm.42.7.tmp",
        ] {
            assert!(
                canonical_ovf_temporary_address(invalid).is_none(),
                "{invalid:?} is not an OVF publisher temporary"
            );
        }
    }
}
