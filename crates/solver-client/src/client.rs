use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use solver_contract::framing::{self, FrameError};
use solver_contract::{
    ContractError, SolveResponse, SolverEnvelope, SolverStatus, ValidateContract, solver_envelope,
};
use thiserror::Error;

use crate::CancellationToken;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(5);
const DEFAULT_STDERR_LIMIT: usize = 64 * 1024;

/// Executable and launch settings for the one-shot solver worker.
#[derive(Clone)]
pub struct SidecarSpec {
    executable: PathBuf,
    args: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    inherit_environment: bool,
    current_dir: Option<PathBuf>,
}

impl SidecarSpec {
    /// Define a worker executable without invoking a shell.
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            args: Vec::new(),
            environment: Vec::new(),
            inherit_environment: true,
            current_dir: None,
        }
    }

    /// Append one literal process argument.
    #[must_use]
    pub fn arg(mut self, argument: impl Into<OsString>) -> Self {
        self.args.push(argument.into());
        self
    }

    /// Set one worker environment variable.
    ///
    /// Environment values are deliberately redacted from this type's `Debug`
    /// output because they may contain secrets.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((key.into(), value.into()));
        self
    }

    /// Prevent inherited loader variables or other host environment from affecting a managed worker.
    /// Explicit variables added with [`Self::env`] remain available.
    #[must_use]
    pub fn clear_environment(mut self) -> Self {
        self.inherit_environment = false;
        self
    }

    /// Set the worker's current directory.
    #[must_use]
    pub fn current_dir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(directory.into());
        self
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        if !self.inherit_environment {
            command.env_clear();
        }
        command
            .args(&self.args)
            .envs(self.environment.iter().map(|(key, value)| (key, value)));
        if let Some(directory) = &self.current_dir {
            command.current_dir(directory);
        }
        command
    }
}

impl fmt::Debug for SidecarSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let environment_keys: Vec<&OsStr> = self
            .environment
            .iter()
            .map(|(key, _value)| key.as_os_str())
            .collect();
        formatter
            .debug_struct("SidecarSpec")
            .field("executable", &self.executable)
            .field("args", &self.args)
            .field("environment_keys", &environment_keys)
            .field("inherit_environment", &self.inherit_environment)
            .field("current_dir", &self.current_dir)
            .finish()
    }
}

/// A bounded stderr capture. The reader continues draining after the capture
/// limit so a verbose worker cannot deadlock on a full pipe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StderrCapture {
    /// Lossily decoded prefix retained for diagnostics.
    pub text: String,
    /// Bytes retained in `text` before UTF-8 replacement.
    pub captured_bytes: usize,
    /// Total bytes drained from the worker, saturating at `usize::MAX`.
    pub total_bytes: usize,
    /// Whether bytes were discarded because the configured limit was reached.
    pub truncated: bool,
    /// An I/O or reader-thread failure, if stderr could not be fully drained.
    pub read_failure: Option<String>,
}

/// Process-level provenance that never includes the request or response payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessReport {
    /// Wall-clock duration observed by the client.
    pub elapsed: Duration,
    /// Platform exit code, or `None` when terminated by a signal.
    pub exit_code: Option<i32>,
    /// Whether the OS reported a successful process exit.
    pub exit_success: bool,
    /// Bounded diagnostic stderr.
    pub stderr: StderrCapture,
}

/// Stable application-facing mapping of every worker protocol status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolverRunStatus {
    Optimal,
    Feasible,
    ProvenInfeasible,
    Timeout,
    Unknown,
    Cancelled,
    InvalidInput,
    InvalidModel,
    InternalError,
}

/// A protocol status plus the process evidence used to obtain it.
#[derive(Debug)]
pub struct SolverRunOutcome {
    /// Exact mapped status. `InternalError` is returned as an error instead.
    pub status: SolverRunStatus,
    /// Worker response, absent only for client-enforced timeout or cancellation.
    pub response: Option<SolveResponse>,
    /// Exit and bounded stderr details.
    pub process: ProcessReport,
}

