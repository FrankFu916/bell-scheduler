#![forbid(unsafe_code)]

//! Exhaustive reference solver for differential tests on very small scheduling fixtures.
//!
//! This crate deliberately performs no production search or constraint reasoning. It enumerates
//! every `allowed start x teacher candidate x room candidate` assignment and delegates all hard
//! constraint decisions to `class-schedule-validation`. Fixed limits make accidental use with a
//! school-sized problem fail before enumeration.

use std::cmp::Ordering;

use class_schedule_scheduling::{
    ActivityIndex, Assignment, SchedulingProblemSnapshot, TeacherIndex, TimeslotIndex,
};
use class_schedule_validation::validate_assignments;
use thiserror::Error;

/// Absolute activity limit for the exhaustive oracle.
pub const MAX_ACTIVITIES: usize = 8;
/// Absolute number of choices allowed for one activity.
pub const MAX_CHOICES_PER_ACTIVITY: u128 = 512;
/// Absolute number of complete assignment vectors the oracle may inspect.
pub const MAX_COMPLETE_ASSIGNMENTS: u64 = 1_000_000;

/// A minimization objective ordered by tier: earlier vector elements have higher priority.
pub type ObjectiveCallback<'a> = dyn FnMut(&[Assignment]) -> Vec<i64> + 'a;

type ScoredAssignments = (Vec<i64>, Vec<Assignment>);

/// Why the bounded test oracle refused to start.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum OracleError {
    #[error("test oracle supports at most {maximum} activities, but the problem has {actual}")]
    TooManyActivities { actual: usize, maximum: usize },
    #[error(
        "activity {activity_index} has {actual} start/teacher/room choices; the test oracle limit is {maximum}"
    )]
    TooManyChoicesForActivity {
        activity_index: usize,
        actual: u128,
        maximum: u128,
    },
    #[error(
        "problem has {actual} complete assignment combinations; the test oracle limit is {maximum}"
    )]
    SearchSpaceTooLarge { actual: u128, maximum: u64 },
    #[error(
        "objective returned {actual} tiers after previously returning {expected}; tier count must be stable"
    )]
    InconsistentObjectiveTierCount { expected: usize, actual: usize },
}

/// Mathematically distinct outcomes reported by exhaustive enumeration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OracleStatus {
    /// The deterministic first feasible assignment was returned without optimizing.
    Feasible,
    /// Every candidate was inspected and the lexicographic optimum was returned.
    Optimal,
    /// Every candidate was inspected and none passed independent hard validation.
    ProvenInfeasible,
}

/// Auditable counts for one oracle run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OracleStatistics {
    /// Size of the complete Cartesian search space.
    pub search_space: u64,
    /// Number of complete assignment vectors passed to the independent validator.
    pub evaluated_assignments: u64,
    /// Number of vectors accepted by the independent validator.
    pub feasible_assignments: u64,
}

/// Result from the bounded exhaustive oracle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleResult {
    pub status: OracleStatus,
    /// Empty for `ProvenInfeasible`; otherwise one assignment per activity in activity order.
    pub assignments: Vec<Assignment>,
    /// Present only when a lexicographic objective callback was supplied and an optimum exists.
    pub objective: Option<Vec<i64>>,
    pub statistics: OracleStatistics,
}

