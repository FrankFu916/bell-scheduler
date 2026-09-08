#pragma once

#include <iosfwd>

#include "scheduler/v1/solver.pb.h"

namespace class_schedule::solver {

inline constexpr unsigned int kProtocolVersion = 1U;
inline constexpr unsigned int kSnapshotSchemaVersion = 1U;
inline constexpr char kEngineName[] = "or-tools-cp-sat";
inline constexpr char kEngineVersion[] = "9.15.6755";
inline constexpr char kAdapterVersion[] = "0.1.0";

// Validates and solves one request envelope. Every returned envelope uses the
// worker's protocol version and preserves the request correlation id.
scheduler::v1::SolverEnvelope SolveEnvelope(
    const scheduler::v1::SolverEnvelope& request_envelope);

// Implements the sidecar lifecycle: one framed request, EOF, one framed
// response, then return. Protocol/transport diagnostics are written only to
// the supplied diagnostics stream.
int RunOneShot(std::istream& input, std::ostream& output,
               std::ostream& diagnostics);

}  // namespace class_schedule::solver