/// Invalid stdout protocol, kept separate from worker crashes and solver errors.
#[derive(Debug, Error)]
pub enum WorkerProtocolError {
    #[error("invalid response frame: {0}")]
    Frame(#[from] FrameError),
    #[error("worker wrote data after its single response frame")]
    TrailingStdout,
    #[error("response envelope violates the solver contract: {0}")]
    Contract(#[from] ContractError),
    #[error("response request_id does not match the request")]
    MismatchedRequestId,
    #[error("response input_snapshot_hash does not match the requested snapshot")]
    MismatchedSnapshotHash,
    #[error("response engine identity does not match the required engine")]
    MismatchedEngineVersion,
    #[error("worker returned a request payload where a response was required")]
    UnexpectedPayload,
    #[error("validated response contained unknown status {0}")]
    UnknownStatus(i32),
    #[error("response status must not be UNSPECIFIED")]
    UnspecifiedStatus,
}

impl WorkerProtocolError {
    /// Stable protocol problem identity, independent of human-readable error text.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Frame(_) => "SOLVER_PROTOCOL_INVALID_FRAME",
            Self::TrailingStdout => "SOLVER_PROTOCOL_TRAILING_STDOUT",
            Self::Contract(_) => "SOLVER_PROTOCOL_INVALID_RESPONSE_CONTRACT",
            Self::MismatchedRequestId => "SOLVER_PROTOCOL_REQUEST_ID_MISMATCH",
            Self::MismatchedSnapshotHash => "SOLVER_PROTOCOL_SNAPSHOT_HASH_MISMATCH",
            Self::MismatchedEngineVersion => "SOLVER_PROTOCOL_ENGINE_VERSION_MISMATCH",
            Self::UnexpectedPayload => "SOLVER_PROTOCOL_UNEXPECTED_PAYLOAD",
            Self::UnknownStatus(_) => "SOLVER_PROTOCOL_UNKNOWN_STATUS",
            Self::UnspecifiedStatus => "SOLVER_PROTOCOL_UNSPECIFIED_STATUS",
        }
    }
}

