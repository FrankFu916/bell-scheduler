#![forbid(unsafe_code)]
//! Synchronous, one-shot process isolation for the scheduling solver worker.
//!
//! Each call launches a fresh worker, sends exactly one bounded protobuf frame,
//! closes stdin, reads exactly one response frame, and waits for process exit.
//! Request and response payloads are never written to logs by this crate.

mod cancellation;
mod client;

pub use cancellation::CancellationToken;
pub use client::{
    ProcessReport, SidecarSpec, SolverClient, SolverClientError, SolverRunOutcome, SolverRunStatus,
    StderrCapture, WorkerProtocolError,
};
