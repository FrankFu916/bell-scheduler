#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Deterministic, solver-independent quality scoring.
//!
//! Hard validity is deliberately outside the objective: invalid assignments are rejected before
//! a score is produced. Tiers are compared lexicographically and weights apply only within a tier.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use class_schedule_domain::{AdministrativeClassId, BuildingId, Day, SubjectId, TeachingSectionId};
use class_schedule_scheduling::{
    ActivityIndex, Assignment, DenseBitSet, SchedulingProblemSnapshot, TimeslotIndex,
};
use class_schedule_validation::{ValidationReport, validate_assignments};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    RepairChanges,
    CourseDistribution,
    DailySubjectConcentration,
    TeacherGaps,
    TeacherConsecutiveLoad,
    UndesirableTimeFairness,
    StudentMovement,
    TeacherMovement,
    LayoutStability,
}

impl MetricKind {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RepairChanges => "QUALITY_REPAIR_CHANGES",
            Self::CourseDistribution => "QUALITY_COURSE_DISTRIBUTION",
            Self::DailySubjectConcentration => "QUALITY_DAILY_SUBJECT_CONCENTRATION",
            Self::TeacherGaps => "QUALITY_TEACHER_GAPS",
            Self::TeacherConsecutiveLoad => "QUALITY_TEACHER_CONSECUTIVE_LOAD",
            Self::UndesirableTimeFairness => "QUALITY_UNDESIRABLE_TIME_FAIRNESS",
            Self::StudentMovement => "QUALITY_STUDENT_MOVEMENT",
            Self::TeacherMovement => "QUALITY_TEACHER_MOVEMENT",
            Self::LayoutStability => "QUALITY_LAYOUT_STABILITY",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetricDefinition {
    pub kind: MetricKind,
    pub weight_within_tier: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TierDefinition {
    pub id: String,
    pub priority: u32,
    pub metrics: Vec<MetricDefinition>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObjectivePlan {
    tiers: Vec<TierDefinition>,
}

impl ObjectivePlan {
    /// Constructs a plan and canonicalizes tier order.
    ///
    /// # Errors
    ///
    /// Rejects blank/duplicate tier identities, duplicate priorities, empty tiers, zero weights,
    /// and duplicate metric kinds within one tier.
    pub fn new(mut tiers: Vec<TierDefinition>) -> Result<Self, ScoringError> {
        let mut ids = BTreeSet::new();
        let mut priorities = BTreeSet::new();
        for tier in &tiers {
            if tier.id.trim().is_empty() {
                return Err(ScoringError::InvalidPlan("blank tier id"));
            }
            if !ids.insert(tier.id.clone()) {
                return Err(ScoringError::InvalidPlan("duplicate tier id"));
            }
            if tier.priority == 0 || !priorities.insert(tier.priority) {
                return Err(ScoringError::InvalidPlan(
                    "tier priority must be positive and unique",
                ));
            }
            if tier.metrics.is_empty() {
                return Err(ScoringError::InvalidPlan("empty tier"));
            }
            let mut kinds = BTreeSet::new();
            for metric in &tier.metrics {
                if metric.weight_within_tier == 0 {
                    return Err(ScoringError::InvalidPlan("zero metric weight"));
                }
                if !kinds.insert(metric.kind) {
                    return Err(ScoringError::InvalidPlan("duplicate metric in tier"));
                }
            }
        }
        tiers.sort_by_key(|tier| tier.priority);
        Ok(Self { tiers })
    }

    #[must_use]
    pub fn tiers(&self) -> &[TierDefinition] {
        &self.tiers
    }

    /// A conservative default for generation. Repair uses [`Self::repair_default`].
    #[must_use]
    pub fn balanced_default() -> Self {
        Self {
            tiers: vec![
                TierDefinition {
                    id: "distribution".to_owned(),
                    priority: 1,
                    metrics: vec![
                        MetricDefinition {
                            kind: MetricKind::CourseDistribution,
                            weight_within_tier: 4,
                        },
                        MetricDefinition {
                            kind: MetricKind::DailySubjectConcentration,
                            weight_within_tier: 2,
                        },
                        MetricDefinition {
                            kind: MetricKind::UndesirableTimeFairness,
                            weight_within_tier: 1,
                        },
                    ],
                },
                TierDefinition {
                    id: "staff_and_movement".to_owned(),
                    priority: 2,
                    metrics: vec![
                        MetricDefinition {
                            kind: MetricKind::TeacherGaps,
                            weight_within_tier: 2,
                        },
                        MetricDefinition {
                            kind: MetricKind::TeacherConsecutiveLoad,
                            weight_within_tier: 2,
                        },
                        MetricDefinition {
                            kind: MetricKind::StudentMovement,
                            weight_within_tier: 1,
                        },
                        MetricDefinition {
                            kind: MetricKind::TeacherMovement,
                            weight_within_tier: 1,
                        },
                    ],
                },
            ],
        }
    }

    #[must_use]
    pub fn repair_default() -> Self {
        let mut tiers = vec![TierDefinition {
            id: "repair_changes".to_owned(),
            priority: 1,
            metrics: vec![MetricDefinition {
                kind: MetricKind::RepairChanges,
                weight_within_tier: 1,
            }],
        }];
        tiers.extend(Self::balanced_default().tiers.into_iter().map(|mut tier| {
            tier.priority += 1;
            tier
        }));
        Self { tiers }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoringContext {
    pub undesirable_timeslots: DenseBitSet,
    pub baseline: BTreeMap<ActivityIndex, Assignment>,
    pub building_travel_costs: BTreeMap<(BuildingId, BuildingId), u32>,
    pub maximum_subject_periods_per_day: u16,
    pub maximum_teacher_consecutive_periods: u16,
}

impl ScoringContext {
    #[must_use]
    pub fn neutral(problem: &SchedulingProblemSnapshot) -> Self {
        Self {
            undesirable_timeslots: DenseBitSet::empty(problem.timeslots().len()),
            baseline: BTreeMap::new(),
            building_travel_costs: BTreeMap::new(),
            maximum_subject_periods_per_day: 2,
            maximum_teacher_consecutive_periods: 3,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetricScore {
    pub kind: MetricKind,
    pub raw_value: i64,
    pub weight_within_tier: u32,
    pub weighted_value: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TierScore {
    pub id: String,
    pub priority: u32,
    pub value: i64,
    pub metrics: Vec<MetricScore>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObjectiveVector {
    pub tiers: Vec<TierScore>,
}

impl ObjectiveVector {
    /// Lower values are better; only the first differing tier is considered.
    #[must_use]
    pub fn lexicographic_cmp(&self, other: &Self) -> Ordering {
        self.tiers
            .iter()
            .map(|tier| tier.value)
            .cmp(other.tiers.iter().map(|tier| tier.value))
    }
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum ScoringError {
    #[error("cannot score a timetable that violates hard constraints")]
    HardInvalid(ValidationReport),
    #[error("invalid scoring plan: {0}")]
    InvalidPlan(&'static str),
    #[error("scoring context bitset length {actual} does not match timeslot count {expected}")]
    ContextBitSetLength { expected: usize, actual: usize },
    #[error("metric {metric:?} requires a complete baseline; activity {activity} is missing")]
    MissingBaseline { metric: MetricKind, activity: u32 },
    #[error("quality score overflowed i64")]
    Overflow,
}

/// Computes a deterministic quality vector after independently checking every Hard constraint.
///
/// # Errors
///
/// Returns an error for an invalid timetable, inconsistent context, missing required baseline, or
/// arithmetic overflow.
pub fn evaluate(
    problem: &SchedulingProblemSnapshot,
    assignments: &[Assignment],
    plan: &ObjectivePlan,
    context: &ScoringContext,
) -> Result<ObjectiveVector, ScoringError> {
    let hard = validate_assignments(problem, assignments);
    if !hard.is_valid() {
        return Err(ScoringError::HardInvalid(hard));
    }
    if context.undesirable_timeslots.len() != problem.timeslots().len() {
        return Err(ScoringError::ContextBitSetLength {
            expected: problem.timeslots().len(),
            actual: context.undesirable_timeslots.len(),
        });
    }
    let indexed = index_assignments(assignments);
    let facts = ScoreFacts::new(problem, &indexed);
    let mut tiers = Vec::with_capacity(plan.tiers.len());
    for tier in &plan.tiers {
        let mut value = 0_i64;
        let mut metrics = Vec::with_capacity(tier.metrics.len());
        for definition in &tier.metrics {
            let raw = metric_value(definition.kind, problem, &indexed, &facts, context)?;
            let weighted = checked_weight(raw, definition.weight_within_tier)?;
            value = value.checked_add(weighted).ok_or(ScoringError::Overflow)?;
            metrics.push(MetricScore {
                kind: definition.kind,
                raw_value: raw,
                weight_within_tier: definition.weight_within_tier,
                weighted_value: weighted,
            });
        }
        tiers.push(TierScore {
            id: tier.id.clone(),
            priority: tier.priority,
            value,
            metrics,
        });
    }
    Ok(ObjectiveVector { tiers })
}

#[derive(Debug)]
struct ScoreFacts {
    occupied: BTreeMap<ActivityIndex, Vec<TimeslotIndex>>,
}

impl ScoreFacts {
    fn new(
        problem: &SchedulingProblemSnapshot,
        assignments: &BTreeMap<ActivityIndex, Assignment>,
    ) -> Self {
        let occupied = assignments
            .iter()
            .map(|(activity, assignment)| {
                (
                    *activity,
                    problem
                        .occupied_slots(*activity, assignment.start)
                        .expect("hard-valid assignment has valid duration"),
                )
            })
            .collect();
        Self { occupied }
    }
}

fn metric_value(
    kind: MetricKind,
    problem: &SchedulingProblemSnapshot,
    assignments: &BTreeMap<ActivityIndex, Assignment>,
    facts: &ScoreFacts,
    context: &ScoringContext,
) -> Result<i64, ScoringError> {
    match kind {
        MetricKind::RepairChanges => baseline_changes(kind, problem, assignments, context, false),
        MetricKind::LayoutStability => baseline_changes(kind, problem, assignments, context, true),
        MetricKind::CourseDistribution => Ok(course_distribution(problem, assignments)),
        MetricKind::DailySubjectConcentration => Ok(daily_subject_concentration(
            problem,
            assignments,
            context.maximum_subject_periods_per_day,
        )),
        MetricKind::TeacherGaps => Ok(teacher_gaps(problem, assignments, facts)),
        MetricKind::TeacherConsecutiveLoad => Ok(teacher_consecutive_load(
            problem,
            assignments,
            facts,
            context.maximum_teacher_consecutive_periods,
        )),
        MetricKind::UndesirableTimeFairness => Ok(undesirable_time_fairness(
            problem,
            assignments,
            facts,
            context,
        )),
        MetricKind::StudentMovement => Ok(student_movement(problem, assignments, context)),
        MetricKind::TeacherMovement => Ok(teacher_movement(problem, assignments, context)),
    }
}

fn baseline_changes(
    kind: MetricKind,
    problem: &SchedulingProblemSnapshot,
    assignments: &BTreeMap<ActivityIndex, Assignment>,
    context: &ScoringContext,
    count_dimensions: bool,
) -> Result<i64, ScoringError> {
    let mut changes = 0_i64;
    for index in 0..problem.activities().len() {
        let activity = ActivityIndex(compact_u32(index));
        let baseline = context
            .baseline
            .get(&activity)
            .ok_or(ScoringError::MissingBaseline {
                metric: kind,
                activity: activity.0,
            })?;
        let current = assignments[&activity];
        changes += if count_dimensions {
            i64::from(current.start != baseline.start)
                + i64::from(current.room != baseline.room)
                + i64::from(current.teacher != baseline.teacher)
        } else {
            i64::from(current != *baseline)
        };
    }
    Ok(changes)
}

fn course_distribution(
    problem: &SchedulingProblemSnapshot,
    assignments: &BTreeMap<ActivityIndex, Assignment>,
) -> i64 {
    let mut penalty = 0_i64;
    for pattern in problem.meeting_patterns() {
        let days = pattern
            .activities
            .iter()
            .map(|activity| {
                day_number(problem.timeslots()[assignments[activity].start.as_usize()].day)
            })
            .collect::<Vec<_>>();
        for left in 0..days.len() {
            for right in (left + 1)..days.len() {
                penalty += match days[left].abs_diff(days[right]) {
                    0 => 4,
                    1 => 1,
                    _ => 0,
                };
            }
        }
    }
    penalty
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AudienceKey {
    section: Option<TeachingSectionId>,
    administrative_class: Option<AdministrativeClassId>,
}

fn daily_subject_concentration(
    problem: &SchedulingProblemSnapshot,
    assignments: &BTreeMap<ActivityIndex, Assignment>,
    maximum: u16,
) -> i64 {
    let mut periods: BTreeMap<(AudienceKey, SubjectId, Day), u16> = BTreeMap::new();
    for (activity_index, assignment) in assignments {
        let activity = &problem.activities()[activity_index.as_usize()];
        let day = problem.timeslots()[assignment.start.as_usize()].day;
        let key = AudienceKey {
            section: activity.teaching_section_id,
            administrative_class: activity.administrative_class_id,
        };
        *periods.entry((key, activity.subject_id, day)).or_default() +=
            u16::from(activity.duration_periods);
    }
    periods
        .values()
        .map(|count| {
            let excess = i64::from(count.saturating_sub(maximum));
            excess * excess
        })
        .sum()
}

fn teacher_gaps(
    problem: &SchedulingProblemSnapshot,
    assignments: &BTreeMap<ActivityIndex, Assignment>,
    facts: &ScoreFacts,
) -> i64 {
    let mut occupied: BTreeMap<(u32, Day), BTreeSet<u16>> = BTreeMap::new();
    for (activity, assignment) in assignments {
        for slot in &facts.occupied[activity] {
            let value = &problem.timeslots()[slot.as_usize()];
            occupied
                .entry((assignment.teacher.0, value.day))
                .or_default()
                .insert(value.period_index);
        }
    }
    occupied
        .values()
        .map(|periods| {
            let Some(first) = periods.first() else {
                return 0;
            };
            let last = periods.last().expect("non-empty when first exists");
            i64::from(last - first + 1) - i64::try_from(periods.len()).unwrap_or(i64::MAX)
        })
        .sum()
}

fn teacher_consecutive_load(
    problem: &SchedulingProblemSnapshot,
    assignments: &BTreeMap<ActivityIndex, Assignment>,
    facts: &ScoreFacts,
    maximum: u16,
) -> i64 {
    let mut groups: BTreeMap<(u32, Day, u8), BTreeSet<u16>> = BTreeMap::new();
    for (activity, assignment) in assignments {
        for slot in &facts.occupied[activity] {
            let value = &problem.timeslots()[slot.as_usize()];
            groups
                .entry((assignment.teacher.0, value.day, value.instructional_block))
                .or_default()
                .insert(value.period_index);
        }
    }
    groups
        .values()
        .map(|periods| consecutive_excess(periods, maximum))
        .sum()
}

fn consecutive_excess(periods: &BTreeSet<u16>, maximum: u16) -> i64 {
    let mut previous = None;
    let mut run = 0_u16;
    let mut penalty = 0_i64;
    for period in periods {
        if previous.is_some_and(|value| value + 1 == *period) {
            run += 1;
        } else {
            penalty += squared_excess(run, maximum);
            run = 1;
        }
        previous = Some(*period);
    }
    penalty + squared_excess(run, maximum)
}

fn squared_excess(value: u16, maximum: u16) -> i64 {
    let excess = i64::from(value.saturating_sub(maximum));
    excess * excess
}

fn undesirable_time_fairness(
    problem: &SchedulingProblemSnapshot,
    assignments: &BTreeMap<ActivityIndex, Assignment>,
    facts: &ScoreFacts,
    context: &ScoringContext,
) -> i64 {
    let mut counts: BTreeMap<AudienceKey, i64> = BTreeMap::new();
    for (activity_index, activity) in problem.activities().iter().enumerate() {
        let key = AudienceKey {
            section: activity.teaching_section_id,
            administrative_class: activity.administrative_class_id,
        };
        counts.entry(key).or_default();
        let activity_index = ActivityIndex(compact_u32(activity_index));
        if assignments.contains_key(&activity_index) {
            let undesirable_count = facts.occupied[&activity_index]
                .iter()
                .filter(|slot| context.undesirable_timeslots.contains(slot.as_usize()))
                .count();
            *counts.get_mut(&key).expect("inserted above") +=
                i64::try_from(undesirable_count).expect("materialized timeslot count fits i64");
        }
    }
    if counts.len() < 2 {
        return 0;
    }
    let total: i64 = counts.values().sum();
    let count = i64::try_from(counts.len()).expect("materialized audience count fits i64");
    counts
        .values()
        .map(|value| (value * count - total).abs())
        .sum()
}

fn student_movement(
    problem: &SchedulingProblemSnapshot,
    assignments: &BTreeMap<ActivityIndex, Assignment>,
    context: &ScoringContext,
) -> i64 {
    let mut movement = 0_i64;
    for student in 0..problem.students().len() {
        let mut by_day: BTreeMap<Day, Vec<(u16, BuildingId)>> = BTreeMap::new();
        for (activity_index, assignment) in assignments {
            let activity = &problem.activities()[activity_index.as_usize()];
            if activity.audience.contains(student) {
                let slot = &problem.timeslots()[assignment.start.as_usize()];
                by_day.entry(slot.day).or_default().push((
                    slot.period_index,
                    problem.rooms()[assignment.room.as_usize()].building_id,
                ));
            }
        }
        movement += movement_for_days(&mut by_day, context);
    }
    movement
}

fn teacher_movement(
    problem: &SchedulingProblemSnapshot,
    assignments: &BTreeMap<ActivityIndex, Assignment>,
    context: &ScoringContext,
) -> i64 {
    let mut by_teacher_day: BTreeMap<(u32, Day), Vec<(u16, BuildingId)>> = BTreeMap::new();
    for assignment in assignments.values() {
        let slot = &problem.timeslots()[assignment.start.as_usize()];
        by_teacher_day
            .entry((assignment.teacher.0, slot.day))
            .or_default()
            .push((
                slot.period_index,
                problem.rooms()[assignment.room.as_usize()].building_id,
            ));
    }
    by_teacher_day
        .into_values()
        .map(|mut values| movement_for_sequence(&mut values, context))
        .sum()
}

fn movement_for_days(
    by_day: &mut BTreeMap<Day, Vec<(u16, BuildingId)>>,
    context: &ScoringContext,
) -> i64 {
    by_day
        .values_mut()
        .map(|values| movement_for_sequence(values, context))
        .sum()
}

fn movement_for_sequence(values: &mut [(u16, BuildingId)], context: &ScoringContext) -> i64 {
    values.sort_unstable();
    values
        .windows(2)
        .map(|pair| travel_cost(pair[0].1, pair[1].1, context))
        .sum()
}

fn travel_cost(from: BuildingId, to: BuildingId, context: &ScoringContext) -> i64 {
    if from == to {
        return 0;
    }
    i64::from(
        context
            .building_travel_costs
            .get(&(from, to))
            .or_else(|| context.building_travel_costs.get(&(to, from)))
            .copied()
            .unwrap_or(1),
    )
}

fn checked_weight(raw: i64, weight: u32) -> Result<i64, ScoringError> {
    raw.checked_mul(i64::from(weight))
        .ok_or(ScoringError::Overflow)
}

fn index_assignments(assignments: &[Assignment]) -> BTreeMap<ActivityIndex, Assignment> {
    assignments
        .iter()
        .map(|assignment| (assignment.activity, *assignment))
        .collect()
}

fn compact_u32(index: usize) -> u32 {
    u32::try_from(index).expect("materialized snapshot index fits u32")
}

const fn day_number(day: Day) -> u8 {
    match day {
        Day::Monday => 1,
        Day::Tuesday => 2,
        Day::Wednesday => 3,
        Day::Thursday => 4,
        Day::Friday => 5,
        Day::Saturday => 6,
        Day::Sunday => 7,
    }
}