/// Exhaustively searches a very-small snapshot.
///
/// With no callback, enumeration stops at the deterministic first feasible assignment. With a
/// callback, smaller objective vectors are better, comparison is lexicographic, and the complete
/// bounded space is searched. Equal scores use a stable assignment tuple tie-break.
///
/// The callback must return the same number of objective tiers for every feasible assignment.
///
/// # Errors
///
/// Returns [`OracleError`] before or during enumeration when a hard safety limit is exceeded or
/// when the objective callback changes its number of tiers.
pub fn solve(
    problem: &SchedulingProblemSnapshot,
    mut objective: Option<&mut ObjectiveCallback<'_>>,
) -> Result<OracleResult, OracleError> {
    let domains = build_domains(problem)?;
    let search_space = bounded_search_space(&domains)?;
    let mut statistics = OracleStatistics {
        search_space,
        evaluated_assignments: 0,
        feasible_assignments: 0,
    };

    if domains.iter().any(Vec::is_empty) {
        return Ok(infeasible(statistics));
    }

    let mut cursor = vec![0; domains.len()];
    let mut expected_tier_count = None;
    let mut best: Option<ScoredAssignments> = None;

    loop {
        let assignments = materialize_assignment(&domains, &cursor);
        statistics.evaluated_assignments += 1;
        if validate_assignments(problem, &assignments).is_valid() {
            statistics.feasible_assignments += 1;
            let Some(callback) = objective.as_deref_mut() else {
                return Ok(OracleResult {
                    status: OracleStatus::Feasible,
                    assignments,
                    objective: None,
                    statistics,
                });
            };

            let score = callback(&assignments);
            match expected_tier_count {
                Some(expected) if expected != score.len() => {
                    return Err(OracleError::InconsistentObjectiveTierCount {
                        expected,
                        actual: score.len(),
                    });
                }
                None => expected_tier_count = Some(score.len()),
                Some(_) => {}
            }
            if is_better(&score, &assignments, best.as_ref()) {
                best = Some((score, assignments));
            }
        }

        if !advance(&mut cursor, &domains) {
            break;
        }
    }

    match best {
        Some((score, assignments)) => Ok(OracleResult {
            status: OracleStatus::Optimal,
            assignments,
            objective: Some(score),
            statistics,
        }),
        None => Ok(infeasible(statistics)),
    }
}

/// Finds the deterministic first independently validated feasible assignment.
///
/// # Errors
///
/// Returns [`OracleError`] when the snapshot exceeds an absolute oracle safety limit.
pub fn find_feasible(problem: &SchedulingProblemSnapshot) -> Result<OracleResult, OracleError> {
    solve(problem, None)
}

/// Proves and returns the lexicographic optimum for a bounded very-small snapshot.
///
/// Lower values are better at every tier. The first tier dominates every later tier.
///
/// # Errors
///
/// Returns [`OracleError`] when a safety limit is exceeded or the callback changes tier count.
pub fn optimize_lexicographic(
    problem: &SchedulingProblemSnapshot,
    objective: &mut ObjectiveCallback<'_>,
) -> Result<OracleResult, OracleError> {
    solve(problem, Some(objective))
}

fn build_domains(problem: &SchedulingProblemSnapshot) -> Result<Vec<Vec<Assignment>>, OracleError> {
    if problem.activities().len() > MAX_ACTIVITIES {
        return Err(OracleError::TooManyActivities {
            actual: problem.activities().len(),
            maximum: MAX_ACTIVITIES,
        });
    }

    problem
        .activities()
        .iter()
        .enumerate()
        .map(|(index, activity)| {
            let rooms = activity.room.candidates();
            let choice_count = usize_as_u128(activity.allowed_starts.len())
                .saturating_mul(usize_as_u128(activity.teacher.candidates().len()))
                .saturating_mul(usize_as_u128(rooms.len()));
            if choice_count > MAX_CHOICES_PER_ACTIVITY {
                return Err(OracleError::TooManyChoicesForActivity {
                    activity_index: index,
                    actual: choice_count,
                    maximum: MAX_CHOICES_PER_ACTIVITY,
                });
            }

            let mut starts = activity.allowed_starts.clone();
            starts.sort_unstable();
            let mut teachers = activity.teacher.candidates().to_vec();
            teachers.sort_unstable();
            let mut rooms = rooms;
            rooms.sort_unstable();
            let activity_index = ActivityIndex(
                u32::try_from(index).expect("the oracle activity hard limit always fits u32"),
            );
            let mut domain = Vec::with_capacity(
                usize::try_from(choice_count)
                    .expect("an activity domain is bounded by the usize-sized oracle limit"),
            );
            for start in starts {
                for teacher in &teachers {
                    for room in &rooms {
                        domain.push(Assignment {
                            activity: activity_index,
                            start,
                            room: *room,
                            teacher: *teacher,
                        });
                    }
                }
            }
            Ok(domain)
        })
        .collect()
}

