use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use class_schedule_scheduling::{
    Assignment, RoomRequirement as SemanticRoomRequirement,
    SchedulingProblemSnapshot as SemanticSnapshot,
    TeacherRequirement as SemanticTeacherRequirement,
};
use class_schedule_validation::{ValidationReport, static_feasibility_check, validate_assignments};
use solver_client::{
    CancellationToken, ProcessReport, SolverClient, SolverClientError, SolverRunOutcome,
    SolverRunStatus,
};
use solver_contract::{
    Activity, ActivityConflictClique, AdminHomeRoom, CalendarTimeslot, CandidateTeachers,
    ConstraintGroup, ConstraintSeverity, EngineVersion, FixedRoom, FixedTeacher, FlexibleRooms,
    LockedAssignment, MeetingAssignment, MeetingPattern, ObjectiveMetricDefinition,
    ObjectiveMetricKind, ObjectiveTierDefinition, PreferredFixedRooms, ResourceConflictGroup,
    ResourceKind, Room, RoomPolicy, SchedulingProblemSnapshot, SectionFixedRooms,
    SectionRoomBinding, SolveMode, SolveRequest, SolveResponse, SolverEnvelope, SolverParameters,
    SolverProfile, StudentConflictGraph, Teacher, TeacherAssignmentPolicy, WeekPattern,
    room_policy, solver_envelope, teacher_assignment_policy,
};
use thiserror::Error;

const ENGINE_NAME: &str = "or-tools-cp-sat";
const ENGINE_VERSION: &str = "9.15.6755";
const ADAPTER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SolveContext {
    pub project_id: String,
    pub project_revision: u64,
    pub scenario_id: String,
    pub scenario_revision: u64,
    pub request_id: String,
}

