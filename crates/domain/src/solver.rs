use crate::{
    ContentHash, DomainError, DomainProblem, ExternalCode, ObjectiveTier, ScenarioId, SolverRunId,
    SolverSeed, TimetableId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SolveMode {
    Generate,
    Improve,
    Repair,
    Diagnose,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SolverProfile {
    Fast,
    Balanced,
    BestQuality,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Fast,
    Reproducible,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SolverRunStatus {
    Pending,
    Running,
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

impl SolverRunStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Optimal => "optimal",
            Self::Feasible => "feasible",
            Self::ProvenInfeasible => "proven_infeasible",
            Self::Timeout => "timeout",
            Self::Unknown => "unknown",
            Self::Cancelled => "cancelled",
            Self::InvalidInput => "invalid_input",
            Self::InvalidModel => "invalid_model",
            Self::InternalError => "internal_error",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SolverParameters {
    time_limit_ms: u64,
    worker_count: u16,
    execution_mode: ExecutionMode,
}

impl SolverParameters {
    pub fn new(
        time_limit_ms: u64,
        worker_count: u16,
        execution_mode: ExecutionMode,
    ) -> Result<Self, DomainError> {
        if time_limit_ms == 0 {
            return Err(DomainError::ZeroValue {
                field: "solver_parameters.time_limit_ms",
            });
        }
        if worker_count == 0 {
            return Err(DomainError::ZeroValue {
                field: "solver_parameters.worker_count",
            });
        }
        if execution_mode == ExecutionMode::Reproducible && worker_count != 1 {
            return Err(DomainError::ValueOutOfRange {
                field: "solver_parameters.reproducible_worker_count",
                min: 1,
                max: 1,
                actual: u64::from(worker_count),
            });
        }
        Ok(Self {
            time_limit_ms,
            worker_count,
            execution_mode,
        })
    }

    #[must_use]
    pub const fn time_limit_ms(self) -> u64 {
        self.time_limit_ms
    }
    #[must_use]
    pub const fn worker_count(self) -> u16 {
        self.worker_count
    }
    #[must_use]
    pub const fn execution_mode(self) -> ExecutionMode {
        self.execution_mode
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObjectiveValue {
    tier: ObjectiveTier,
    metric: ExternalCode,
    value: i64,
}

impl ObjectiveValue {
    #[must_use]
    pub const fn new(tier: ObjectiveTier, metric: ExternalCode, value: i64) -> Self {
        Self {
            tier,
            metric,
            value,
        }
    }

    #[must_use]
    pub const fn tier(&self) -> ObjectiveTier {
        self.tier
    }
    #[must_use]
    pub const fn metric(&self) -> &ExternalCode {
        &self.metric
    }
    #[must_use]
    pub const fn value(&self) -> i64 {
        self.value
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SolverStatistics {
    pub wall_time_ms: u64,
    pub branches: u64,
    pub conflicts: u64,
    pub solutions_considered: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum SolverOutputValidation {
    NotRun,
    Passed {
        validator_version: ExternalCode,
        output_hash: ContentHash,
    },
    Failed {
        validator_version: ExternalCode,
        problems: Vec<DomainProblem>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SolverRun {
    id: SolverRunId,
    scenario_id: ScenarioId,
    mode: SolveMode,
    profile: SolverProfile,
    seed: SolverSeed,
    parameters: SolverParameters,
    input_snapshot_hash: ContentHash,
    protocol_version: u32,
    engine_version: ExternalCode,
    status: SolverRunStatus,
    timetable_id: Option<TimetableId>,
    objective_values: Vec<ObjectiveValue>,
    statistics: SolverStatistics,
    output_validation: SolverOutputValidation,
}

impl SolverRun {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: SolverRunId,
        scenario_id: ScenarioId,
        mode: SolveMode,
        profile: SolverProfile,
        seed: SolverSeed,
        parameters: SolverParameters,
        input_snapshot_hash: ContentHash,
        protocol_version: u32,
        engine_version: ExternalCode,
    ) -> Result<Self, DomainError> {
        if protocol_version == 0 {
            return Err(DomainError::ZeroValue {
                field: "solver_run.protocol_version",
            });
        }
        Ok(Self {
            id,
            scenario_id,
            mode,
            profile,
            seed,
            parameters,
            input_snapshot_hash,
            protocol_version,
            engine_version,
            status: SolverRunStatus::Pending,
            timetable_id: None,
            objective_values: Vec::new(),
            statistics: SolverStatistics::default(),
            output_validation: SolverOutputValidation::NotRun,
        })
    }

    pub fn start(&mut self) -> Result<(), DomainError> {
        self.transition_to(SolverRunStatus::Running)
    }

    pub fn finish(
        &mut self,
        status: SolverRunStatus,
        timetable_id: Option<TimetableId>,
        objective_values: Vec<ObjectiveValue>,
        statistics: SolverStatistics,
    ) -> Result<(), DomainError> {
        if matches!(status, SolverRunStatus::Optimal | SolverRunStatus::Feasible)
            && timetable_id.is_none()
        {
            return Err(DomainError::InvalidReference {
                field: "solver_run.timetable_id",
                target: "timetable for feasible result",
                value: "none".to_owned(),
            });
        }
        if !matches!(status, SolverRunStatus::Optimal | SolverRunStatus::Feasible)
            && let Some(unexpected_timetable_id) = timetable_id
        {
            return Err(DomainError::InvalidReference {
                field: "solver_run.timetable_id",
                target: "no timetable for unsuccessful result",
                value: unexpected_timetable_id.to_string(),
            });
        }
        require_unique_objectives(&objective_values)?;
        self.transition_to(status)?;
        self.timetable_id = timetable_id;
        self.objective_values = objective_values;
        self.statistics = statistics;
        Ok(())
    }

    pub fn record_output_validation(
        &mut self,
        validation: SolverOutputValidation,
    ) -> Result<(), DomainError> {
        if !matches!(
            self.status,
            SolverRunStatus::Optimal | SolverRunStatus::Feasible
        ) {
            return Err(DomainError::InvalidStateTransition {
                entity: "solver_run.output_validation",
                from: self.status.as_str(),
                to: "validated",
            });
        }
        if matches!(validation, SolverOutputValidation::NotRun) {
            return Err(DomainError::InvalidStateTransition {
                entity: "solver_run.output_validation",
                from: "terminal",
                to: "not_run",
            });
        }
        if matches!(validation, SolverOutputValidation::Failed { .. }) {
            self.status = SolverRunStatus::InternalError;
            self.timetable_id = None;
        }
        self.output_validation = validation;
        Ok(())
    }

    fn transition_to(&mut self, to: SolverRunStatus) -> Result<(), DomainError> {
        let allowed = matches!(
            (self.status, to),
            (SolverRunStatus::Pending, SolverRunStatus::Running)
                | (SolverRunStatus::Pending, SolverRunStatus::Cancelled)
                | (SolverRunStatus::Pending, SolverRunStatus::InvalidInput)
                | (SolverRunStatus::Running, SolverRunStatus::Optimal)
                | (SolverRunStatus::Running, SolverRunStatus::Feasible)
                | (SolverRunStatus::Running, SolverRunStatus::ProvenInfeasible)
                | (SolverRunStatus::Running, SolverRunStatus::Timeout)
                | (SolverRunStatus::Running, SolverRunStatus::Unknown)
                | (SolverRunStatus::Running, SolverRunStatus::Cancelled)
                | (SolverRunStatus::Running, SolverRunStatus::InvalidInput)
                | (SolverRunStatus::Running, SolverRunStatus::InvalidModel)
                | (SolverRunStatus::Running, SolverRunStatus::InternalError)
        );
        if !allowed {
            return Err(DomainError::InvalidStateTransition {
                entity: "solver_run",
                from: self.status.as_str(),
                to: to.as_str(),
            });
        }
        self.status = to;
        Ok(())
    }

    #[must_use]
    pub const fn id(&self) -> SolverRunId {
        self.id
    }
    #[must_use]
    pub const fn scenario_id(&self) -> ScenarioId {
        self.scenario_id
    }
    #[must_use]
    pub const fn mode(&self) -> SolveMode {
        self.mode
    }
    #[must_use]
    pub const fn profile(&self) -> SolverProfile {
        self.profile
    }
    #[must_use]
    pub const fn seed(&self) -> SolverSeed {
        self.seed
    }
    #[must_use]
    pub const fn parameters(&self) -> SolverParameters {
        self.parameters
    }
    #[must_use]
    pub const fn input_snapshot_hash(&self) -> ContentHash {
        self.input_snapshot_hash
    }
    #[must_use]
    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }
    #[must_use]
    pub const fn engine_version(&self) -> &ExternalCode {
        &self.engine_version
    }
    #[must_use]
    pub const fn status(&self) -> SolverRunStatus {
        self.status
    }
    #[must_use]
    pub const fn timetable_id(&self) -> Option<TimetableId> {
        self.timetable_id
    }
    #[must_use]
    pub fn objective_values(&self) -> &[ObjectiveValue] {
        &self.objective_values
    }
    #[must_use]
    pub const fn statistics(&self) -> SolverStatistics {
        self.statistics
    }
    #[must_use]
    pub const fn output_validation(&self) -> &SolverOutputValidation {
        &self.output_validation
    }
}

fn require_unique_objectives(values: &[ObjectiveValue]) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for value in values {
        let key = (value.tier(), value.metric().clone());
        if !seen.insert(key) {
            return Err(DomainError::DuplicateValue {
                field: "solver_run.objective_values",
                value: format!("{}:{}", value.tier().priority(), value.metric()),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProblemCode;

    fn run() -> SolverRun {
        SolverRun::new(
            SolverRunId::new_v4(),
            ScenarioId::new_v4(),
            SolveMode::Generate,
            SolverProfile::Balanced,
            SolverSeed::new(42),
            SolverParameters::new(10_000, 1, ExecutionMode::Reproducible).unwrap(),
            ContentHash::from_bytes([7; 32]),
            1,
            ExternalCode::new("ortools-9.14").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn reproducible_mode_rejects_parallel_workers() {
        let error = SolverParameters::new(1_000, 2, ExecutionMode::Reproducible).unwrap_err();
        assert_eq!(error.code(), ProblemCode::DomainValueOutOfRange);
    }

    #[test]
    fn timeout_and_proven_infeasible_are_distinct_terminal_states() {
        assert_ne!(SolverRunStatus::Timeout, SolverRunStatus::ProvenInfeasible);
        assert_ne!(SolverRunStatus::Unknown, SolverRunStatus::ProvenInfeasible);
        assert!(SolverRunStatus::Timeout.is_terminal());
    }

    #[test]
    fn solver_run_rejects_skipping_running_state() {
        let mut run = run();
        let error = run
            .finish(
                SolverRunStatus::ProvenInfeasible,
                None,
                Vec::new(),
                SolverStatistics::default(),
            )
            .unwrap_err();
        assert_eq!(error.code(), ProblemCode::DomainInvalidStateTransition);
    }
}