/// Failures outside normal solver statuses.
#[derive(Debug, Error)]
pub enum SolverClientError {
    #[error("request envelope violates the solver contract: {0}")]
    InvalidRequest(#[from] ContractError),
    #[error("client expected a solve request envelope")]
    ExpectedRequestPayload,
    #[error("request frame could not be encoded: {0}")]
    RequestFrame(#[source] FrameError),
    #[error("failed to launch solver worker `{program}`: {source}")]
    Spawn {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("worker process pipe `{0}` was not available")]
    MissingPipe(&'static str),
    #[error("failed to spawn `{role}` I/O thread: {source}")]
    ThreadSpawn {
        role: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("`{0}` I/O thread panicked")]
    ThreadPanicked(&'static str),
    #[error("failed while waiting for worker process: {0}")]
    ProcessWait(#[source] io::Error),
    #[error("failed to terminate worker process: {0}")]
    ProcessTermination(#[source] io::Error),
    #[error("worker exited unsuccessfully with code {exit_code:?}")]
    WorkerExited {
        exit_code: Option<i32>,
        process: Box<ProcessReport>,
    },
    #[error("failed to write the complete request frame: {source}")]
    RequestWrite {
        #[source]
        source: io::Error,
        process: Box<ProcessReport>,
    },
    #[error("worker stdout violated the protocol: {source}")]
    Protocol {
        #[source]
        source: WorkerProtocolError,
        process: Box<ProcessReport>,
    },
    #[error("worker reported InternalError with detail code `{detail_code}`")]
    WorkerInternalError {
        detail_code: String,
        process: Box<ProcessReport>,
    },
}

/// Synchronous one-shot solver sidecar client.
#[derive(Clone, Debug)]
pub struct SolverClient {
    sidecar: SidecarSpec,
    timeout: Duration,
    poll_interval: Duration,
    maximum_frame_len: usize,
    maximum_stderr_bytes: usize,
}

impl SolverClient {
    /// Create a client with a 30-second process timeout and bounded pipes.
    pub fn new(sidecar: SidecarSpec) -> Self {
        Self {
            sidecar,
            timeout: DEFAULT_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
            maximum_frame_len: framing::DEFAULT_MAX_FRAME_LEN,
            maximum_stderr_bytes: DEFAULT_STDERR_LIMIT,
        }
    }

    /// Override the wall-clock lifetime of the worker process.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Override the response and request frame size limit.
    #[must_use]
    pub fn maximum_frame_len(mut self, maximum: usize) -> Self {
        self.maximum_frame_len = maximum;
        self
    }

    /// Override how much stderr is retained. All stderr is still drained.
    #[must_use]
    pub fn maximum_stderr_bytes(mut self, maximum: usize) -> Self {
        self.maximum_stderr_bytes = maximum;
        self
    }

    /// Execute one request in one fresh worker process.
    ///
    /// Client-enforced timeout and cancellation are successful control outcomes
    /// with no response payload. Solver-native `InternalError`, unsuccessful
    /// process exit, and malformed protocol output remain distinct errors.
    ///
    /// # Errors
    ///
    /// Returns [`SolverClientError`] for an invalid request, process/pipe
    /// failure, protocol violation, non-zero worker exit, or worker-reported
    /// `InternalError`.
    pub fn solve(
        &self,
        request: &SolverEnvelope,
        cancellation: &CancellationToken,
    ) -> Result<SolverRunOutcome, SolverClientError> {
        request.validate_contract()?;
        if !matches!(
            request.payload,
            Some(solver_envelope::Payload::SolveRequest(_))
        ) {
            return Err(SolverClientError::ExpectedRequestPayload);
        }

        if cancellation.is_cancelled() {
            return Ok(SolverRunOutcome {
                status: SolverRunStatus::Cancelled,
                response: None,
                process: ProcessReport {
                    elapsed: Duration::ZERO,
                    exit_code: None,
                    exit_success: false,
                    stderr: StderrCapture::default(),
                },
            });
        }

        let request_frame = framing::encode_frame(request, self.maximum_frame_len)
            .map_err(SolverClientError::RequestFrame)?;
        let started = Instant::now();
        let mut command = self.sidecar.command();
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().map_err(|source| SolverClientError::Spawn {
            program: self.sidecar.executable.clone(),
            source,
        })?;
        let mut child = ChildGuard::new(child);

        let mut child_stdin = child
            .child
            .stdin
            .take()
            .ok_or(SolverClientError::MissingPipe("stdin"))?;
        let child_stdout = child
            .child
            .stdout
            .take()
            .ok_or(SolverClientError::MissingPipe("stdout"))?;
        let child_stderr = child
            .child
            .stderr
            .take()
            .ok_or(SolverClientError::MissingPipe("stderr"))?;

        let input_thread = spawn_thread("solver-stdin", move || {
            child_stdin.write_all(&request_frame)?;
            child_stdin.flush()?;
            drop(child_stdin);
            Ok::<(), io::Error>(())
        })?;
        let maximum_frame_len = self.maximum_frame_len;
        let output_thread = spawn_thread("solver-stdout", move || {
            read_single_response(child_stdout, maximum_frame_len)
        })?;
        let maximum_stderr_bytes = self.maximum_stderr_bytes;
        let stderr_thread = spawn_thread("solver-stderr", move || {
            drain_stderr(child_stderr, maximum_stderr_bytes)
        })?;

        let disposition =
            wait_for_process(&mut child, self.timeout, self.poll_interval, cancellation)?;

        let input_result = input_thread.join();
        let output_result = output_thread.join();
        let stderr = stderr_capture(stderr_thread.join());
        let exit_status = disposition.exit_status();
        let process = ProcessReport {
            elapsed: started.elapsed(),
            exit_code: exit_status.code(),
            exit_success: exit_status.success(),
            stderr,
        };

        interpret_completion(request, disposition, input_result, output_result, process)
    }
}

fn interpret_completion(
    request: &SolverEnvelope,
    disposition: WaitDisposition,
    input_result: thread::Result<Result<(), io::Error>>,
    output_result: thread::Result<Result<SolverEnvelope, WorkerProtocolError>>,
    process: ProcessReport,
) -> Result<SolverRunOutcome, SolverClientError> {
    match disposition {
        WaitDisposition::Cancelled(_) => {
            return Ok(SolverRunOutcome {
                status: SolverRunStatus::Cancelled,
                response: None,
                process,
            });
        }
        WaitDisposition::TimedOut(_) => {
            return Ok(SolverRunOutcome {
                status: SolverRunStatus::Timeout,
                response: None,
                process,
            });
        }
        WaitDisposition::Exited(status) if !status.success() => {
            return Err(SolverClientError::WorkerExited {
                exit_code: status.code(),
                process: Box::new(process),
            });
        }
        WaitDisposition::Exited(_) => {}
    }

    let input_result =
        input_result.map_err(|_| SolverClientError::ThreadPanicked("solver-stdin"))?;
    input_result.map_err(|source| SolverClientError::RequestWrite {
        source,
        process: Box::new(process.clone()),
    })?;
    let response_envelope = output_result
        .map_err(|_| SolverClientError::ThreadPanicked("solver-stdout"))?
        .map_err(|source| SolverClientError::Protocol {
            source,
            process: Box::new(process.clone()),
        })?;

    response_envelope
        .validate_contract()
        .map_err(WorkerProtocolError::Contract)
        .map_err(|source| SolverClientError::Protocol {
            source,
            process: Box::new(process.clone()),
        })?;
    if response_envelope.request_id != request.request_id {
        return Err(SolverClientError::Protocol {
            source: WorkerProtocolError::MismatchedRequestId,
            process: Box::new(process),
        });
    }
    let Some(solver_envelope::Payload::SolveResponse(response)) = response_envelope.payload else {
        return Err(SolverClientError::Protocol {
            source: WorkerProtocolError::UnexpectedPayload,
            process: Box::new(process),
        });
    };
    validate_response_identity(request, &response).map_err(|source| {
        SolverClientError::Protocol {
            source,
            process: Box::new(process.clone()),
        }
    })?;
    let wire_status =
        SolverStatus::try_from(response.status).map_err(|_| SolverClientError::Protocol {
            source: WorkerProtocolError::UnknownStatus(response.status),
            process: Box::new(process.clone()),
        })?;
    let status = map_status(wire_status).map_err(|source| SolverClientError::Protocol {
        source,
        process: Box::new(process.clone()),
    })?;
    if status == SolverRunStatus::InternalError {
        return Err(SolverClientError::WorkerInternalError {
            detail_code: response.status_detail_code,
            process: Box::new(process),
        });
    }

    Ok(SolverRunOutcome {
        status,
        response: Some(response),
        process,
    })
}

fn validate_response_identity(
    request: &SolverEnvelope,
    response: &SolveResponse,
) -> Result<(), WorkerProtocolError> {
    let Some(solver_envelope::Payload::SolveRequest(solve_request)) = request.payload.as_ref()
    else {
        return Err(WorkerProtocolError::UnexpectedPayload);
    };
    let requested_problem = solve_request
        .problem
        .as_ref()
        .ok_or(ContractError::MissingField("problem"))?;
    if response.input_snapshot_hash != requested_problem.snapshot_hash {
        return Err(WorkerProtocolError::MismatchedSnapshotHash);
    }
    let required_engine = solve_request
        .required_engine_version
        .as_ref()
        .ok_or(ContractError::MissingField("required_engine_version"))?;
    // Match the worker's version requirement: build_revision is provenance, not a
    // compatibility selector (development builds need not have the requester's build ID).
    let engine_matches = response.engine_version.as_ref().is_some_and(|actual| {
        actual.engine_name == required_engine.engine_name
            && actual.engine_version == required_engine.engine_version
            && actual.adapter_version == required_engine.adapter_version
    });
    if engine_matches {
        Ok(())
    } else {
        Err(WorkerProtocolError::MismatchedEngineVersion)
    }
}

fn map_status(status: SolverStatus) -> Result<SolverRunStatus, WorkerProtocolError> {
    match status {
        SolverStatus::Unspecified => Err(WorkerProtocolError::UnspecifiedStatus),
        SolverStatus::Optimal => Ok(SolverRunStatus::Optimal),
        SolverStatus::Feasible => Ok(SolverRunStatus::Feasible),
        SolverStatus::ProvenInfeasible => Ok(SolverRunStatus::ProvenInfeasible),
        SolverStatus::Timeout => Ok(SolverRunStatus::Timeout),
        SolverStatus::Unknown => Ok(SolverRunStatus::Unknown),
        SolverStatus::Cancelled => Ok(SolverRunStatus::Cancelled),
        SolverStatus::InvalidInput => Ok(SolverRunStatus::InvalidInput),
        SolverStatus::InvalidModel => Ok(SolverRunStatus::InvalidModel),
        SolverStatus::InternalError => Ok(SolverRunStatus::InternalError),
    }
}

fn spawn_thread<T, F>(role: &'static str, task: F) -> Result<JoinHandle<T>, SolverClientError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    thread::Builder::new()
        .name(role.into())
        .spawn(task)
        .map_err(|source| SolverClientError::ThreadSpawn { role, source })
}

fn read_single_response(
    mut stdout: impl Read,
    maximum_frame_len: usize,
) -> Result<SolverEnvelope, WorkerProtocolError> {
    let response = framing::read_frame(&mut stdout, maximum_frame_len)?;
    let mut extra = [0_u8; 1];
    loop {
        match stdout.read(&mut extra) {
            Ok(0) => return Ok(response),
            Ok(_) => return Err(WorkerProtocolError::TrailingStdout),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(WorkerProtocolError::Frame(FrameError::Io(error))),
        }
    }
}

fn drain_stderr(mut stderr: impl Read, maximum: usize) -> Result<StderrCapture, io::Error> {
    let mut retained = Vec::with_capacity(maximum.min(8 * 1024));
    let mut total_bytes = 0_usize;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        match stderr.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                total_bytes = total_bytes.saturating_add(count);
                let remaining = maximum.saturating_sub(retained.len());
                let retain_count = remaining.min(count);
                retained.extend_from_slice(&buffer[..retain_count]);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    let captured_bytes = retained.len();
    Ok(StderrCapture {
        text: String::from_utf8_lossy(&retained).into_owned(),
        captured_bytes,
        total_bytes,
        truncated: total_bytes > captured_bytes,
        read_failure: None,
    })
}

fn stderr_capture(joined: thread::Result<Result<StderrCapture, io::Error>>) -> StderrCapture {
    match joined {
        Ok(Ok(capture)) => capture,
        Ok(Err(error)) => StderrCapture {
            read_failure: Some(error.to_string()),
            ..StderrCapture::default()
        },
        Err(_) => StderrCapture {
            read_failure: Some("stderr reader thread panicked".into()),
            ..StderrCapture::default()
        },
    }
}

#[derive(Clone, Copy, Debug)]
enum WaitDisposition {
    Exited(ExitStatus),
    TimedOut(ExitStatus),
    Cancelled(ExitStatus),
}

impl WaitDisposition {
    fn exit_status(self) -> ExitStatus {
        match self {
            Self::Exited(status) | Self::TimedOut(status) | Self::Cancelled(status) => status,
        }
    }
}

fn wait_for_process(
    child: &mut ChildGuard,
    timeout: Duration,
    poll_interval: Duration,
    cancellation: &CancellationToken,
) -> Result<WaitDisposition, SolverClientError> {
    let started = Instant::now();
    loop {
        if cancellation.is_cancelled() {
            return child
                .terminate()
                .map(WaitDisposition::Cancelled)
                .map_err(SolverClientError::ProcessTermination);
        }
        if let Some(status) = child.try_wait().map_err(SolverClientError::ProcessWait)? {
            return Ok(WaitDisposition::Exited(status));
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            return child
                .terminate()
                .map(WaitDisposition::TimedOut)
                .map_err(SolverClientError::ProcessTermination);
        }
        let remaining = timeout.saturating_sub(elapsed);
        let wait = poll_interval.min(remaining);
        cancellation.wait_timeout(wait);
    }
}

#[derive(Debug)]
struct ChildGuard {
    child: Child,
    reaped: bool,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>, io::Error> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    fn terminate(&mut self) -> Result<ExitStatus, io::Error> {
        self.child.kill()?;
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.reaped {
            let _ignored = self.child.kill();
            let _ignored = self.child.wait();
        }
    }
}