fn bounded_search_space(domains: &[Vec<Assignment>]) -> Result<u64, OracleError> {
    let actual = domains
        .iter()
        .map(|domain| usize_as_u128(domain.len()))
        .product::<u128>();
    if actual > u128::from(MAX_COMPLETE_ASSIGNMENTS) {
        Err(OracleError::SearchSpaceTooLarge {
            actual,
            maximum: MAX_COMPLETE_ASSIGNMENTS,
        })
    } else {
        Ok(u64::try_from(actual)
            .expect("accepted oracle search space is bounded by a u64 constant"))
    }
}

fn materialize_assignment(domains: &[Vec<Assignment>], cursor: &[usize]) -> Vec<Assignment> {
    domains
        .iter()
        .zip(cursor)
        .map(|(domain, index)| domain[*index])
        .collect()
}

fn advance(cursor: &mut [usize], domains: &[Vec<Assignment>]) -> bool {
    for index in (0..cursor.len()).rev() {
        cursor[index] += 1;
        if cursor[index] < domains[index].len() {
            return true;
        }
        cursor[index] = 0;
    }
    false
}

fn is_better(
    score: &[i64],
    assignments: &[Assignment],
    current: Option<&ScoredAssignments>,
) -> bool {
    let Some((current_score, current_assignments)) = current else {
        return true;
    };
    match score.cmp(current_score) {
        Ordering::Less => true,
        Ordering::Equal => compare_assignments(assignments, current_assignments).is_lt(),
        Ordering::Greater => false,
    }
}

fn compare_assignments(left: &[Assignment], right: &[Assignment]) -> Ordering {
    left.iter()
        .map(assignment_key)
        .cmp(right.iter().map(assignment_key))
}

const fn assignment_key(
    assignment: &Assignment,
) -> (
    ActivityIndex,
    TimeslotIndex,
    TeacherIndex,
    class_schedule_scheduling::RoomIndex,
) {
    (
        assignment.activity,
        assignment.start,
        assignment.teacher,
        assignment.room,
    )
}

const fn infeasible(statistics: OracleStatistics) -> OracleResult {
    OracleResult {
        status: OracleStatus::ProvenInfeasible,
        assignments: Vec::new(),
        objective: None,
        statistics,
    }
}

