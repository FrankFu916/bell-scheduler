use std::collections::HashSet;

use thiserror::Error;

use crate::{
    ConstraintSeverity, DiagnosticSignal, EngineVersion, ObjectiveMetricKind, PROTOCOL_VERSION,
    SNAPSHOT_SCHEMA_VERSION, SchedulingProblemSnapshot, SolveMode, SolveRequest, SolveResponse,
    SolverEnvelope, SolverParameters, SolverProfile, SolverStatus, solver_envelope,
};

/// Structural contract failures detected before a request reaches the solver.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("unsupported protocol version {actual}; expected {expected}")]
    UnsupportedProtocolVersion { expected: u32, actual: u32 },
    #[error("unsupported snapshot schema version {actual}; expected {expected}")]
    UnsupportedSnapshotSchemaVersion { expected: u32, actual: u32 },
    #[error("required field `{0}` is missing")]
    MissingField(&'static str),
    #[error("field `{0}` must not be empty")]
    EmptyField(&'static str),
    #[error("field `{field}` has invalid length {actual}; expected {expected}")]
    InvalidLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("field `{field}` contains unknown enum value {value}")]
    UnknownEnum { field: &'static str, value: i32 },
    #[error("field `{0}` must not use the UNSPECIFIED enum value")]
    UnspecifiedEnum(&'static str),
    #[error("field `{0}` must be greater than zero")]
    MustBePositive(&'static str),
    #[error("field `{0}` contains a duplicate identifier")]
    DuplicateIdentifier(&'static str),
    #[error("reproducible solver runs require exactly one worker")]
    ReproducibleWorkerCount,
    #[error("non-success response status must not carry timetable assignments")]
    AssignmentsForNonSuccessStatus,
    #[error("response statistics do not echo the effective seed or worker count")]
    InconsistentStatistics,
}

/// Validates semantic invariants that protobuf decoding alone cannot enforce.
pub trait ValidateContract {
    /// Check protocol versions, required fields, enum values, and cross-field invariants.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError`] at the first invalid contract invariant.
    fn validate_contract(&self) -> Result<(), ContractError>;
}

impl ValidateContract for SolverEnvelope {
    fn validate_contract(&self) -> Result<(), ContractError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ContractError::UnsupportedProtocolVersion {
                expected: PROTOCOL_VERSION,
                actual: self.protocol_version,
            });
        }
        require_text(&self.request_id, "request_id")?;
        if self.request_id.len() > 128 {
            return Err(ContractError::InvalidLength {
                field: "request_id",
                expected: 128,
                actual: self.request_id.len(),
            });
        }
        match self.payload.as_ref() {
            Some(solver_envelope::Payload::SolveRequest(request)) => request.validate_contract(),
            Some(solver_envelope::Payload::SolveResponse(response)) => response.validate_contract(),
            None => Err(ContractError::MissingField("payload")),
        }
    }
}

impl ValidateContract for SolveRequest {
    fn validate_contract(&self) -> Result<(), ContractError> {
        self.required_engine_version
            .as_ref()
            .ok_or(ContractError::MissingField("required_engine_version"))?
            .validate_contract()?;
        let parameters = self
            .parameters
            .as_ref()
            .ok_or(ContractError::MissingField("parameters"))?;
        parameters.validate_contract()?;
        let problem = self
            .problem
            .as_ref()
            .ok_or(ContractError::MissingField("problem"))?;
        problem.validate_contract()?;

        let mode = enum_value::<SolveMode>(parameters.mode, "parameters.mode")?;
        if matches!(mode, SolveMode::Improve | SolveMode::Repair)
            && problem.incumbent_assignments.is_empty()
        {
            return Err(ContractError::MissingField("problem.incumbent_assignments"));
        }
        Ok(())
    }
}

impl ValidateContract for SolveResponse {
    fn validate_contract(&self) -> Result<(), ContractError> {
        self.engine_version
            .as_ref()
            .ok_or(ContractError::MissingField("engine_version"))?
            .validate_contract()?;
        let parameters = self
            .effective_parameters
            .as_ref()
            .ok_or(ContractError::MissingField("effective_parameters"))?;
        parameters.validate_contract()?;
        require_hash(&self.input_snapshot_hash, "input_snapshot_hash")?;

        let status = enum_value::<SolverStatus>(self.status, "status")?;
        let success = matches!(status, SolverStatus::Optimal | SolverStatus::Feasible);
        if !success && (!self.assignments.is_empty() || !self.section_room_assignments.is_empty()) {
            return Err(ContractError::AssignmentsForNonSuccessStatus);
        }

        if let Some(statistics) = &self.statistics
            && (statistics.seed != parameters.seed
                || statistics.worker_count != parameters.worker_count)
        {
            return Err(ContractError::InconsistentStatistics);
        }
        for group in &self.diagnostic_groups {
            enum_value::<DiagnosticSignal>(group.signal, "diagnostic_groups.signal")?;
            require_text(&group.problem_code, "diagnostic_groups.problem_code")?;
        }
        if let Some(objective) = &self.objective {
            for tier in &objective.tiers {
                require_text(&tier.tier_id, "objective.tiers.tier_id")?;
                for metric in &tier.metrics {
                    enum_value::<ObjectiveMetricKind>(
                        metric.metric_kind,
                        "objective.tiers.metrics.metric_kind",
                    )?;
                }
            }
        }
        Ok(())
    }
}

impl ValidateContract for EngineVersion {
    fn validate_contract(&self) -> Result<(), ContractError> {
        require_text(&self.engine_name, "engine_version.engine_name")?;
        require_text(&self.engine_version, "engine_version.engine_version")?;
        require_text(&self.adapter_version, "engine_version.adapter_version")
    }
}

impl ValidateContract for SolverParameters {
    fn validate_contract(&self) -> Result<(), ContractError> {
        enum_value::<SolveMode>(self.mode, "parameters.mode")?;
        enum_value::<SolverProfile>(self.profile, "parameters.profile")?;
        positive(self.time_limit_millis, "parameters.time_limit_millis")?;
        positive(self.worker_count, "parameters.worker_count")?;
        if self.reproducible && self.worker_count != 1 {
            return Err(ContractError::ReproducibleWorkerCount);
        }
        Ok(())
    }
}

impl ValidateContract for SchedulingProblemSnapshot {
    fn validate_contract(&self) -> Result<(), ContractError> {
        if self.schema_version != SNAPSHOT_SCHEMA_VERSION {
            return Err(ContractError::UnsupportedSnapshotSchemaVersion {
                expected: SNAPSHOT_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        require_hash(&self.snapshot_hash, "problem.snapshot_hash")?;
        require_text(&self.project_id, "problem.project_id")?;

        validate_snapshot_ids(self)?;

        for activity in &self.activities {
            positive(
                activity.duration_periods,
                "problem.activities.duration_periods",
            )?;
            if activity.allowed_start_timeslot_ids.is_empty() {
                return Err(ContractError::EmptyField(
                    "problem.activities.allowed_start_timeslot_ids",
                ));
            }
            let teacher_policy =
                activity
                    .teacher_policy
                    .as_ref()
                    .ok_or(ContractError::MissingField(
                        "problem.activities.teacher_policy",
                    ))?;
            if teacher_policy.policy.is_none() {
                return Err(ContractError::MissingField(
                    "problem.activities.teacher_policy.policy",
                ));
            }
            positive(
                activity.section_room_binding_id,
                "problem.activities.section_room_binding_id",
            )?;
            positive(
                activity.meeting_pattern_id,
                "problem.activities.meeting_pattern_id",
            )?;
            positive(
                activity.teacher_binding_id,
                "problem.activities.teacher_binding_id",
            )?;
        }
        for binding in &self.section_room_bindings {
            if binding
                .policy
                .as_ref()
                .and_then(|policy| policy.policy.as_ref())
                .is_none()
            {
                return Err(ContractError::MissingField(
                    "problem.section_room_bindings.policy",
                ));
            }
        }
        for group in &self.constraint_groups {
            require_text(&group.group_id, "problem.constraint_groups.group_id")?;
            require_text(
                &group.problem_code,
                "problem.constraint_groups.problem_code",
            )?;
            enum_value::<ConstraintSeverity>(group.severity, "problem.constraint_groups.severity")?;
        }
        for tier in &self.objective_tiers {
            require_text(&tier.tier_id, "problem.objective_tiers.tier_id")?;
            for metric in &tier.metrics {
                enum_value::<ObjectiveMetricKind>(
                    metric.metric_kind,
                    "problem.objective_tiers.metrics.metric_kind",
                )?;
                positive(
                    metric.weight_within_tier,
                    "problem.objective_tiers.metrics.weight_within_tier",
                )?;
            }
        }
        Ok(())
    }
}

fn validate_snapshot_ids(snapshot: &SchedulingProblemSnapshot) -> Result<(), ContractError> {
    unique_positive(
        snapshot.timeslots.iter().map(|item| item.timeslot_id),
        "problem.timeslots.timeslot_id",
    )?;
    unique_positive(
        snapshot.rooms.iter().map(|item| item.room_id),
        "problem.rooms.room_id",
    )?;
    unique_positive(
        snapshot.teachers.iter().map(|item| item.teacher_id),
        "problem.teachers.teacher_id",
    )?;
    unique_positive(
        snapshot.activities.iter().map(|item| item.activity_id),
        "problem.activities.activity_id",
    )?;
    unique_positive(
        snapshot
            .section_room_bindings
            .iter()
            .map(|item| item.binding_id),
        "problem.section_room_bindings.binding_id",
    )?;
    unique_positive(
        snapshot
            .meeting_patterns
            .iter()
            .map(|item| item.meeting_pattern_id),
        "problem.meeting_patterns.meeting_pattern_id",
    )
}

fn require_text(value: &str, field: &'static str) -> Result<(), ContractError> {
    if value.trim().is_empty() {
        Err(ContractError::EmptyField(field))
    } else {
        Ok(())
    }
}

fn require_hash(value: &[u8], field: &'static str) -> Result<(), ContractError> {
    if value.len() == 32 {
        Ok(())
    } else {
        Err(ContractError::InvalidLength {
            field,
            expected: 32,
            actual: value.len(),
        })
    }
}

fn positive<T>(value: T, field: &'static str) -> Result<(), ContractError>
where
    T: Copy + PartialEq + From<u8>,
{
    if value == T::from(0) {
        Err(ContractError::MustBePositive(field))
    } else {
        Ok(())
    }
}

fn unique_positive(
    values: impl IntoIterator<Item = u32>,
    field: &'static str,
) -> Result<(), ContractError> {
    let mut seen = HashSet::new();
    for value in values {
        positive(value, field)?;
        if !seen.insert(value) {
            return Err(ContractError::DuplicateIdentifier(field));
        }
    }
    Ok(())
}

fn enum_value<E>(value: i32, field: &'static str) -> Result<E, ContractError>
where
    E: TryFrom<i32> + PartialEq + Default,
{
    let parsed = E::try_from(value).map_err(|_| ContractError::UnknownEnum { field, value })?;
    if parsed == E::default() {
        Err(ContractError::UnspecifiedEnum(field))
    } else {
        Ok(parsed)
    }
}
