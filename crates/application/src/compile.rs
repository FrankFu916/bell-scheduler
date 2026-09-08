use std::collections::{BTreeMap, BTreeSet};

use class_schedule_domain::{AdministrativeClassId, Day, StudentId, TeachingSectionId};
use class_schedule_import::{
    CourseOfferingImportRow, CoursePlanImportRow, ImportBatch, ImportedAudienceKind, ImportedDay,
    ImportedRoomPolicy, ImportedTeacherAssignment, TeachingSectionImportRow,
};
use class_schedule_scheduling::{
    Activity, ActivityIndex, Assignment, DenseBitSet, DenseRoom, DenseTeacher, DenseTimeslot,
    LockedAssignment, MeetingPatternRule, RoomIndex, RoomRequirement, SchedulingError,
    SchedulingProblemDraft, SchedulingProblemSnapshot, TeacherIndex, TeacherRequirement,
    TimeslotIndex,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::PreparedSectioningCandidate;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CalendarPeriodDefinition {
    pub index: u16,
    pub label: String,
    pub instructional_block: u8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CalendarDefinition {
    pub days: Vec<Day>,
    pub periods: Vec<CalendarPeriodDefinition>,
}

impl CalendarDefinition {
    /// Creates a validated calendar template.
    ///
    /// # Errors
    ///
    /// Rejects empty or duplicate days and malformed period definitions.
    pub fn new(
        days: Vec<Day>,
        mut periods: Vec<CalendarPeriodDefinition>,
    ) -> Result<Self, CompileError> {
        if days.is_empty() || periods.is_empty() {
            return Err(CompileError::CalendarInvalid {
                detail: "days and periods must be non-empty".to_owned(),
            });
        }
        if days.iter().collect::<BTreeSet<_>>().len() != days.len() {
            return Err(CompileError::CalendarInvalid {
                detail: "duplicate instructional day".to_owned(),
            });
        }
        periods.sort_by_key(|period| period.index);
        for (position, period) in periods.iter().enumerate() {
            let expected = u16::try_from(position + 1).map_err(|_| CompileError::IndexOverflow)?;
            if period.index != expected
                || period.label.trim().is_empty()
                || period.instructional_block == 0
            {
                return Err(CompileError::CalendarInvalid {
                    detail: format!("invalid period definition at position {}", position + 1),
                });
            }
        }
        Ok(Self { days, periods })
    }

    /// Builds Monday-Friday with an explicit break boundary.
    ///
    /// # Errors
    ///
    /// Rejects zero periods or a break position outside the period range.
    pub fn weekday_with_break(
        periods_per_day: u16,
        break_after_period: u16,
    ) -> Result<Self, CompileError> {
        if periods_per_day == 0 || break_after_period == 0 || break_after_period >= periods_per_day
        {
            return Err(CompileError::CalendarInvalid {
                detail: "break must split a positive school day".to_owned(),
            });
        }
        Self::new(
            vec![
                Day::Monday,
                Day::Tuesday,
                Day::Wednesday,
                Day::Thursday,
                Day::Friday,
            ],
            (1..=periods_per_day)
                .map(|index| CalendarPeriodDefinition {
                    index,
                    label: format!("第{index}节"),
                    instructional_block: if index <= break_after_period { 1 } else { 2 },
                })
                .collect(),
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompiledActivityLabel {
    pub course_plan_code: String,
    pub audience_kind: ImportedAudienceKind,
    pub audience_code: String,
    pub meeting_ordinal: u16,
    pub duration_periods: u8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompiledCatalog {
    pub student_codes: Vec<String>,
    pub teacher_codes: Vec<String>,
    pub room_codes: Vec<String>,
    pub timeslot_labels: Vec<String>,
    pub activities: Vec<CompiledActivityLabel>,
}

#[derive(Clone, Debug)]
pub struct CompiledSchoolProblem {
    pub problem: SchedulingProblemSnapshot,
    pub catalog: CompiledCatalog,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CompileError {
    #[error("calendar definition is invalid: {detail}")]
    CalendarInvalid { detail: String },
    #[error("calendar reference does not exist: {detail}")]
    CalendarReference { detail: String },
    #[error("validated import reference could not be resolved: {detail}")]
    MissingReference { detail: String },
    #[error("audience `{audience}` has no students")]
    EmptyAudience { audience: String },
    #[error("meeting demand has no legal calendar start: {detail}")]
    NoLegalStart { detail: String },
    #[error(
        "student choices require sectioning before timetable compilation ({choice_count} choices)"
    )]
    UnresolvedSectioning { choice_count: usize },
    #[error("generated sectioning cannot be overlaid on existing sections or enrollments")]
    SectioningModeConflict,
    #[error("generated sectioning was prepared for a different project stable key")]
    SectioningProjectKeyMismatch,
    #[error("a compact index exceeded u32")]
    IndexOverflow,
    #[error("semantic scheduling snapshot is invalid: {0}")]
    Scheduling(#[from] SchedulingError),
}

impl CompileError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CalendarInvalid { .. } => "APPLICATION_CALENDAR_INVALID",
            Self::CalendarReference { .. } => "APPLICATION_CALENDAR_REFERENCE_NOT_FOUND",
            Self::MissingReference { .. } => "APPLICATION_IMPORT_REFERENCE_NOT_FOUND",
            Self::EmptyAudience { .. } => "APPLICATION_AUDIENCE_EMPTY",
            Self::NoLegalStart { .. } => "APPLICATION_MEETING_NO_LEGAL_START",
            Self::UnresolvedSectioning { .. } => "APPLICATION_SECTIONING_REQUIRED",
            Self::SectioningModeConflict => "APPLICATION_SECTIONING_MODE_CONFLICT",
            Self::SectioningProjectKeyMismatch => "APPLICATION_SECTIONING_PROJECT_KEY_MISMATCH",
            Self::IndexOverflow => "APPLICATION_COMPACT_INDEX_OVERFLOW",
            Self::Scheduling(_) => "APPLICATION_SCHEDULING_SNAPSHOT_INVALID",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum AudienceRef<'a> {
    AdministrativeClass(&'a str),
    TeachingSection(&'a str),
}

impl<'a> AudienceRef<'a> {
    const fn kind(self) -> ImportedAudienceKind {
        match self {
            Self::AdministrativeClass(_) => ImportedAudienceKind::AdministrativeClass,
            Self::TeachingSection(_) => ImportedAudienceKind::TeachingSection,
        }
    }

    const fn code(self) -> &'a str {
        match self {
            Self::AdministrativeClass(code) | Self::TeachingSection(code) => code,
        }
    }
}

#[derive(Debug)]
struct Offering<'a> {
    plan: &'a CoursePlanImportRow,
    audience: AudienceRef<'a>,
    explicit: Option<&'a CourseOfferingImportRow>,
    section: Option<&'a TeachingSectionImportRow>,
}

#[derive(Debug)]
struct DenseCatalogs<'a> {
    student_index: BTreeMap<&'a str, usize>,
    teacher_index: BTreeMap<&'a str, TeacherIndex>,
    room_index: BTreeMap<&'a str, RoomIndex>,
    class_ids: BTreeMap<&'a str, AdministrativeClassId>,
    section_ids: BTreeMap<&'a str, TeachingSectionId>,
}

/// Compiles a validated import batch into a solver-independent semantic snapshot.
///
/// # Errors
///
/// Returns a structured error for an unresolved calendar/reference, empty audience, impossible
/// duration, or semantic snapshot invariant failure.
#[allow(clippy::too_many_lines)]
pub fn compile_import_batch(
    batch: &ImportBatch,
    calendar: &CalendarDefinition,
    project_stable_key: &str,
) -> Result<CompiledSchoolProblem, CompileError> {
    if batch.teaching_sections().is_empty() && !batch.student_subject_choices().is_empty() {
        return Err(CompileError::UnresolvedSectioning {
            choice_count: batch.student_subject_choices().len(),
        });
    }
    compile_import_batch_parts(
        batch,
        batch.teaching_sections(),
        batch.section_enrollments(),
        calendar,
        project_stable_key,
    )
}

/// Compiles a validated Input-B import with one independently validated generated candidate.
///
/// # Errors
///
/// Rejects mixed imported/generated sectioning, project-key drift, or any normal compilation
/// invariant failure.
pub fn compile_import_batch_with_sectioning(
    batch: &ImportBatch,
    calendar: &CalendarDefinition,
    project_stable_key: &str,
    sectioning: &PreparedSectioningCandidate,
) -> Result<CompiledSchoolProblem, CompileError> {
    if !batch.teaching_sections().is_empty() || !batch.section_enrollments().is_empty() {
        return Err(CompileError::SectioningModeConflict);
    }
    if !sectioning.matches_project_key(project_stable_key) {
        return Err(CompileError::SectioningProjectKeyMismatch);
    }
    compile_import_batch_parts(
        batch,
        &sectioning.sections,
        &sectioning.enrollments,
        calendar,
        project_stable_key,
    )
}

#[allow(clippy::too_many_lines)]
fn compile_import_batch_parts<'a>(
    batch: &'a ImportBatch,
    sections: &'a [TeachingSectionImportRow],
    section_enrollments: &'a [class_schedule_import::SectionEnrollmentImportRow],
    calendar: &CalendarDefinition,
    project_stable_key: &str,
) -> Result<CompiledSchoolProblem, CompileError> {
    let calendar = CalendarDefinition::new(calendar.days.clone(), calendar.periods.clone())?;
    let mut student_rows = batch.students().iter().collect::<Vec<_>>();
    student_rows.sort_by_key(|row| row.student_code.as_str());
    let students = student_rows
        .iter()
        .map(|row| stable_id(project_stable_key, "student", &row.student_code))
        .collect::<Vec<StudentId>>();
    let student_index = student_rows
        .iter()
        .enumerate()
        .map(|(index, row)| (row.student_code.as_str(), index))
        .collect::<BTreeMap<_, _>>();

    let (timeslots, timeslot_lookup, timeslot_labels) =
        compile_timeslots(&calendar, project_stable_key)?;
    let slot_count = timeslots.len();
    let mut teacher_rows = batch.teachers().iter().collect::<Vec<_>>();
    teacher_rows.sort_by_key(|row| row.teacher_code.as_str());
    let teacher_index = teacher_rows
        .iter()
        .enumerate()
        .map(|(index, row)| Ok((row.teacher_code.as_str(), TeacherIndex(compact_u32(index)?))))
        .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
    let unavailable = batch
        .teacher_unavailability()
        .iter()
        .map(|row| {
            let slot = timeslot_lookup
                .get(&(imported_day(row.day), row.period))
                .copied()
                .ok_or_else(|| CompileError::CalendarReference {
                    detail: format!("teacher_unavailability row {}", row.row),
                })?;
            Ok((row.teacher_code.as_str(), slot))
        })
        .collect::<Result<BTreeSet<_>, CompileError>>()?;
    let teachers = teacher_rows
        .iter()
        .map(|row| {
            let available = (0..slot_count)
                .filter(|slot| !unavailable.contains(&(row.teacher_code.as_str(), *slot)));
            Ok(DenseTeacher {
                stable_id: stable_id(project_stable_key, "teacher", &row.teacher_code),
                available: DenseBitSet::from_indices(slot_count, available)?,
            })
        })
        .collect::<Result<Vec<_>, SchedulingError>>()?;

    let mut room_rows = batch.rooms().iter().collect::<Vec<_>>();
    room_rows.sort_by_key(|row| row.room_code.as_str());
    let room_index = room_rows
        .iter()
        .enumerate()
        .map(|(index, row)| Ok((row.room_code.as_str(), RoomIndex(compact_u32(index)?))))
        .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
    let rooms = room_rows
        .iter()
        .map(|row| DenseRoom {
            stable_id: stable_id(project_stable_key, "room", &row.room_code),
            building_id: stable_id(project_stable_key, "building", &row.building_code),
            capacity: row.capacity,
            features: row.features.iter().cloned().collect(),
            available: DenseBitSet::full(slot_count),
        })
        .collect::<Vec<_>>();
    let class_ids = batch
        .administrative_classes()
        .iter()
        .map(|row| {
            (
                row.administrative_class_code.as_str(),
                stable_id(
                    project_stable_key,
                    "administrative_class",
                    &row.administrative_class_code,
                ),
            )
        })
        .collect();
    let section_ids = sections
        .iter()
        .map(|row| {
            (
                row.section_code.as_str(),
                stable_id(project_stable_key, "teaching_section", &row.section_code),
            )
        })
        .collect();
    let catalogs = DenseCatalogs {
        student_index,
        teacher_index,
        room_index,
        class_ids,
        section_ids,
    };
    let audiences = compile_audiences(
        batch,
        sections,
        section_enrollments,
        students.len(),
        &catalogs,
    )?;
    let mut activities = Vec::new();
    let mut patterns = Vec::new();
    let mut labels = Vec::new();
    let mut activity_keys = BTreeMap::new();

    for offering in collect_offerings(batch, sections) {
        let audience_key = (offering.audience.kind(), offering.audience.code());
        let audience = audiences.get(&audience_key).cloned().ok_or_else(|| {
            CompileError::MissingReference {
                detail: format!("audience {}", offering.audience.code()),
            }
        })?;
        if audience.is_empty() {
            return Err(CompileError::EmptyAudience {
                audience: offering.audience.code().to_owned(),
            });
        }
        let offering_key = format!(
            "{}:{:?}:{}",
            offering.plan.course_plan_code,
            offering.audience.kind(),
            offering.audience.code()
        );
        let offering_id = stable_id(project_stable_key, "course_offering", &offering_key);
        let plan_id = stable_id(
            project_stable_key,
            "course_plan",
            &offering.plan.course_plan_code,
        );
        let subject_id = stable_id(project_stable_key, "subject", &offering.plan.subject_code);
        let teacher = compile_teacher_requirement(
            offering
                .explicit
                .map_or_else(|| default_teacher(&offering), |row| &row.teacher_assignment),
            &catalogs.teacher_index,
        )?;
        let room_policy = offering
            .explicit
            .and_then(|row| row.room_policy.as_ref())
            .or(offering.plan.room_policy.as_ref())
            .or_else(|| offering.section.map(|section| &section.room_policy));
        let room = compile_room_requirement(room_policy, offering.audience, batch, &catalogs)?;
        let starts = legal_starts(&calendar, offering.plan, &timeslots)?;
        let first_activity = activities.len();
        for (ordinal_index, duration) in offering.plan.meeting_pattern.iter().copied().enumerate() {
            let ordinal =
                u16::try_from(ordinal_index + 1).map_err(|_| CompileError::IndexOverflow)?;
            let allowed_starts = starts.get(&duration).cloned().unwrap_or_default();
            if allowed_starts.is_empty() {
                return Err(CompileError::NoLegalStart {
                    detail: format!("{offering_key}:{ordinal}"),
                });
            }
            let activity_index = activities.len();
            activities.push(Activity {
                stable_id: stable_id(
                    project_stable_key,
                    "meeting_demand",
                    &format!("{offering_key}:{ordinal}"),
                ),
                course_offering_id: offering_id,
                subject_id,
                course_plan_id: plan_id,
                teaching_section_id: match offering.audience {
                    AudienceRef::TeachingSection(code) => Some(catalogs.section_ids[code]),
                    AudienceRef::AdministrativeClass(_) => None,
                },
                administrative_class_id: match offering.audience {
                    AudienceRef::AdministrativeClass(code) => Some(catalogs.class_ids[code]),
                    AudienceRef::TeachingSection(_) => None,
                },
                duration_periods: duration,
                may_cross_breaks: offering.plan.may_cross_breaks,
                allowed_starts,
                audience: audience.clone(),
                teacher: teacher.clone(),
                room: room.clone(),
                required_capacity: u16::try_from(audience.count())
                    .map_err(|_| CompileError::IndexOverflow)?,
                required_room_features: offering
                    .plan
                    .required_room_features
                    .iter()
                    .cloned()
                    .collect(),
            });
            activity_keys.insert(
                (
                    offering.plan.course_plan_code.as_str(),
                    offering.audience.kind(),
                    offering.audience.code(),
                    ordinal,
                ),
                ActivityIndex(compact_u32(activity_index)?),
            );
            labels.push(CompiledActivityLabel {
                course_plan_code: offering.plan.course_plan_code.clone(),
                audience_kind: offering.audience.kind(),
                audience_code: offering.audience.code().to_owned(),
                meeting_ordinal: ordinal,
                duration_periods: duration,
            });
        }
        patterns.push(MeetingPatternRule {
            course_offering_id: offering_id,
            course_plan_id: plan_id,
            activities: (first_activity..activities.len())
                .map(|index| Ok(ActivityIndex(compact_u32(index)?)))
                .collect::<Result<Vec<_>, CompileError>>()?,
            minimum_gap_days: offering.plan.min_days_between,
            maximum_periods_per_day: u8::try_from(offering.plan.max_periods_per_day)
                .map_err(|_| CompileError::IndexOverflow)?,
        });
    }
    let locks = compile_fixed_activities(
        batch,
        &activity_keys,
        &timeslot_lookup,
        &catalogs,
        &activities,
    )?;
    let problem = SchedulingProblemDraft {
        schema_version: 1,
        students,
        teachers,
        rooms,
        timeslots,
        activities,
        meeting_patterns: patterns,
        locks,
    }
    .try_into()?;
    Ok(CompiledSchoolProblem {
        problem,
        catalog: CompiledCatalog {
            student_codes: student_rows
                .into_iter()
                .map(|row| row.student_code.clone())
                .collect(),
            teacher_codes: teacher_rows
                .into_iter()
                .map(|row| row.teacher_code.clone())
                .collect(),
            room_codes: room_rows
                .into_iter()
                .map(|row| row.room_code.clone())
                .collect(),
            timeslot_labels,
            activities: labels,
        },
    })
}

type TimeslotLookup = BTreeMap<(Day, u16), usize>;

fn compile_timeslots(
    calendar: &CalendarDefinition,
    project_key: &str,
) -> Result<(Vec<DenseTimeslot>, TimeslotLookup, Vec<String>), CompileError> {
    let mut timeslots = Vec::new();
    let mut lookup = BTreeMap::new();
    let mut labels = Vec::new();
    for day in &calendar.days {
        for (period_position, period) in calendar.periods.iter().enumerate() {
            let index = timeslots.len();
            lookup.insert((*day, period.index), index);
            let next_consecutive = if period_position + 1 < calendar.periods.len() {
                Some(TimeslotIndex(compact_u32(index + 1)?))
            } else {
                None
            };
            timeslots.push(DenseTimeslot {
                stable_id: stable_id(
                    project_key,
                    "timeslot",
                    &format!("{}:{}", day_number(*day), period.index),
                ),
                day: *day,
                period_index: period.index,
                instructional_block: period.instructional_block,
                next_consecutive,
            });
            labels.push(format!("{} {}", day_label(*day), period.label));
        }
    }
    Ok((timeslots, lookup, labels))
}

fn compile_audiences<'a>(
    batch: &'a ImportBatch,
    sections: &'a [TeachingSectionImportRow],
    section_enrollments: &'a [class_schedule_import::SectionEnrollmentImportRow],
    student_count: usize,
    catalogs: &DenseCatalogs<'a>,
) -> Result<BTreeMap<(ImportedAudienceKind, &'a str), DenseBitSet>, CompileError> {
    let mut values = BTreeMap::new();
    for class in batch.administrative_classes() {
        let members = batch
            .students()
            .iter()
            .filter(|student| student.administrative_class_code == class.administrative_class_code)
            .map(|student| catalogs.student_index[student.student_code.as_str()]);
        values.insert(
            (
                ImportedAudienceKind::AdministrativeClass,
                class.administrative_class_code.as_str(),
            ),
            DenseBitSet::from_indices(student_count, members)?,
        );
    }
    for section in sections {
        let members = section_enrollments
            .iter()
            .filter(|enrollment| enrollment.section_code == section.section_code)
            .map(|enrollment| catalogs.student_index[enrollment.student_code.as_str()]);
        values.insert(
            (
                ImportedAudienceKind::TeachingSection,
                section.section_code.as_str(),
            ),
            DenseBitSet::from_indices(student_count, members)?,
        );
    }
    Ok(values)
}

fn collect_offerings<'a>(
    batch: &'a ImportBatch,
    sections: &'a [TeachingSectionImportRow],
) -> Vec<Offering<'a>> {
    let explicit = batch
        .course_offerings()
        .iter()
        .map(|row| {
            (
                (
                    row.course_plan_code.as_str(),
                    row.audience_kind,
                    row.audience_code.as_str(),
                ),
                row,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut plans = batch.course_plans().iter().collect::<Vec<_>>();
    plans.sort_by_key(|plan| plan.course_plan_code.as_str());
    let mut offerings = Vec::new();
    for plan in plans {
        match plan.audience_kind {
            ImportedAudienceKind::AdministrativeClass => {
                let mut classes = batch
                    .administrative_classes()
                    .iter()
                    .filter(|class| class.grade_code == plan.grade_code)
                    .collect::<Vec<_>>();
                classes.sort_by_key(|class| class.administrative_class_code.as_str());
                for class in classes {
                    let audience =
                        AudienceRef::AdministrativeClass(class.administrative_class_code.as_str());
                    offerings.push(Offering {
                        plan,
                        audience,
                        explicit: explicit
                            .get(&(
                                plan.course_plan_code.as_str(),
                                audience.kind(),
                                audience.code(),
                            ))
                            .copied(),
                        section: None,
                    });
                }
            }
            ImportedAudienceKind::TeachingSection => {
                let mut sections = sections
                    .iter()
                    .filter(|section| {
                        section.grade_code == plan.grade_code
                            && section.subject_code == plan.subject_code
                    })
                    .collect::<Vec<_>>();
                sections.sort_by_key(|section| section.section_code.as_str());
                for section in sections {
                    let audience = AudienceRef::TeachingSection(section.section_code.as_str());
                    offerings.push(Offering {
                        plan,
                        audience,
                        explicit: explicit
                            .get(&(
                                plan.course_plan_code.as_str(),
                                audience.kind(),
                                audience.code(),
                            ))
                            .copied(),
                        section: Some(section),
                    });
                }
            }
        }
    }
    offerings
}

fn default_teacher<'a>(offering: &'a Offering<'a>) -> &'a ImportedTeacherAssignment {
    offering
        .section
        .map_or(&offering.plan.teacher_assignment, |section| {
            &section.teacher_assignment
        })
}

fn compile_teacher_requirement(
    assignment: &ImportedTeacherAssignment,
    teachers: &BTreeMap<&str, TeacherIndex>,
) -> Result<TeacherRequirement, CompileError> {
    let resolve = |code: &str| {
        teachers
            .get(code)
            .copied()
            .ok_or_else(|| CompileError::MissingReference {
                detail: format!("teacher {code}"),
            })
    };
    match assignment {
        ImportedTeacherAssignment::Fixed { teacher_code } => Ok(TeacherRequirement::Fixed {
            teacher: resolve(teacher_code)?,
        }),
        ImportedTeacherAssignment::Candidates { teacher_codes } => {
            Ok(TeacherRequirement::Candidates {
                teachers: teacher_codes
                    .iter()
                    .map(|code| resolve(code))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        }
    }
}

fn compile_room_requirement(
    policy: Option<&ImportedRoomPolicy>,
    audience: AudienceRef<'_>,
    batch: &ImportBatch,
    catalogs: &DenseCatalogs<'_>,
) -> Result<RoomRequirement, CompileError> {
    let effective = if let Some(policy) = policy {
        policy
    } else {
        match audience {
            AudienceRef::AdministrativeClass(_) => &ImportedRoomPolicy::AdminHomeRoom,
            AudienceRef::TeachingSection(code) => {
                return Err(CompileError::MissingReference {
                    detail: format!("room policy for teaching section {code}"),
                });
            }
        }
    };
    match effective {
        ImportedRoomPolicy::AdminHomeRoom => {
            let AudienceRef::AdministrativeClass(class_code) = audience else {
                return Err(CompileError::MissingReference {
                    detail: "AdminHomeRoom requires an administrative class".to_owned(),
                });
            };
            let class = batch
                .administrative_classes()
                .iter()
                .find(|class| class.administrative_class_code == class_code)
                .ok_or_else(|| CompileError::MissingReference {
                    detail: format!("administrative class {class_code}"),
                })?;
            Ok(RoomRequirement::AdminHomeRoom {
                room: resolve_room(&class.home_room_code, &catalogs.room_index)?,
            })
        }
        ImportedRoomPolicy::Fixed { room_code } => Ok(RoomRequirement::Fixed {
            room: resolve_room(room_code, &catalogs.room_index)?,
        }),
        ImportedRoomPolicy::SectionFixed {
            candidate_room_codes,
        } => Ok(RoomRequirement::SectionFixed {
            section_id: audience_section_id(audience, catalogs)?,
            candidate_rooms: map_rooms(candidate_room_codes, &catalogs.room_index)?,
        }),
        ImportedRoomPolicy::PreferredFixed {
            preferred_room_codes,
            fallback_room_codes,
        } => Ok(RoomRequirement::PreferredFixed {
            section_id: audience_section_id(audience, catalogs)?,
            preferred_rooms: map_rooms(preferred_room_codes, &catalogs.room_index)?,
            fallback_rooms: map_rooms(fallback_room_codes, &catalogs.room_index)?,
        }),
        ImportedRoomPolicy::Flexible {
            candidate_room_codes,
        } => Ok(RoomRequirement::Flexible {
            candidate_rooms: map_rooms(candidate_room_codes, &catalogs.room_index)?,
        }),
    }
}

fn audience_section_id(
    audience: AudienceRef<'_>,
    catalogs: &DenseCatalogs<'_>,
) -> Result<TeachingSectionId, CompileError> {
    let AudienceRef::TeachingSection(code) = audience else {
        return Err(CompileError::MissingReference {
            detail: "fixed section room policy requires a teaching section".to_owned(),
        });
    };
    catalogs
        .section_ids
        .get(code)
        .copied()
        .ok_or_else(|| CompileError::MissingReference {
            detail: format!("teaching section {code}"),
        })
}

fn resolve_room(code: &str, rooms: &BTreeMap<&str, RoomIndex>) -> Result<RoomIndex, CompileError> {
    rooms
        .get(code)
        .copied()
        .ok_or_else(|| CompileError::MissingReference {
            detail: format!("room {code}"),
        })
}

fn map_rooms(
    codes: &[String],
    rooms: &BTreeMap<&str, RoomIndex>,
) -> Result<Vec<RoomIndex>, CompileError> {
    codes.iter().map(|code| resolve_room(code, rooms)).collect()
}

fn legal_starts(
    calendar: &CalendarDefinition,
    plan: &CoursePlanImportRow,
    timeslots: &[DenseTimeslot],
) -> Result<BTreeMap<u8, Vec<TimeslotIndex>>, CompileError> {
    let mut starts = BTreeMap::new();
    for duration in plan
        .meeting_pattern
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
    {
        let mut legal = Vec::new();
        for start in 0..timeslots.len() {
            if duration_fits(timeslots, start, duration, plan.may_cross_breaks) {
                legal.push(TimeslotIndex(compact_u32(start)?));
            }
        }
        starts.insert(duration, legal);
    }
    let expected_slots = calendar.days.len() * calendar.periods.len();
    if expected_slots != timeslots.len() {
        return Err(CompileError::CalendarInvalid {
            detail: "compiled timeslot cardinality differs from calendar".to_owned(),
        });
    }
    Ok(starts)
}

fn duration_fits(
    timeslots: &[DenseTimeslot],
    start: usize,
    duration: u8,
    may_cross_breaks: bool,
) -> bool {
    if duration == 0 {
        return false;
    }
    let Some(first) = timeslots.get(start) else {
        return false;
    };
    let first_block = first.instructional_block;
    let mut current = start;
    for offset in 0..duration {
        let Some(slot) = timeslots.get(current) else {
            return false;
        };
        if !may_cross_breaks && slot.instructional_block != first_block {
            return false;
        }
        if offset + 1 < duration {
            let Some(next) = slot.next_consecutive else {
                return false;
            };
            current = next.as_usize();
        }
    }
    true
}

fn compile_fixed_activities(
    batch: &ImportBatch,
    activities_by_key: &BTreeMap<(&str, ImportedAudienceKind, &str, u16), ActivityIndex>,
    timeslots: &TimeslotLookup,
    catalogs: &DenseCatalogs<'_>,
    activities: &[Activity],
) -> Result<Vec<LockedAssignment>, CompileError> {
    batch
        .fixed_activities()
        .iter()
        .map(|row| {
            let activity = activities_by_key
                .get(&(
                    row.course_plan_code.as_str(),
                    row.audience_kind,
                    row.audience_code.as_str(),
                    row.meeting_ordinal,
                ))
                .copied()
                .ok_or_else(|| CompileError::MissingReference {
                    detail: format!("fixed activity row {} meeting demand", row.row),
                })?;
            let start_index = timeslots
                .get(&(imported_day(row.day), row.period))
                .copied()
                .ok_or_else(|| CompileError::CalendarReference {
                    detail: format!("fixed activity row {}", row.row),
                })?;
            let start = TimeslotIndex(compact_u32(start_index)?);
            if !activities[activity.as_usize()]
                .allowed_starts
                .contains(&start)
            {
                return Err(CompileError::NoLegalStart {
                    detail: format!("fixed activity row {}", row.row),
                });
            }
            Ok(LockedAssignment {
                assignment: Assignment {
                    activity,
                    start,
                    room: resolve_room(&row.room_code, &catalogs.room_index)?,
                    teacher: catalogs
                        .teacher_index
                        .get(row.teacher_code.as_str())
                        .copied()
                        .ok_or_else(|| CompileError::MissingReference {
                            detail: format!("teacher {}", row.teacher_code),
                        })?,
                },
            })
        })
        .collect()
}

pub(crate) fn stable_id<T>(project_key: &str, kind: &str, external_key: &str) -> T
where
    T: From<Uuid>,
{
    let mut hasher = blake3::Hasher::new();
    for component in [project_key, kind, external_key] {
        hasher.update(&(component.len() as u64).to_le_bytes());
        hasher.update(component.as_bytes());
    }
    let hash = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    T::from(Uuid::from_bytes(bytes))
}

fn compact_u32(value: usize) -> Result<u32, CompileError> {
    u32::try_from(value).map_err(|_| CompileError::IndexOverflow)
}

const fn imported_day(day: ImportedDay) -> Day {
    match day {
        ImportedDay::Monday => Day::Monday,
        ImportedDay::Tuesday => Day::Tuesday,
        ImportedDay::Wednesday => Day::Wednesday,
        ImportedDay::Thursday => Day::Thursday,
        ImportedDay::Friday => Day::Friday,
        ImportedDay::Saturday => Day::Saturday,
        ImportedDay::Sunday => Day::Sunday,
    }
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

const fn day_label(day: Day) -> &'static str {
    match day {
        Day::Monday => "周一",
        Day::Tuesday => "周二",
        Day::Wednesday => "周三",
        Day::Thursday => "周四",
        Day::Friday => "周五",
        Day::Saturday => "周六",
        Day::Sunday => "周日",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use class_schedule_import::{CsvImporter, CsvSource, DatasetKind, ImportConfig};
    use class_schedule_scheduling::{RoomRequirement, TeacherRequirement, TimeslotIndex};

    use super::*;

    #[allow(clippy::too_many_lines)]
    fn imported_batch(fixed_common_period: Option<u16>) -> ImportBatch {
        let mut files = BTreeMap::<DatasetKind, Vec<u8>>::new();
        files.insert(
            DatasetKind::Students,
            b"student_code,name,administrative_class_code\nS1,Student 1,AC1\nS2,Student 2,AC2\n"
                .to_vec(),
        );
        files.insert(
            DatasetKind::AdministrativeClasses,
            concat!(
                "administrative_class_code,name,grade_code,home_room_code\n",
                "AC1,Class 1,G12,RHOME1\n",
                "AC2,Class 2,G12,RHOME2\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::StudentSubjectChoices,
            b"student_code,subject_code\nS1,physics\nS2,physics\n".to_vec(),
        );
        files.insert(
            DatasetKind::Teachers,
            concat!(
                "teacher_code,name\n",
                "TA1,Admin 1\n",
                "TA2,Admin 2\n",
                "TPLAN,Plan default\n",
                "TSEC1,Section 1\n",
                "TSEC2,Section 2\n",
                "TOV,Offering override\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::Rooms,
            concat!(
                "room_code,name,building_code,capacity,features\n",
                "RHOME1,Home 1,B1,30,\n",
                "RHOME2,Home 2,B1,30,\n",
                "ROV,Override,B2,30,\n",
                "RPLAN,Plan room,B2,30,\n",
                "RSEC1,Section room 1,B2,30,\n",
                "RSEC2,Section room 2,B2,30,\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::CoursePlans,
            concat!(
                "course_plan_code,name,grade_code,subject_code,audience_kind,weekly_periods,meeting_pattern,min_days_between,max_periods_per_day,may_cross_breaks,required_room_features,teacher_assignment,teacher_codes,room_policy,room_candidates,preferred_rooms,fallback_rooms\n",
                "CP-COMMON,Common,G12,chinese,administrative_class,2,2,0,2,false,,fixed,TPLAN,,,,\n",
                "CP-PHYS,Physics,G12,physics,teaching_section,2,1;1,1,1,false,,fixed,TPLAN,fixed,RPLAN,,\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::TeachingSections,
            concat!(
                "section_code,name,grade_code,subject_code,min_size,target_size,max_size,room_policy,room_candidates,preferred_rooms,fallback_rooms,teacher_assignment,teacher_codes\n",
                "SEC1,Physics 1,G12,physics,1,1,2,fixed,RSEC1,,,fixed,TSEC1\n",
                "SEC2,Physics 2,G12,physics,1,1,2,fixed,RSEC2,,,fixed,TSEC2\n",
            )
            .as_bytes()
            .to_vec(),
        );
        files.insert(
            DatasetKind::SectionEnrollments,
            b"section_code,student_code\nSEC1,S1\nSEC2,S2\n".to_vec(),
        );
        files.insert(
            DatasetKind::CourseOfferings,
            concat!(
                "course_plan_code,audience_kind,audience_code,teacher_assignment,teacher_codes,room_policy,room_candidates,preferred_rooms,fallback_rooms\n",
                "CP-COMMON,administrative_class,AC1,fixed,TA1,,,,\n",
                "CP-COMMON,administrative_class,AC2,fixed,TA2,,,,\n",
                "CP-PHYS,teaching_section,SEC1,fixed,TOV,fixed,ROV,,\n",
            )
            .as_bytes()
            .to_vec(),
        );
        let fixed = if let Some(period) = fixed_common_period {
            format!(
                concat!(
                    "course_plan_code,audience_kind,audience_code,meeting_ordinal,day,period,duration,room_code,teacher_code\n",
                    "CP-COMMON,administrative_class,AC1,1,monday,{},2,RHOME1,TA1\n",
                ),
                period
            )
        } else {
            concat!(
                "course_plan_code,audience_kind,audience_code,meeting_ordinal,day,period,duration,room_code,teacher_code\n",
                "CP-PHYS,teaching_section,SEC1,1,monday,1,1,ROV,TOV\n",
            )
            .to_owned()
        };
        files.insert(DatasetKind::FixedActivities, fixed.into_bytes());

        CsvImporter::new(ImportConfig::default().with_exact_subject_choices(None))
            .import(
                files
                    .iter()
                    .map(|(&kind, bytes)| CsvSource::new(kind, bytes)),
            )
            .unwrap_or_else(|failure| panic!("import failed: {:?}", failure.problems()))
    }

    fn activity_index(
        catalog: &CompiledCatalog,
        plan: &str,
        audience: &str,
        ordinal: u16,
    ) -> usize {
        catalog
            .activities
            .iter()
            .position(|label| {
                label.course_plan_code == plan
                    && label.audience_code == audience
                    && label.meeting_ordinal == ordinal
            })
            .expect("activity label")
    }

    #[test]
    fn compiles_per_audience_offerings_precedence_conflicts_and_locks() {
        let batch = imported_batch(None);
        let calendar = CalendarDefinition::weekday_with_break(4, 2).unwrap();
        let compiled = compile_import_batch(&batch, &calendar, "project-a").unwrap();
        let problem = &compiled.problem;

        assert_eq!(problem.activities().len(), 6);
        assert_eq!(problem.meeting_patterns().len(), 4);
        let common_ac1 = activity_index(&compiled.catalog, "CP-COMMON", "AC1", 1);
        let common_ac2 = activity_index(&compiled.catalog, "CP-COMMON", "AC2", 1);
        let physics_sec1 = activity_index(&compiled.catalog, "CP-PHYS", "SEC1", 1);
        let physics_sec2 = activity_index(&compiled.catalog, "CP-PHYS", "SEC2", 1);
        assert_ne!(
            problem.activities()[common_ac1].course_offering_id,
            problem.activities()[common_ac2].course_offering_id
        );
        assert_eq!(
            problem.activities()[physics_sec1].course_offering_id,
            problem.activities()[activity_index(&compiled.catalog, "CP-PHYS", "SEC1", 2)]
                .course_offering_id
        );

        let teacher_code = |index: TeacherIndex| &compiled.catalog.teacher_codes[index.as_usize()];
        assert!(matches!(
            problem.activities()[common_ac1].teacher,
            TeacherRequirement::Fixed { teacher } if teacher_code(teacher) == "TA1"
        ));
        assert!(matches!(
            problem.activities()[physics_sec1].teacher,
            TeacherRequirement::Fixed { teacher } if teacher_code(teacher) == "TOV"
        ));
        assert!(matches!(
            problem.activities()[physics_sec2].teacher,
            TeacherRequirement::Fixed { teacher } if teacher_code(teacher) == "TSEC2"
        ));

        let room_code = |index: RoomIndex| &compiled.catalog.room_codes[index.as_usize()];
        assert!(matches!(
            problem.activities()[common_ac1].room,
            RoomRequirement::AdminHomeRoom { room } if room_code(room) == "RHOME1"
        ));
        assert!(matches!(
            problem.activities()[physics_sec1].room,
            RoomRequirement::Fixed { room } if room_code(room) == "ROV"
        ));
        assert!(matches!(
            problem.activities()[physics_sec2].room,
            RoomRequirement::Fixed { room } if room_code(room) == "RPLAN"
        ));

        let conflicts = problem.student_conflict_edges();
        let edge = |left: usize, right: usize| {
            let ordered = if left < right {
                (left, right)
            } else {
                (right, left)
            };
            conflicts
                .iter()
                .any(|&(a, b)| (a.as_usize(), b.as_usize()) == ordered)
        };
        assert!(edge(common_ac1, physics_sec1));
        assert!(!edge(common_ac1, physics_sec2));

        let allowed = &problem.activities()[common_ac1].allowed_starts;
        assert_eq!(allowed.len(), 10);
        assert!(allowed.contains(&TimeslotIndex(0)));
        assert!(!allowed.contains(&TimeslotIndex(1)));
        assert!(allowed.contains(&TimeslotIndex(2)));

        assert_eq!(problem.locks().len(), 1);
        let lock = problem.locks()[0].assignment;
        assert_eq!(lock.activity.as_usize(), physics_sec1);
        assert_eq!(lock.start, TimeslotIndex(0));
        assert_eq!(room_code(lock.room), "ROV");
        assert_eq!(teacher_code(lock.teacher), "TOV");
    }

    #[test]
    fn rejects_a_fixed_double_period_that_crosses_the_break() {
        let batch = imported_batch(Some(2));
        let calendar = CalendarDefinition::weekday_with_break(4, 2).unwrap();

        let error = compile_import_batch(&batch, &calendar, "project-a").unwrap_err();

        assert!(matches!(error, CompileError::NoLegalStart { .. }));
        assert_eq!(error.code(), "APPLICATION_MEETING_NO_LEGAL_START");
    }

    #[test]
    fn stable_ids_are_reproducible_and_project_scoped() {
        let batch = imported_batch(None);
        let calendar = CalendarDefinition::weekday_with_break(4, 2).unwrap();

        let first = compile_import_batch(&batch, &calendar, "project-a").unwrap();
        let repeated = compile_import_batch(&batch, &calendar, "project-a").unwrap();
        let other = compile_import_batch(&batch, &calendar, "project-b").unwrap();

        assert_eq!(first.problem.students(), repeated.problem.students());
        assert_eq!(first.problem.activities(), repeated.problem.activities());
        assert_ne!(first.problem.students(), other.problem.students());
        assert_ne!(
            first.problem.activities()[0].stable_id,
            other.problem.activities()[0].stable_id
        );
    }
}