impl SolveContext {
    #[must_use]
    pub fn with_generated_request_id(
        project_id: impl Into<String>,
        project_revision: u64,
        scenario_id: impl Into<String>,
        scenario_revision: u64,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            project_revision,
            scenario_id: scenario_id.into(),
            scenario_revision,
            request_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SolveOptions {
    pub mode: SolveMode,
    pub profile: SolverProfile,
    pub seed: u64,
    pub reproducible: bool,
    pub time_limit: Duration,
    pub worker_count: u32,
    pub collect_diagnostics: bool,
    pub memory_limit_bytes: u64,
    pub relative_gap_limit_ppm: u32,
    pub objective_tiers: Vec<ObjectiveTierDefinition>,
    /// Complete, independently valid baseline required by Improve and Repair.
    ///
    /// Generate and Diagnose reject a baseline so that solve-mode state cannot be
    /// accidentally confused at the application boundary.
    pub incumbent_assignments: Vec<Assignment>,
}

impl SolveOptions {
    #[must_use]
    pub fn reproducible(seed: u64, time_limit: Duration) -> Self {
        Self {
            mode: SolveMode::Generate,
            profile: SolverProfile::Balanced,
            seed,
            reproducible: true,
            time_limit,
            worker_count: 1,
            collect_diagnostics: true,
            memory_limit_bytes: 0,
            relative_gap_limit_ppm: 0,
            objective_tiers: vec![ObjectiveTierDefinition {
                tier_id: "course_distribution".to_owned(),
                priority: 1,
                metrics: vec![ObjectiveMetricDefinition {
                    metric_kind: ObjectiveMetricKind::CourseDistribution as i32,
                    weight_within_tier: 1,
                    scope: Vec::new(),
                    parameters: HashMap::new(),
                }],
            }],
            incumbent_assignments: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PreparedSolve {
    pub envelope: SolverEnvelope,
    pub snapshot_hash: [u8; 32],
}

#[derive(Debug)]
pub enum SolveExecution {
    PrecheckFailed { report: ValidationReport },
    Completed(Box<CompletedSolve>),
}

#[derive(Debug)]
pub struct CompletedSolve {
    pub status: SolverRunStatus,
    pub assignments: Vec<Assignment>,
    pub independent_validation: Option<ValidationReport>,
    pub response: Option<Box<SolveResponse>>,
    pub process: ProcessReport,
    pub snapshot_hash: [u8; 32],
    pub output_hash: Option<[u8; 32]>,
    /// Actual independent Hard-validation time; absent when no assignment was validated.
    pub validation_elapsed: Option<Duration>,
}

#[derive(Debug, Error)]
pub enum SolveApplicationError {
    #[error("semantic snapshot could not be canonicalized: {0}")]
    SnapshotSerialization(#[from] serde_json::Error),
    #[error("solver adapter could not represent the snapshot: {code}: {detail}")]
    Adapter { code: &'static str, detail: String },
    #[error("solver client failed: {0}")]
    Client(#[from] SolverClientError),
    #[error("solver reported success without a response payload")]
    MissingResponse,
    #[error("solver output mapping failed: {code}: {detail}")]
    InvalidOutputMapping { code: &'static str, detail: String },
    #[error("solver output failed independent hard validation")]
    InvalidSolverOutput { report: ValidationReport },
    #[error("solve-mode incumbent failed independent hard validation")]
    InvalidIncumbent { report: ValidationReport },
}

#[derive(Debug)]
struct CompactCatalogs {
    features: BTreeMap<String, u32>,
    buildings: BTreeMap<class_schedule_domain::BuildingId, u32>,
    subjects: BTreeMap<class_schedule_domain::SubjectId, u32>,
    sections: BTreeMap<class_schedule_domain::TeachingSectionId, u32>,
    administrative_classes: BTreeMap<class_schedule_domain::AdministrativeClassId, u32>,
}

impl CompactCatalogs {
    fn collect(problem: &SemanticSnapshot) -> Result<Self, SolveApplicationError> {
        Ok(Self {
            features: collect_feature_ids(problem)?,
            buildings: collect_building_ids(problem)?,
            subjects: collect_subject_ids(problem)?,
            sections: collect_section_ids(problem)?,
            administrative_classes: collect_admin_class_ids(problem)?,
        })
    }
}

impl SolveApplicationError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SnapshotSerialization(_) => "APPLICATION_SNAPSHOT_SERIALIZATION",
            Self::Adapter { code, .. } | Self::InvalidOutputMapping { code, .. } => code,
            Self::Client(_) => "APPLICATION_SOLVER_CLIENT",
            Self::MissingResponse => "APPLICATION_SOLVER_MISSING_RESPONSE",
            Self::InvalidSolverOutput { .. } => "INTERNAL_ERROR_INVALID_SOLVER_OUTPUT",
            Self::InvalidIncumbent { .. } => "APPLICATION_INVALID_INCUMBENT",
        }
    }
}

/// Runs pre-check, isolated worker solve, mapping, and independent hard validation.
///
/// # Errors
///
/// Returns a structured error for adapter/client failures or for solver output rejected by the
/// independent validator. A rejected output is never returned as a successful timetable.
pub fn execute_solve(
    problem: &SemanticSnapshot,
    context: &SolveContext,
    options: &SolveOptions,
    client: &SolverClient,
    cancellation: &CancellationToken,
) -> Result<SolveExecution, SolveApplicationError> {
    let precheck = static_feasibility_check(problem);
    if !precheck.is_valid() {
        return Ok(SolveExecution::PrecheckFailed { report: precheck });
    }
    let prepared = prepare_solve(problem, context, options)?;
    let snapshot_hash = prepared.snapshot_hash;
    let outcome = client.solve(&prepared.envelope, cancellation)?;
    complete_solve(problem, snapshot_hash, outcome)
}

pub(crate) fn complete_solve(
    problem: &SemanticSnapshot,
    snapshot_hash: [u8; 32],
    outcome: SolverRunOutcome,
) -> Result<SolveExecution, SolveApplicationError> {
    let status = outcome.status;
    let mut assignments = Vec::new();
    let mut validation = None;
    let mut output_hash = None;
    let mut validation_elapsed = None;
    if matches!(status, SolverRunStatus::Optimal | SolverRunStatus::Feasible) {
        let response = outcome
            .response
            .as_ref()
            .ok_or(SolveApplicationError::MissingResponse)?;
        assignments = map_assignments(problem, response)?;
        let validation_started = Instant::now();
        let report = validate_assignments(problem, &assignments);
        validation_elapsed = Some(validation_started.elapsed());
        if !report.is_valid() {
            return Err(SolveApplicationError::InvalidSolverOutput { report });
        }
        output_hash = Some(hash_assignments(&assignments)?);
        validation = Some(report);
    }
    Ok(SolveExecution::Completed(Box::new(CompletedSolve {
        status,
        assignments,
        independent_validation: validation,
        response: outcome.response.map(Box::new),
        process: outcome.process,
        snapshot_hash,
        output_hash,
        validation_elapsed,
    })))
}

/// Converts the internal semantic snapshot into the versioned worker contract.
///
/// # Errors
///
/// Returns an adapter error if a compact index overflows, metadata is blank, or the time limit
/// cannot be represented in milliseconds.
pub fn prepare_solve(
    problem: &SemanticSnapshot,
    context: &SolveContext,
    options: &SolveOptions,
) -> Result<PreparedSolve, SolveApplicationError> {
    validate_context_and_options(context, options)?;
    validate_incumbent(problem, options)?;
    let snapshot_bytes = serde_json::to_vec(problem)?;
    let snapshot_hash = *blake3::hash(&snapshot_bytes).as_bytes();
    let catalogs = CompactCatalogs::collect(problem)?;
    let binding_plan = build_room_bindings(problem, &catalogs.features, &catalogs.sections)?;
    let pattern_plan = build_meeting_patterns(problem)?;
    let teacher_binding_plan = build_teacher_bindings(problem)?;
    let resource_conflicts = build_resource_conflicts(problem)?;
    let (locks, constraint_groups) = build_locks(problem)?;
    let protocol_problem = SchedulingProblemSnapshot {
        schema_version: solver_contract::SNAPSHOT_SCHEMA_VERSION,
        snapshot_hash: snapshot_hash.to_vec(),
        project_id: context.project_id.clone(),
        project_revision: context.project_revision,
        scenario_id: context.scenario_id.clone(),
        scenario_revision: context.scenario_revision,
        week_patterns: vec![WeekPattern {
            week_pattern_id: 1,
            stable_key: "all_weeks".to_owned(),
            teaching_week_numbers: vec![1],
        }],
        timeslots: map_timeslots(problem)?,
        rooms: map_rooms(problem, &catalogs)?,
        activities: map_activities(
            problem,
            &catalogs,
            &binding_plan,
            &pattern_plan,
            &teacher_binding_plan,
        )?,
        student_conflicts: Some(map_student_conflicts(problem)?),
        resource_conflicts,
        section_room_bindings: binding_plan.bindings,
        meeting_patterns: pattern_plan.patterns,
        locks,
        incumbent_assignments: map_incumbent_assignments(problem, &options.incumbent_assignments)?,
        constraint_groups,
        objective_tiers: options.objective_tiers.clone(),
        teachers: map_teachers(problem)?,
    };
    let time_limit_millis = u64::try_from(options.time_limit.as_millis()).map_err(|_| {
        SolveApplicationError::Adapter {
            code: "APPLICATION_TIME_LIMIT_OVERFLOW",
            detail: format!("{} ms", options.time_limit.as_millis()),
        }
    })?;
    let parameters = SolverParameters {
        seed: options.seed,
        mode: options.mode as i32,
        profile: options.profile as i32,
        reproducible: options.reproducible,
        time_limit_millis,
        worker_count: options.worker_count,
        collect_diagnostics: options.collect_diagnostics,
        memory_limit_bytes: options.memory_limit_bytes,
        relative_gap_limit_ppm: options.relative_gap_limit_ppm,
    };
    let envelope = SolverEnvelope {
        protocol_version: solver_contract::PROTOCOL_VERSION,
        request_id: context.request_id.clone(),
        payload: Some(solver_envelope::Payload::SolveRequest(SolveRequest {
            required_engine_version: Some(EngineVersion {
                engine_name: ENGINE_NAME.to_owned(),
                engine_version: ENGINE_VERSION.to_owned(),
                adapter_version: ADAPTER_VERSION.to_owned(),
                build_revision: env!("CARGO_PKG_VERSION").to_owned(),
            }),
            parameters: Some(parameters),
            problem: Some(protocol_problem),
        })),
    };
    Ok(PreparedSolve {
        envelope,
        snapshot_hash,
    })
}

fn map_timeslots(
    problem: &SemanticSnapshot,
) -> Result<Vec<CalendarTimeslot>, SolveApplicationError> {
    problem
        .timeslots()
        .iter()
        .enumerate()
        .map(|(index, slot)| {
            Ok(CalendarTimeslot {
                timeslot_id: positive_index(index, "timeslot")?,
                week_pattern_id: 1,
                day_index: u32::from(day_number(slot.day)),
                period_index: u32::from(slot.period_index),
                next_consecutive_timeslot_id: slot
                    .next_consecutive
                    .map(|next| positive_index(next.as_usize(), "next_timeslot"))
                    .transpose()?,
            })
        })
        .collect()
}

fn map_rooms(
    problem: &SemanticSnapshot,
    catalogs: &CompactCatalogs,
) -> Result<Vec<Room>, SolveApplicationError> {
    problem
        .rooms()
        .iter()
        .enumerate()
        .map(|(index, room)| {
            Ok(Room {
                room_id: positive_index(index, "room")?,
                capacity: u32::from(room.capacity),
                feature_ids: room
                    .features
                    .iter()
                    .map(|feature| catalogs.features[feature])
                    .collect(),
                building_id: catalogs.buildings[&room.building_id],
                available_timeslot_ids: room
                    .available
                    .indices()
                    .map(|slot| positive_index(slot, "room_available_timeslot"))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        })
        .collect()
}

fn map_teachers(problem: &SemanticSnapshot) -> Result<Vec<Teacher>, SolveApplicationError> {
    problem
        .teachers()
        .iter()
        .enumerate()
        .map(|(index, teacher)| {
            Ok(Teacher {
                teacher_id: positive_index(index, "teacher")?,
                available_timeslot_ids: teacher
                    .available
                    .indices()
                    .map(|slot| positive_index(slot, "teacher_available_timeslot"))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        })
        .collect()
}

fn map_activities(
    problem: &SemanticSnapshot,
    catalogs: &CompactCatalogs,
    binding_plan: &BindingPlan,
    pattern_plan: &PatternPlan,
    teacher_binding_plan: &TeacherBindingPlan,
) -> Result<Vec<Activity>, SolveApplicationError> {
    problem
        .activities()
        .iter()
        .enumerate()
        .map(|(index, activity)| {
            let teacher_policy = map_teacher_policy(&activity.teacher)?;
            Ok(Activity {
                activity_id: positive_index(index, "activity")?,
                meeting_demand_id: positive_index(index, "meeting_demand")?,
                subject_id: catalogs.subjects[&activity.subject_id],
                section_id: activity
                    .teaching_section_id
                    .map(|id| catalogs.sections[&id]),
                administrative_class_id: activity
                    .administrative_class_id
                    .map(|id| catalogs.administrative_classes[&id]),
                duration_periods: u32::from(activity.duration_periods),
                allowed_start_timeslot_ids: activity
                    .allowed_starts
                    .iter()
                    .map(|slot| positive_index(slot.as_usize(), "allowed_start"))
                    .collect::<Result<Vec<_>, _>>()?,
                teacher_policy: Some(teacher_policy),
                section_room_binding_id: binding_plan.activity_binding_ids[index],
                meeting_pattern_id: pattern_plan.activity_pattern_ids[index],
                constraint_group_ids: Vec::new(),
                teacher_binding_id: teacher_binding_plan.activity_binding_ids[index],
            })
        })
        .collect()
}

fn map_teacher_policy(
    requirement: &SemanticTeacherRequirement,
) -> Result<TeacherAssignmentPolicy, SolveApplicationError> {
    let policy = match requirement {
        SemanticTeacherRequirement::Fixed { teacher } => {
            teacher_assignment_policy::Policy::FixedTeacher(FixedTeacher {
                teacher_id: positive_index(teacher.as_usize(), "teacher")?,
            })
        }
        SemanticTeacherRequirement::Candidates { teachers } => {
            teacher_assignment_policy::Policy::CandidateTeachers(CandidateTeachers {
                teacher_ids: teachers
                    .iter()
                    .map(|teacher| positive_index(teacher.as_usize(), "teacher_candidate"))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        }
    };
    Ok(TeacherAssignmentPolicy {
        policy: Some(policy),
    })
}

fn map_student_conflicts(
    problem: &SemanticSnapshot,
) -> Result<StudentConflictGraph, SolveApplicationError> {
    let mut unique_cliques = BTreeSet::new();
    for student in 0..problem.students().len() {
        let activity_ids = problem
            .activities()
            .iter()
            .enumerate()
            .filter(|(_index, activity)| activity.audience.contains(student))
            .map(|(index, _activity)| positive_index(index, "conflict_clique_activity"))
            .collect::<Result<Vec<_>, _>>()?;
        if activity_ids.len() >= 2 {
            unique_cliques.insert(activity_ids);
        }
    }
    Ok(StudentConflictGraph {
        edges: Vec::new(),
        cliques: unique_cliques
            .into_iter()
            .map(|activity_ids| ActivityConflictClique { activity_ids })
            .collect(),
    })
}

pub(crate) fn validate_context_and_options(
    context: &SolveContext,
    options: &SolveOptions,
) -> Result<(), SolveApplicationError> {
    for (field, value) in [
        ("project_id", context.project_id.as_str()),
        ("scenario_id", context.scenario_id.as_str()),
        ("request_id", context.request_id.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(SolveApplicationError::Adapter {
                code: "APPLICATION_EMPTY_SOLVE_CONTEXT",
                detail: field.to_owned(),
            });
        }
    }
    if options.time_limit.is_zero() || options.worker_count == 0 {
        return Err(SolveApplicationError::Adapter {
            code: "APPLICATION_INVALID_SOLVER_PARAMETERS",
            detail: "time limit and worker count must be positive".to_owned(),
        });
    }
    if options.reproducible && options.worker_count != 1 {
        return Err(SolveApplicationError::Adapter {
            code: "APPLICATION_REPRODUCIBLE_REQUIRES_ONE_WORKER",
            detail: options.worker_count.to_string(),
        });
    }
    match options.mode {
        SolveMode::Improve | SolveMode::Repair if options.incumbent_assignments.is_empty() => {
            return Err(SolveApplicationError::Adapter {
                code: "APPLICATION_INCUMBENT_REQUIRED_FOR_MODE",
                detail: format!("{:?}", options.mode),
            });
        }
        SolveMode::Generate | SolveMode::Diagnose if !options.incumbent_assignments.is_empty() => {
            return Err(SolveApplicationError::Adapter {
                code: "APPLICATION_INCUMBENT_NOT_ALLOWED_FOR_MODE",
                detail: format!("{:?}", options.mode),
            });
        }
        SolveMode::Unspecified => {
            return Err(SolveApplicationError::Adapter {
                code: "APPLICATION_SOLVE_MODE_UNSPECIFIED",
                detail: "solve mode must be explicit".to_owned(),
            });
        }
        _ => {}
    }
    Ok(())
}

fn validate_incumbent(
    problem: &SemanticSnapshot,
    options: &SolveOptions,
) -> Result<(), SolveApplicationError> {
    if options.incumbent_assignments.is_empty() {
        return Ok(());
    }
    let report = validate_assignments(problem, &options.incumbent_assignments);
    if report.is_valid() {
        Ok(())
    } else {
        Err(SolveApplicationError::InvalidIncumbent { report })
    }
}

#[derive(Debug)]
struct BindingPlan {
    bindings: Vec<SectionRoomBinding>,
    activity_binding_ids: Vec<u32>,
}

#[derive(Debug)]
struct BindingAccumulator {
    section_id: Option<u32>,
    candidates: BTreeSet<u32>,
    preferred: BTreeSet<u32>,
    fixed_kind: FixedBindingKind,
    required_capacity: u32,
    required_features: BTreeSet<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixedBindingKind {
    Admin,
    Fixed,
    Section,
    Preferred,
    Flexible,
}

// One pass intentionally performs intersection and requirement aggregation atomically.
#[allow(clippy::too_many_lines)]
fn build_room_bindings(
    problem: &SemanticSnapshot,
    feature_ids: &BTreeMap<String, u32>,
    section_ids: &BTreeMap<class_schedule_domain::TeachingSectionId, u32>,
) -> Result<BindingPlan, SolveApplicationError> {
    let mut keys = Vec::with_capacity(problem.activities().len());
    let mut accumulators: BTreeMap<String, BindingAccumulator> = BTreeMap::new();
    for (index, activity) in problem.activities().iter().enumerate() {
        let (key, section_id, candidates, preferred, fixed_kind) = match &activity.room {
            SemanticRoomRequirement::AdminHomeRoom { room } => (
                format!("activity:{index}:admin"),
                None,
                BTreeSet::from([positive_index(room.as_usize(), "room")?]),
                BTreeSet::new(),
                FixedBindingKind::Admin,
            ),
            SemanticRoomRequirement::Fixed { room } => (
                format!("activity:{index}:fixed"),
                None,
                BTreeSet::from([positive_index(room.as_usize(), "room")?]),
                BTreeSet::new(),
                FixedBindingKind::Fixed,
            ),
            SemanticRoomRequirement::SectionFixed {
                section_id,
                candidate_rooms,
            } => (
                format!("section:{section_id}"),
                Some(section_ids[section_id]),
                room_index_set(candidate_rooms, "section_room")?,
                BTreeSet::new(),
                FixedBindingKind::Section,
            ),
            SemanticRoomRequirement::PreferredFixed {
                section_id,
                preferred_rooms,
                fallback_rooms,
            } => {
                let preferred = room_index_set(preferred_rooms, "preferred_room")?;
                let mut all = preferred.clone();
                all.extend(room_index_set(fallback_rooms, "fallback_room")?);
                (
                    format!("section:{section_id}"),
                    Some(section_ids[section_id]),
                    all,
                    preferred,
                    FixedBindingKind::Preferred,
                )
            }
            SemanticRoomRequirement::Flexible { candidate_rooms } => (
                format!("activity:{index}:flexible"),
                None,
                room_index_set(candidate_rooms, "flexible_room")?,
                BTreeSet::new(),
                FixedBindingKind::Flexible,
            ),
        };
        keys.push(key.clone());
        let required_features = activity
            .required_room_features
            .iter()
            .map(|feature| feature_ids[feature])
            .collect::<BTreeSet<_>>();
        accumulators
            .entry(key)
            .and_modify(|accumulator| {
                accumulator
                    .candidates
                    .retain(|room| candidates.contains(room));
                accumulator
                    .preferred
                    .retain(|room| preferred.contains(room));
                accumulator.required_capacity = accumulator.required_capacity.max(
                    u32::try_from(
                        activity
                            .audience
                            .count()
                            .max(usize::from(activity.required_capacity)),
                    )
                    .expect("student audience uses compact u32 indices"),
                );
                accumulator.required_features.extend(&required_features);
                if accumulator.fixed_kind != fixed_kind {
                    accumulator.fixed_kind = FixedBindingKind::Section;
                }
            })
            .or_insert(BindingAccumulator {
                section_id,
                candidates,
                preferred,
                fixed_kind,
                required_capacity: u32::try_from(
                    activity
                        .audience
                        .count()
                        .max(usize::from(activity.required_capacity)),
                )
                .expect("student audience uses compact u32 indices"),
                required_features,
            });
    }
    let key_to_id = accumulators
        .keys()
        .enumerate()
        .map(|(index, key)| Ok((key.clone(), positive_index(index, "room_binding")?)))
        .collect::<Result<BTreeMap<_, _>, SolveApplicationError>>()?;
    let bindings = accumulators
        .into_iter()
        .map(|(key, accumulator)| {
            let candidate_vec = accumulator.candidates.iter().copied().collect::<Vec<_>>();
            let policy = match accumulator.fixed_kind {
                FixedBindingKind::Admin => room_policy::Policy::AdminHomeRoom(AdminHomeRoom {
                    room_id: candidate_vec.first().copied().unwrap_or_default(),
                }),
                FixedBindingKind::Fixed => room_policy::Policy::Fixed(FixedRoom {
                    room_id: candidate_vec.first().copied().unwrap_or_default(),
                }),
                FixedBindingKind::Section => room_policy::Policy::SectionFixed(SectionFixedRooms {
                    candidate_room_ids: candidate_vec,
                }),
                FixedBindingKind::Preferred => {
                    let preferred = accumulator.preferred;
                    room_policy::Policy::PreferredFixed(PreferredFixedRooms {
                        preferred_room_ids: preferred.iter().copied().collect(),
                        fallback_room_ids: accumulator
                            .candidates
                            .difference(&preferred)
                            .copied()
                            .collect(),
                    })
                }
                FixedBindingKind::Flexible => room_policy::Policy::Flexible(FlexibleRooms {
                    candidate_room_ids: candidate_vec,
                }),
            };
            Ok(SectionRoomBinding {
                binding_id: key_to_id[&key],
                section_id: accumulator.section_id,
                policy: Some(RoomPolicy {
                    policy: Some(policy),
                }),
                required_capacity: accumulator.required_capacity,
                required_feature_ids: accumulator.required_features.into_iter().collect(),
            })
        })
        .collect::<Result<Vec<_>, SolveApplicationError>>()?;
    Ok(BindingPlan {
        bindings,
        activity_binding_ids: keys.iter().map(|key| key_to_id[key]).collect(),
    })
}

#[derive(Debug)]
struct PatternPlan {
    patterns: Vec<MeetingPattern>,
    activity_pattern_ids: Vec<u32>,
}

fn build_meeting_patterns(
    problem: &SemanticSnapshot,
) -> Result<PatternPlan, SolveApplicationError> {
    let explicit: HashMap<_, _> = problem
        .meeting_patterns()
        .iter()
        .map(|rule| (rule.course_offering_id, rule))
        .collect();
    let mut activities_by_offering: BTreeMap<_, Vec<usize>> = BTreeMap::new();
    for (index, activity) in problem.activities().iter().enumerate() {
        activities_by_offering
            .entry(activity.course_offering_id)
            .or_default()
            .push(index);
    }
    let mut activity_pattern_ids = vec![0; problem.activities().len()];
    let mut patterns = Vec::new();
    for (pattern_index, (offering, activity_indices)) in
        activities_by_offering.into_iter().enumerate()
    {
        let pattern_id = positive_index(pattern_index, "meeting_pattern")?;
        let explicit_rule = explicit.get(&offering).copied();
        let forbid_cross_break = activity_indices
            .iter()
            .all(|index| !problem.activities()[*index].may_cross_breaks);
        let mut duration_periods = Vec::new();
        let mut activity_ids = Vec::new();
        let mut total = 0_u32;
        for index in activity_indices {
            activity_pattern_ids[index] = pattern_id;
            let duration = u32::from(problem.activities()[index].duration_periods);
            total = total.saturating_add(duration);
            duration_periods.push(duration);
            activity_ids.push(positive_index(index, "pattern_activity")?);
        }
        patterns.push(MeetingPattern {
            meeting_pattern_id: pattern_id,
            activity_ids,
            duration_periods,
            minimum_gap_days: explicit_rule.map_or(0, |rule| u32::from(rule.minimum_gap_days)),
            maximum_periods_per_day: explicit_rule
                .map_or(total, |rule| u32::from(rule.maximum_periods_per_day)),
            forbid_cross_break,
        });
    }
    Ok(PatternPlan {
        patterns,
        activity_pattern_ids,
    })
}

#[derive(Debug)]
struct TeacherBindingPlan {
    activity_binding_ids: Vec<u32>,
}

fn build_teacher_bindings(
    problem: &SemanticSnapshot,
) -> Result<TeacherBindingPlan, SolveApplicationError> {
    let mut requirements = BTreeMap::new();
    for activity in problem.activities() {
        if let Some(previous) =
            requirements.insert(activity.course_offering_id, activity.teacher.clone())
            && previous != activity.teacher
        {
            return Err(SolveApplicationError::Adapter {
                code: "APPLICATION_INCONSISTENT_OFFERING_TEACHERS",
                detail: activity.course_offering_id.to_string(),
            });
        }
    }
    let ids = requirements
        .keys()
        .enumerate()
        .map(|(index, offering)| Ok((*offering, positive_index(index, "teacher_binding")?)))
        .collect::<Result<BTreeMap<_, _>, SolveApplicationError>>()?;
    Ok(TeacherBindingPlan {
        activity_binding_ids: problem
            .activities()
            .iter()
            .map(|activity| ids[&activity.course_offering_id])
            .collect(),
    })
}

fn build_resource_conflicts(
    problem: &SemanticSnapshot,
) -> Result<Vec<ResourceConflictGroup>, SolveApplicationError> {
    let mut teacher_activities = vec![Vec::new(); problem.teachers().len()];
    let mut room_activities = vec![Vec::new(); problem.rooms().len()];
    for (activity_index, activity) in problem.activities().iter().enumerate() {
        let compact_activity = positive_index(activity_index, "resource_activity")?;
        for teacher in activity.teacher.candidates() {
            teacher_activities[teacher.as_usize()].push(compact_activity);
        }
        for room in activity.room.candidates() {
            room_activities[room.as_usize()].push(compact_activity);
        }
    }
    let mut groups = Vec::new();
    for (index, activity_ids) in teacher_activities.into_iter().enumerate() {
        if !activity_ids.is_empty() {
            groups.push(ResourceConflictGroup {
                resource_kind: ResourceKind::Teacher as i32,
                resource_id: positive_index(index, "resource_teacher")?,
                activity_ids,
            });
        }
    }
    for (index, activity_ids) in room_activities.into_iter().enumerate() {
        if !activity_ids.is_empty() {
            groups.push(ResourceConflictGroup {
                resource_kind: ResourceKind::Room as i32,
                resource_id: positive_index(index, "resource_room")?,
                activity_ids,
            });
        }
    }
    Ok(groups)
}

fn build_locks(
    problem: &SemanticSnapshot,
) -> Result<(Vec<LockedAssignment>, Vec<ConstraintGroup>), SolveApplicationError> {
    let mut locks = Vec::new();
    let mut groups = Vec::new();
    for lock in problem.locks() {
        let activity_id = positive_index(lock.assignment.activity.as_usize(), "lock_activity")?;
        let group_id = format!("lock:{activity_id}");
        locks.push(LockedAssignment {
            lock_id: group_id.clone(),
            assignment: Some(MeetingAssignment {
                activity_id,
                start_timeslot_id: positive_index(lock.assignment.start.as_usize(), "lock_start")?,
                room_id: positive_index(lock.assignment.room.as_usize(), "lock_room")?,
                teacher_id: positive_index(lock.assignment.teacher.as_usize(), "lock_teacher")?,
                duration_periods: u32::from(
                    problem.activities()[lock.assignment.activity.as_usize()].duration_periods,
                ),
            }),
            constraint_group_id: group_id.clone(),
        });
        groups.push(ConstraintGroup {
            group_id,
            problem_code: "HARD_LOCKED_ASSIGNMENT".to_owned(),
            severity: ConstraintSeverity::Hard as i32,
            assumption_enabled: true,
            related_entities: Vec::new(),
            parameters: HashMap::new(),
        });
    }
    Ok((locks, groups))
}

fn map_assignments(
    problem: &SemanticSnapshot,
    response: &SolveResponse,
) -> Result<Vec<Assignment>, SolveApplicationError> {
    response
        .assignments
        .iter()
        .map(|assignment| {
            let activity = zero_index(
                assignment.activity_id,
                problem.activities().len(),
                "activity_id",
            )?;
            let start = zero_index(
                assignment.start_timeslot_id,
                problem.timeslots().len(),
                "start_timeslot_id",
            )?;
            let room = zero_index(assignment.room_id, problem.rooms().len(), "room_id")?;
            let teacher = zero_index(
                assignment.teacher_id,
                problem.teachers().len(),
                "teacher_id",
            )?;
            let expected_duration = u32::from(problem.activities()[activity].duration_periods);
            if assignment.duration_periods != expected_duration {
                return Err(SolveApplicationError::InvalidOutputMapping {
                    code: "SOLVER_OUTPUT_DURATION_MISMATCH",
                    detail: format!(
                        "activity {} returned duration {}, expected {}",
                        assignment.activity_id, assignment.duration_periods, expected_duration
                    ),
                });
            }
            Ok(Assignment {
                activity: class_schedule_scheduling::ActivityIndex(compact_u32(activity)?),
                start: class_schedule_scheduling::TimeslotIndex(compact_u32(start)?),
                room: class_schedule_scheduling::RoomIndex(compact_u32(room)?),
                teacher: class_schedule_scheduling::TeacherIndex(compact_u32(teacher)?),
            })
        })
        .collect()
}

fn map_incumbent_assignments(
    problem: &SemanticSnapshot,
    assignments: &[Assignment],
) -> Result<Vec<MeetingAssignment>, SolveApplicationError> {
    assignments
        .iter()
        .map(|assignment| {
            let activity = problem
                .activities()
                .get(assignment.activity.as_usize())
                .ok_or_else(|| SolveApplicationError::Adapter {
                    code: "APPLICATION_INCUMBENT_ACTIVITY_OUT_OF_RANGE",
                    detail: assignment.activity.0.to_string(),
                })?;
            Ok(MeetingAssignment {
                activity_id: positive_index(assignment.activity.as_usize(), "incumbent_activity")?,
                start_timeslot_id: positive_index(assignment.start.as_usize(), "incumbent_start")?,
                room_id: positive_index(assignment.room.as_usize(), "incumbent_room")?,
                teacher_id: positive_index(assignment.teacher.as_usize(), "incumbent_teacher")?,
                duration_periods: u32::from(activity.duration_periods),
            })
        })
        .collect()
}

fn hash_assignments(assignments: &[Assignment]) -> Result<[u8; 32], SolveApplicationError> {
    Ok(*blake3::hash(&serde_json::to_vec(assignments)?).as_bytes())
}

fn collect_feature_ids(
    problem: &SemanticSnapshot,
) -> Result<BTreeMap<String, u32>, SolveApplicationError> {
    let features = problem
        .rooms()
        .iter()
        .flat_map(|room| &room.features)
        .chain(
            problem
                .activities()
                .iter()
                .flat_map(|activity| &activity.required_room_features),
        )
        .cloned()
        .collect::<BTreeSet<_>>();
    enumerate_ids(features, "feature")
}

fn collect_building_ids(
    problem: &SemanticSnapshot,
) -> Result<BTreeMap<class_schedule_domain::BuildingId, u32>, SolveApplicationError> {
    enumerate_ids(
        problem.rooms().iter().map(|room| room.building_id),
        "building",
    )
}

fn collect_subject_ids(
    problem: &SemanticSnapshot,
) -> Result<BTreeMap<class_schedule_domain::SubjectId, u32>, SolveApplicationError> {
    enumerate_ids(
        problem
            .activities()
            .iter()
            .map(|activity| activity.subject_id),
        "subject",
    )
}

fn collect_section_ids(
    problem: &SemanticSnapshot,
) -> Result<BTreeMap<class_schedule_domain::TeachingSectionId, u32>, SolveApplicationError> {
    enumerate_ids(
        problem
            .activities()
            .iter()
            .filter_map(|activity| activity.teaching_section_id),
        "section",
    )
}

fn collect_admin_class_ids(
    problem: &SemanticSnapshot,
) -> Result<BTreeMap<class_schedule_domain::AdministrativeClassId, u32>, SolveApplicationError> {
    enumerate_ids(
        problem
            .activities()
            .iter()
            .filter_map(|activity| activity.administrative_class_id),
        "administrative_class",
    )
}

fn enumerate_ids<T: Ord>(
    values: impl IntoIterator<Item = T>,
    field: &'static str,
) -> Result<BTreeMap<T, u32>, SolveApplicationError> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .enumerate()
        .map(|(index, value)| Ok((value, positive_index(index, field)?)))
        .collect()
}

fn room_index_set(
    rooms: &[class_schedule_scheduling::RoomIndex],
    field: &'static str,
) -> Result<BTreeSet<u32>, SolveApplicationError> {
    rooms
        .iter()
        .map(|room| positive_index(room.as_usize(), field))
        .collect()
}

fn positive_index(index: usize, field: &'static str) -> Result<u32, SolveApplicationError> {
    compact_u32(index.checked_add(1).ok_or(SolveApplicationError::Adapter {
        code: "APPLICATION_COMPACT_INDEX_OVERFLOW",
        detail: field.to_owned(),
    })?)
}

fn compact_u32(value: usize) -> Result<u32, SolveApplicationError> {
    u32::try_from(value).map_err(|_| SolveApplicationError::Adapter {
        code: "APPLICATION_COMPACT_INDEX_OVERFLOW",
        detail: value.to_string(),
    })
}

fn zero_index(value: u32, len: usize, field: &'static str) -> Result<usize, SolveApplicationError> {
    let index = value
        .checked_sub(1)
        .ok_or_else(|| SolveApplicationError::InvalidOutputMapping {
            code: "SOLVER_OUTPUT_ZERO_COMPACT_ID",
            detail: field.to_owned(),
        })? as usize;
    if index >= len {
        return Err(SolveApplicationError::InvalidOutputMapping {
            code: "SOLVER_OUTPUT_COMPACT_ID_OUT_OF_RANGE",
            detail: format!("{field}={value}, len={len}"),
        });
    }
    Ok(index)
}

const fn day_number(day: class_schedule_domain::Day) -> u8 {
    match day {
        class_schedule_domain::Day::Monday => 1,
        class_schedule_domain::Day::Tuesday => 2,
        class_schedule_domain::Day::Wednesday => 3,
        class_schedule_domain::Day::Thursday => 4,
        class_schedule_domain::Day::Friday => 5,
        class_schedule_domain::Day::Saturday => 6,
        class_schedule_domain::Day::Sunday => 7,
    }
}