fn usize_as_u128(value: usize) -> u128 {
    u128::try_from(value).expect("usize always fits into u128")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use class_schedule_domain::{
        BuildingId, CourseOfferingId, CoursePlanId, Day, MeetingDemandId, RoomId, StudentId,
        SubjectId, TeacherId, TimeslotId,
    };
    use class_schedule_scheduling::{
        Activity, DenseBitSet, DenseRoom, DenseTeacher, DenseTimeslot, RoomIndex, RoomRequirement,
        SchedulingProblemDraft, TeacherRequirement,
    };
    use uuid::Uuid;

    use super::*;

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn fixture(slot_count: usize, activity_count: usize) -> SchedulingProblemSnapshot {
        let students = vec![StudentId::from_uuid(id(1))];
        let building_id = BuildingId::from_uuid(id(2));
        let timeslots = (0..slot_count)
            .map(|index| DenseTimeslot {
                stable_id: TimeslotId::from_uuid(id(100 + usize_as_u128(index))),
                day: Day::Monday,
                period_index: u16::try_from(index + 1).unwrap(),
                instructional_block: 1,
                next_consecutive: None,
            })
            .collect();
        let teachers = (0..2)
            .map(|index| DenseTeacher {
                stable_id: TeacherId::from_uuid(id(200 + usize_as_u128(index))),
                available: DenseBitSet::full(slot_count),
            })
            .collect();
        let rooms = (0..2)
            .map(|index| DenseRoom {
                stable_id: RoomId::from_uuid(id(300 + usize_as_u128(index))),
                building_id,
                capacity: 1,
                features: BTreeSet::new(),
                available: DenseBitSet::full(slot_count),
            })
            .collect();
        let starts = (0..slot_count)
            .rev()
            .map(|index| TimeslotIndex(u32::try_from(index).unwrap()))
            .collect::<Vec<_>>();
        let activities = (0..activity_count)
            .map(|index| Activity {
                stable_id: MeetingDemandId::from_uuid(id(400 + usize_as_u128(index))),
                course_offering_id: CourseOfferingId::from_uuid(id(450 + usize_as_u128(index))),
                subject_id: SubjectId::from_uuid(id(500 + usize_as_u128(index))),
                course_plan_id: CoursePlanId::from_uuid(id(600 + usize_as_u128(index))),
                teaching_section_id: None,
                administrative_class_id: None,
                duration_periods: 1,
                may_cross_breaks: false,
                allowed_starts: starts.clone(),
                audience: DenseBitSet::from_indices(1, [0]).unwrap(),
                teacher: TeacherRequirement::Candidates {
                    teachers: vec![TeacherIndex(1), TeacherIndex(0)],
                },
                room: RoomRequirement::Flexible {
                    candidate_rooms: vec![RoomIndex(1), RoomIndex(0)],
                },
                required_capacity: 1,
                required_room_features: BTreeSet::new(),
            })
            .collect();

        SchedulingProblemDraft {
            schema_version: 1,
            students,
            teachers,
            rooms,
            timeslots,
            activities,
            meeting_patterns: vec![],
            locks: vec![],
        }
        .try_into()
        .unwrap()
    }

    fn distribution_objective(assignments: &[Assignment]) -> Vec<i64> {
        let slot_two_count = assignments
            .iter()
            .filter(|assignment| assignment.start == TimeslotIndex(2))
            .count();
        let start_sum = assignments
            .iter()
            .map(|assignment| i64::from(assignment.start.0))
            .sum();
        vec![i64::try_from(slot_two_count).unwrap(), start_sum]
    }

    #[test]
    fn finds_a_feasible_assignment_with_stable_domain_order() {
        let problem = fixture(2, 2);
        let result = find_feasible(&problem).unwrap();

        assert_eq!(result.status, OracleStatus::Feasible);
        assert_eq!(result.statistics.search_space, 64);
        assert_eq!(result.statistics.evaluated_assignments, 5);
        assert_eq!(result.statistics.feasible_assignments, 1);
        assert_eq!(result.assignments[0].start, TimeslotIndex(0));
        assert_eq!(result.assignments[1].start, TimeslotIndex(1));
        assert!(validate_assignments(&problem, &result.assignments).is_valid());
    }

    #[test]
    fn proves_infeasibility_using_the_independent_validator() {
        let problem = fixture(1, 2);
        let result = find_feasible(&problem).unwrap();

        assert_eq!(result.status, OracleStatus::ProvenInfeasible);
        assert!(result.assignments.is_empty());
        assert_eq!(result.statistics.search_space, 16);
        assert_eq!(result.statistics.evaluated_assignments, 16);
        assert_eq!(result.statistics.feasible_assignments, 0);
    }

    #[test]
    fn returns_a_deterministic_lexicographic_optimum_and_stable_tie_break() {
        let problem = fixture(3, 2);
        let mut first_objective = distribution_objective;
        let mut second_objective = distribution_objective;
        let first = optimize_lexicographic(&problem, &mut first_objective).unwrap();
        let second = optimize_lexicographic(&problem, &mut second_objective).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.status, OracleStatus::Optimal);
        assert_eq!(first.objective, Some(vec![0, 1]));
        assert_eq!(first.assignments[0].start, TimeslotIndex(0));
        assert_eq!(first.assignments[1].start, TimeslotIndex(1));
        assert!(
            first
                .assignments
                .iter()
                .all(|assignment| assignment.teacher == TeacherIndex(0))
        );
        assert!(
            first
                .assignments
                .iter()
                .all(|assignment| assignment.room == RoomIndex(0))
        );
        assert_eq!(first.statistics.evaluated_assignments, 144);
    }

    #[test]
    fn rejects_a_problem_above_the_absolute_activity_limit() {
        let problem = fixture(1, MAX_ACTIVITIES + 1);
        assert_eq!(
            find_feasible(&problem),
            Err(OracleError::TooManyActivities {
                actual: MAX_ACTIVITIES + 1,
                maximum: MAX_ACTIVITIES,
            })
        );
    }

    #[test]
    fn rejects_a_cartesian_product_above_the_absolute_search_limit() {
        let problem = fixture(8, 5);
        assert_eq!(
            find_feasible(&problem),
            Err(OracleError::SearchSpaceTooLarge {
                actual: 32_u128.pow(5),
                maximum: MAX_COMPLETE_ASSIGNMENTS,
            })
        );
    }
}
