#![forbid(unsafe_code)]
//! Versioned, solver-independent wire contract between Rust and the native worker.
//!
//! The generated protobuf models scheduling concepts only.  Framing is a
//! four-byte, unsigned, big-endian payload length followed by one protobuf
//! `SolverEnvelope`. Worker stdout must contain only these frames.

pub mod framing;
mod validation;

/// Protocol messages generated from `scheduler/v1/solver.proto`.
pub mod scheduler {
    /// Version 1 of the scheduling worker protocol.
    #[allow(clippy::doc_markdown)]
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/scheduler.v1.rs"));
    }
}

pub use scheduler::v1::*;
pub use validation::{ContractError, ValidateContract};

/// Wire protocol version accepted by this crate.
pub const PROTOCOL_VERSION: u32 = 1;
/// Scheduling snapshot schema version accepted by this crate.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
/// Descriptor set for compatibility tooling and cross-language conformance tests.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/scheduler_v1_descriptor.bin"));
