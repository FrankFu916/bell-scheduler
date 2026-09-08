use crate::{
    AdministrativeClassImportRow, ColumnMapping, CourseOfferingImportRow, CoursePlanImportRow,
    CsvSource, DatasetKind, FixedActivityImportRow, ImportBatch, ImportConfig, ImportFailure,
    ImportLocation, ImportProblem, ImportProblemCode, ImportedAudienceKind, ImportedDay,
    ImportedRoomPolicy, ImportedTeacherAssignment, ParsedBatch, RoomImportRow,
    SectionEnrollmentImportRow, StudentImportRow, StudentSubjectChoiceImportRow, TeacherImportRow,
    TeacherUnavailabilityImportRow, TeachingSectionImportRow,
};
use csv::{ReaderBuilder, StringRecord, Trim};
use std::collections::{BTreeMap, BTreeSet};

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

#[derive(Clone, Debug)]
pub struct CsvImporter {
    config: ImportConfig,
}

impl CsvImporter {
    #[must_use]
    pub const fn new(config: ImportConfig) -> Self {
        Self { config }
    }

    /// Parses and validates the complete set of supplied CSV datasets without writing state.
    ///
    /// # Errors
    ///
    /// Returns every detected syntax, schema, value, and cross-dataset problem as one immutable
    /// failure. No [`ImportBatch`] is returned when any problem exists.
    pub fn import<'a>(
        &self,
        sources: impl IntoIterator<Item = CsvSource<'a>>,
    ) -> Result<ImportBatch, ImportFailure> {
        let mut parsed = ParsedBatch::default();
        let mut problems = Vec::new();
        let mut seen = BTreeSet::new();

        for source in sources {
            if !seen.insert(source.kind()) {
                problems.push(ImportProblem::new(
                    ImportProblemCode::ImportDuplicateDataset,
                    ImportLocation::dataset(source.kind()),
                ));
                continue;
            }
            self.parse_source(source, &mut parsed, &mut problems);
        }

        if self.config.require_core_datasets() {
            for kind in CORE_DATASETS {
                if !seen.contains(&kind) {
                    problems.push(ImportProblem::new(
                        ImportProblemCode::ImportMissingDataset,
                        ImportLocation::dataset(kind),
                    ));
                }
            }
        }

        crate::validate::validate_batch(&parsed, &self.config, &mut problems);
        if problems.is_empty() {
            Ok(ImportBatch::from_validated(parsed))
        } else {
            Err(ImportFailure::new(problems))
        }
    }

    fn parse_source(
        &self,
        source: CsvSource<'_>,
        parsed: &mut ParsedBatch,
        problems: &mut Vec<ImportProblem>,
    ) {
        let bytes = source
            .bytes()
            .strip_prefix(UTF8_BOM)
            .unwrap_or(source.bytes());
        if std::str::from_utf8(bytes).is_err() {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportInvalidUtf8,
                ImportLocation::dataset(source.kind()),
            ));
            return;
        }

        let mut reader = ReaderBuilder::new()
            .has_headers(true)
            .flexible(false)
            .trim(Trim::All)
            .from_reader(bytes);

        let raw_headers = match reader.headers() {
            Ok(headers) if !headers.is_empty() => headers.clone(),
            Ok(_) => {
                problems.push(ImportProblem::new(
                    ImportProblemCode::ImportMissingHeader,
                    ImportLocation::dataset(source.kind()),
                ));
                return;
            }
            Err(_) => {
                problems.push(ImportProblem::new(
                    ImportProblemCode::ImportCsvSyntax,
                    ImportLocation::row(source.kind(), 1),
                ));
                return;
            }
        };

        let Some(headers) = resolve_headers(
            source.kind(),
            &raw_headers,
            self.config.mapping(source.kind()),
            problems,
        ) else {
            // Header-invalid files are not interpreted under an ambiguous schema.
            return;
        };

        for (record_index, record) in reader.records().enumerate() {
            let record = match record {
                Ok(record) => record,
                Err(error) => {
                    let row = error
                        .position()
                        .map_or(record_index as u64 + 2, csv::Position::line);
                    problems.push(ImportProblem::new(
                        ImportProblemCode::ImportCsvSyntax,
                        ImportLocation::row(source.kind(), row),
                    ));
                    continue;
                }
            };
            let row = record
                .position()
                .map_or(record_index as u64 + 2, csv::Position::line);
            parse_record(source.kind(), row, &headers, &record, parsed, problems);
        }
    }
}

const CORE_DATASETS: [DatasetKind; 6] = [
    DatasetKind::Students,
    DatasetKind::AdministrativeClasses,
    DatasetKind::StudentSubjectChoices,
    DatasetKind::Teachers,
    DatasetKind::Rooms,
    DatasetKind::CoursePlans,
];

/// The same header contract used by the parser, for transport templates and mapping controls.
#[derive(Clone, Debug)]
pub struct CsvDatasetSchema {
    pub kind: DatasetKind,
    pub required_dataset: bool,
    pub required_headers: &'static [&'static str],
    pub optional_headers: &'static [&'static str],
}

#[must_use]
pub fn csv_dataset_schema(kind: DatasetKind) -> CsvDatasetSchema {
    let fields = schema(kind);
    CsvDatasetSchema {
        kind,
        required_dataset: CORE_DATASETS.contains(&kind),
        required_headers: fields.required,
        optional_headers: fields.optional,
    }
}

struct Schema {
    required: &'static [&'static str],
    optional: &'static [&'static str],
}

impl Schema {
    fn allows(&self, value: &str) -> bool {
        self.required.contains(&value) || self.optional.contains(&value)
    }
}

// Keeping the complete external contract in one match makes missing dataset fields auditable.
#[allow(clippy::too_many_lines)]
fn schema(kind: DatasetKind) -> Schema {
    match kind {
        DatasetKind::Students => Schema {
            required: &["student_code", "name", "administrative_class_code"],
            optional: &[],
        },
        DatasetKind::AdministrativeClasses => Schema {
            required: &[
                "administrative_class_code",
                "name",
                "grade_code",
                "home_room_code",
            ],
            optional: &[],
        },
        DatasetKind::StudentSubjectChoices => Schema {
            required: &["student_code", "subject_code"],
            optional: &[],
        },
        DatasetKind::Teachers => Schema {
            required: &["teacher_code", "name"],
            optional: &[],
        },
        DatasetKind::TeacherUnavailability => Schema {
            required: &["teacher_code", "day", "period"],
            optional: &[],
        },
        DatasetKind::Rooms => Schema {
            required: &["room_code", "name", "building_code", "capacity"],
            optional: &["features"],
        },
        DatasetKind::CoursePlans => Schema {
            required: &[
                "course_plan_code",
                "name",
                "grade_code",
                "subject_code",
                "audience_kind",
                "weekly_periods",
                "meeting_pattern",
                "min_days_between",
                "max_periods_per_day",
                "may_cross_breaks",
                "required_room_features",
                "teacher_assignment",
                "teacher_codes",
            ],
            optional: &[
                "room_policy",
                "room_candidates",
                "preferred_rooms",
                "fallback_rooms",
            ],
        },
        DatasetKind::TeachingSections => Schema {
            required: &[
                "section_code",
                "name",
                "grade_code",
                "subject_code",
                "min_size",
                "target_size",
                "max_size",
                "room_policy",
                "room_candidates",
                "preferred_rooms",
                "fallback_rooms",
                "teacher_assignment",
                "teacher_codes",
            ],
            optional: &[],
        },
        DatasetKind::SectionEnrollments => Schema {
            required: &["section_code", "student_code"],
            optional: &[],
        },
        DatasetKind::CourseOfferings => Schema {
            required: &[
                "course_plan_code",
                "audience_kind",
                "audience_code",
                "teacher_assignment",
                "teacher_codes",
            ],
            optional: &[
                "room_policy",
                "room_candidates",
                "preferred_rooms",
                "fallback_rooms",
            ],
        },
        DatasetKind::FixedActivities => Schema {
            required: &[
                "course_plan_code",
                "audience_kind",
                "audience_code",
                "meeting_ordinal",
                "day",
                "period",
                "duration",
                "room_code",
                "teacher_code",
            ],
            optional: &[],
        },
    }
}

#[derive(Debug)]
struct ResolvedHeaders(BTreeMap<String, usize>);

impl ResolvedHeaders {
    fn index(&self, field: &str) -> Option<usize> {
        self.0.get(field).copied()
    }
}

fn resolve_headers(
    kind: DatasetKind,
    raw: &StringRecord,
    mapping: Option<&ColumnMapping>,
    problems: &mut Vec<ImportProblem>,
) -> Option<ResolvedHeaders> {
    let schema = schema(kind);
    let before = problems.len();
    let mut raw_seen = BTreeMap::new();
    let mut resolved = BTreeMap::new();

    for (index, header) in raw.iter().enumerate() {
        if header.is_empty() {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportEmptyHeader,
                ImportLocation::row(kind, 1),
            ));
            continue;
        }
        if let Some(first_index) = raw_seen.insert(header, index) {
            problems.push(
                ImportProblem::new(
                    ImportProblemCode::ImportDuplicateHeader,
                    ImportLocation::cell(kind, 1, header),
                )
                .with_related_row(first_index as u64 + 1),
            );
        }
        let canonical = mapping.map_or(header, |mapping| mapping.resolve(header));
        if !schema.allows(canonical) {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportUnknownHeader,
                ImportLocation::cell(kind, 1, canonical),
            ));
            continue;
        }
        if resolved.insert(canonical.to_owned(), index).is_some() {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportDuplicateMappedField,
                ImportLocation::cell(kind, 1, canonical),
            ));
        }
    }

    if let Some(mapping) = mapping {
        for (source, target) in mapping.entries() {
            if !raw.iter().any(|header| header == source) || !schema.allows(target) {
                problems.push(ImportProblem::new(
                    ImportProblemCode::ImportInvalidColumnMapping,
                    ImportLocation::cell(kind, 1, target),
                ));
            }
        }
    }
    for required in schema.required {
        if !resolved.contains_key(*required) {
            problems.push(ImportProblem::new(
                ImportProblemCode::ImportMissingRequiredHeader,
                ImportLocation::cell(kind, 1, *required),
            ));
        }
    }
    (problems.len() == before).then_some(ResolvedHeaders(resolved))
}

// This is the single dispatch point from untrusted rows into typed dataset rows.
#[allow(clippy::too_many_lines)]
fn parse_record(
    kind: DatasetKind,
    row: u64,
    headers: &ResolvedHeaders,
    record: &StringRecord,
    parsed: &mut ParsedBatch,
    problems: &mut Vec<ImportProblem>,
) {
    let mut cells = Cells {
        kind,
        row,
        headers,
        record,
        problems,
    };
    match kind {
        DatasetKind::Students => {
            let values = (
                cells.text("student_code"),
                cells.text("name"),
                cells.text("administrative_class_code"),
            );
            if let (Some(student_code), Some(name), Some(administrative_class_code)) = values {
                parsed.students.push(StudentImportRow {
                    row,
                    student_code,
                    name,
                    administrative_class_code,
                });
            }
        }
        DatasetKind::AdministrativeClasses => {
            let values = (
                cells.text("administrative_class_code"),
                cells.text("name"),
                cells.text("grade_code"),
                cells.text("home_room_code"),
            );
            if let (
                Some(administrative_class_code),
                Some(name),
                Some(grade_code),
                Some(home_room_code),
            ) = values
            {
                parsed
                    .administrative_classes
                    .push(AdministrativeClassImportRow {
                        row,
                        administrative_class_code,
                        name,
                        grade_code,
                        home_room_code,
                    });
            }
        }
        DatasetKind::StudentSubjectChoices => {
            let values = (cells.text("student_code"), cells.text("subject_code"));
            if let (Some(student_code), Some(subject_code)) = values {
                parsed
                    .student_subject_choices
                    .push(StudentSubjectChoiceImportRow {
                        row,
                        student_code,
                        subject_code,
                    });
            }
        }
        DatasetKind::Teachers => {
            let values = (cells.text("teacher_code"), cells.text("name"));
            if let (Some(teacher_code), Some(name)) = values {
                parsed.teachers.push(TeacherImportRow {
                    row,
                    teacher_code,
                    name,
                });
            }
        }
        DatasetKind::TeacherUnavailability => {
            let values = (
                cells.text("teacher_code"),
                cells.day("day"),
                cells.positive_u16("period"),
            );
            if let (Some(teacher_code), Some(day), Some(period)) = values {
                parsed
                    .teacher_unavailability
                    .push(TeacherUnavailabilityImportRow {
                        row,
                        teacher_code,
                        day,
                        period,
                    });
            }
        }
        DatasetKind::Rooms => {
            let values = (
                cells.text("room_code"),
                cells.text("name"),
                cells.text("building_code"),
                cells.positive_u16("capacity"),
                cells.optional_list("features"),
            );
            if let (
                Some(room_code),
                Some(name),
                Some(building_code),
                Some(capacity),
                Some(features),
            ) = values
            {
                parsed.rooms.push(RoomImportRow {
                    row,
                    room_code,
                    name,
                    building_code,
                    capacity,
                    features,
                });
            }
        }
        DatasetKind::CoursePlans => parse_course_plan(row, &mut cells, parsed),
        DatasetKind::TeachingSections => parse_teaching_section(row, &mut cells, parsed),
        DatasetKind::SectionEnrollments => {
            let values = (cells.text("section_code"), cells.text("student_code"));
            if let (Some(section_code), Some(student_code)) = values {
                parsed.section_enrollments.push(SectionEnrollmentImportRow {
                    row,
                    section_code,
                    student_code,
                });
            }
        }
        DatasetKind::CourseOfferings => parse_course_offering(row, &mut cells, parsed),
        DatasetKind::FixedActivities => parse_fixed_activity(row, &mut cells, parsed),
    }
}

fn parse_course_offering(row: u64, cells: &mut Cells<'_>, parsed: &mut ParsedBatch) {
    let start_problem_count = cells.problems.len();
    let course_plan_code = cells.text("course_plan_code");
    let audience_kind = cells.audience("audience_kind");
    let audience_code = cells.text("audience_code");
    let teacher_assignment = cells.teacher_assignment();
    let room_policy = cells.optional_room_policy();
    if cells.problems.len() == start_problem_count {
        parsed.course_offerings.push(CourseOfferingImportRow {
            row,
            course_plan_code: course_plan_code.expect("validated"),
            audience_kind: audience_kind.expect("validated"),
            audience_code: audience_code.expect("validated"),
            teacher_assignment: teacher_assignment.expect("validated"),
            room_policy: room_policy.expect("validated"),
        });
    }
}

fn parse_course_plan(row: u64, cells: &mut Cells<'_>, parsed: &mut ParsedBatch) {
    let start_problem_count = cells.problems.len();
    let course_plan_code = cells.text("course_plan_code");
    let name = cells.text("name");
    let grade_code = cells.text("grade_code");
    let subject_code = cells.text("subject_code");
    let audience_kind = cells.audience("audience_kind");
    let weekly_periods = cells.positive_u16("weekly_periods");
    let meeting_pattern = cells.duration_list("meeting_pattern");
    let min_days_between = cells.u8("min_days_between");
    let max_periods_per_day = cells.positive_u16("max_periods_per_day");
    let may_cross_breaks = cells.boolean("may_cross_breaks");
    let room_policy = cells.optional_room_policy();
    let required_room_features = cells.optional_list("required_room_features");
    let teacher_assignment = cells.teacher_assignment();
    if cells.problems.len() == start_problem_count {
        parsed.course_plans.push(CoursePlanImportRow {
            row,
            course_plan_code: course_plan_code.expect("validated"),
            name: name.expect("validated"),
            grade_code: grade_code.expect("validated"),
            subject_code: subject_code.expect("validated"),
            audience_kind: audience_kind.expect("validated"),
            weekly_periods: weekly_periods.expect("validated"),
            meeting_pattern: meeting_pattern.expect("validated"),
            min_days_between: min_days_between.expect("validated"),
            max_periods_per_day: max_periods_per_day.expect("validated"),
            may_cross_breaks: may_cross_breaks.expect("validated"),
            room_policy: room_policy.expect("validated"),
            required_room_features: required_room_features.expect("validated"),
            teacher_assignment: teacher_assignment.expect("validated"),
        });
    }
}

fn parse_teaching_section(row: u64, cells: &mut Cells<'_>, parsed: &mut ParsedBatch) {
    let start_problem_count = cells.problems.len();
    let section_code = cells.text("section_code");
    let name = cells.text("name");
    let grade_code = cells.text("grade_code");
    let subject_code = cells.text("subject_code");
    let min_size = cells.u16("min_size");
    let target_size = cells.u16("target_size");
    let max_size = cells.positive_u16("max_size");
    let room_policy = cells.required_room_policy();
    let teacher_assignment = cells.teacher_assignment();
    if cells.problems.len() == start_problem_count {
        parsed.teaching_sections.push(TeachingSectionImportRow {
            row,
            section_code: section_code.expect("validated"),
            name: name.expect("validated"),
            grade_code: grade_code.expect("validated"),
            subject_code: subject_code.expect("validated"),
            min_size: min_size.expect("validated"),
            target_size: target_size.expect("validated"),
            max_size: max_size.expect("validated"),
            room_policy: room_policy.expect("validated"),
            teacher_assignment: teacher_assignment.expect("validated"),
        });
    }
}

fn parse_fixed_activity(row: u64, cells: &mut Cells<'_>, parsed: &mut ParsedBatch) {
    let start_problem_count = cells.problems.len();
    let course_plan_code = cells.text("course_plan_code");
    let audience_kind = cells.audience("audience_kind");
    let audience_code = cells.text("audience_code");
    let meeting_ordinal = cells.positive_u16("meeting_ordinal");
    let day = cells.day("day");
    let period = cells.positive_u16("period");
    let duration = cells.positive_u8("duration");
    let room_code = cells.text("room_code");
    let teacher_code = cells.text("teacher_code");
    if cells.problems.len() == start_problem_count {
        parsed.fixed_activities.push(FixedActivityImportRow {
            row,
            course_plan_code: course_plan_code.expect("validated"),
            audience_kind: audience_kind.expect("validated"),
            audience_code: audience_code.expect("validated"),
            meeting_ordinal: meeting_ordinal.expect("validated"),
            day: day.expect("validated"),
            period: period.expect("validated"),
            duration: duration.expect("validated"),
            room_code: room_code.expect("validated"),
            teacher_code: teacher_code.expect("validated"),
        });
    }
}

struct Cells<'a> {
    kind: DatasetKind,
    row: u64,
    headers: &'a ResolvedHeaders,
    record: &'a StringRecord,
    problems: &'a mut Vec<ImportProblem>,
}

impl Cells<'_> {
    fn raw(&self, field: &str) -> Option<&str> {
        self.headers
            .index(field)
            .and_then(|index| self.record.get(index))
    }

    fn text(&mut self, field: &'static str) -> Option<String> {
        let value = self.raw(field).unwrap_or_default().trim();
        if crate::values::required_text_is_valid(value) {
            Some(value.to_owned())
        } else {
            self.problem(ImportProblemCode::ImportEmptyRequiredValue, field);
            None
        }
    }

    fn optional_text(&self, field: &str) -> Option<String> {
        self.raw(field)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }

    fn u16(&mut self, field: &'static str) -> Option<u16> {
        let value = self.text(field)?;
        value.parse().map_err(|_| ()).map_or_else(
            |()| {
                self.problem(ImportProblemCode::ImportInvalidUnsignedInteger, field);
                None
            },
            Some,
        )
    }

    fn positive_u16(&mut self, field: &'static str) -> Option<u16> {
        self.u16(field).and_then(|value| {
            if value == 0 {
                self.problem(ImportProblemCode::ImportInvalidUnsignedInteger, field);
                None
            } else {
                Some(value)
            }
        })
    }

    fn u8(&mut self, field: &'static str) -> Option<u8> {
        let value = self.text(field)?;
        value.parse().map_err(|_| ()).map_or_else(
            |()| {
                self.problem(ImportProblemCode::ImportInvalidUnsignedInteger, field);
                None
            },
            Some,
        )
    }

    fn positive_u8(&mut self, field: &'static str) -> Option<u8> {
        self.u8(field).and_then(|value| {
            if value == 0 {
                self.problem(ImportProblemCode::ImportInvalidUnsignedInteger, field);
                None
            } else {
                Some(value)
            }
        })
    }

    fn boolean(&mut self, field: &'static str) -> Option<bool> {
        match self.text(field)?.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "是" => Some(true),
            "false" | "0" | "no" | "否" => Some(false),
            _ => {
                self.problem(ImportProblemCode::ImportInvalidBoolean, field);
                None
            }
        }
    }

    fn day(&mut self, field: &'static str) -> Option<ImportedDay> {
        match self.text(field)?.to_ascii_lowercase().as_str() {
            "monday" | "mon" | "1" | "周一" | "星期一" => Some(ImportedDay::Monday),
            "tuesday" | "tue" | "2" | "周二" | "星期二" => Some(ImportedDay::Tuesday),
            "wednesday" | "wed" | "3" | "周三" | "星期三" => Some(ImportedDay::Wednesday),
            "thursday" | "thu" | "4" | "周四" | "星期四" => Some(ImportedDay::Thursday),
            "friday" | "fri" | "5" | "周五" | "星期五" => Some(ImportedDay::Friday),
            "saturday" | "sat" | "6" | "周六" | "星期六" => Some(ImportedDay::Saturday),
            "sunday" | "sun" | "7" | "周日" | "星期日" => Some(ImportedDay::Sunday),
            _ => {
                self.problem(ImportProblemCode::ImportInvalidEnumValue, field);
                None
            }
        }
    }

    fn audience(&mut self, field: &'static str) -> Option<ImportedAudienceKind> {
        match self.text(field)?.to_ascii_lowercase().as_str() {
            "administrative_class" | "admin_class" | "行政班" => {
                Some(ImportedAudienceKind::AdministrativeClass)
            }
            "teaching_section" | "section" | "教学班" => {
                Some(ImportedAudienceKind::TeachingSection)
            }
            _ => {
                self.problem(ImportProblemCode::ImportInvalidEnumValue, field);
                None
            }
        }
    }

    fn optional_list(&mut self, field: &'static str) -> Option<Vec<String>> {
        let Some(value) = self.optional_text(field) else {
            return Some(Vec::new());
        };
        self.parse_list_value(field, &value)
    }

    fn required_list(&mut self, field: &'static str) -> Option<Vec<String>> {
        let value = self.text(field)?;
        self.parse_list_value(field, &value)
    }

    fn parse_list_value(&mut self, field: &'static str, value: &str) -> Option<Vec<String>> {
        let values: Vec<_> = value
            .split([';', '；'])
            .map(str::trim)
            .map(str::to_owned)
            .collect();
        if !crate::values::list_is_valid(&values) {
            self.problem(ImportProblemCode::ImportInvalidList, field);
            return None;
        }
        Some(values)
    }

    fn duration_list(&mut self, field: &'static str) -> Option<Vec<u8>> {
        // Repeated durations are meaningful (`1;1;1;1;1` is the usual five-meeting
        // pattern), unlike candidate/resource lists where duplicates are invalid.
        let value = self.text(field)?;
        let values: Vec<_> = value.split([';', '；']).map(str::trim).collect();
        if values.iter().any(|value| value.is_empty()) {
            self.problem(ImportProblemCode::ImportInvalidList, field);
            return None;
        }
        let mut parsed = Vec::with_capacity(values.len());
        for value in values {
            let Ok(value) = value.parse::<u8>() else {
                self.problem(ImportProblemCode::ImportInvalidList, field);
                return None;
            };
            if value == 0 {
                self.problem(ImportProblemCode::ImportInvalidList, field);
                return None;
            }
            parsed.push(value);
        }
        Some(parsed)
    }

    // Outer Option is parse success; inner Option deliberately represents a valid absent override.
    #[allow(clippy::option_option)]
    fn optional_room_policy(&mut self) -> Option<Option<ImportedRoomPolicy>> {
        let policy = self.optional_text("room_policy");
        let candidates = self.optional_list("room_candidates")?;
        let preferred = self.optional_list("preferred_rooms")?;
        let fallback = self.optional_list("fallback_rooms")?;
        match policy {
            None if candidates.is_empty() && preferred.is_empty() && fallback.is_empty() => {
                Some(None)
            }
            None => {
                self.problem(ImportProblemCode::ImportInvalidRoomPolicy, "room_policy");
                None
            }
            Some(policy) => self
                .parse_room_policy_value(&policy, candidates, preferred, fallback)
                .map(Some),
        }
    }

    fn required_room_policy(&mut self) -> Option<ImportedRoomPolicy> {
        let policy = self.text("room_policy")?;
        let candidates = self.optional_list("room_candidates")?;
        let preferred = self.optional_list("preferred_rooms")?;
        let fallback = self.optional_list("fallback_rooms")?;
        self.parse_room_policy_value(&policy, candidates, preferred, fallback)
    }

    fn parse_room_policy_value(
        &mut self,
        policy: &str,
        candidates: Vec<String>,
        preferred: Vec<String>,
        fallback: Vec<String>,
    ) -> Option<ImportedRoomPolicy> {
        let policy = policy.to_ascii_lowercase();
        let invalid = |this: &mut Self| {
            this.problem(ImportProblemCode::ImportInvalidRoomPolicy, "room_policy");
            None
        };
        match policy.as_str() {
            "admin_home_room" | "行政班教室" | "行政班主教室"
                if candidates.is_empty() && preferred.is_empty() && fallback.is_empty() =>
            {
                Some(ImportedRoomPolicy::AdminHomeRoom)
            }
            "fixed" | "固定教室"
                if candidates.len() == 1 && preferred.is_empty() && fallback.is_empty() =>
            {
                Some(ImportedRoomPolicy::Fixed {
                    room_code: candidates.into_iter().next().expect("one candidate"),
                })
            }
            "section_fixed" | "教学班固定教室"
                if !candidates.is_empty() && preferred.is_empty() && fallback.is_empty() =>
            {
                Some(ImportedRoomPolicy::SectionFixed {
                    candidate_room_codes: candidates,
                })
            }
            "preferred_fixed" | "优先固定教室"
                if candidates.is_empty()
                    && !preferred.is_empty()
                    && !fallback.is_empty()
                    && preferred.iter().all(|room| !fallback.contains(room)) =>
            {
                Some(ImportedRoomPolicy::PreferredFixed {
                    preferred_room_codes: preferred,
                    fallback_room_codes: fallback,
                })
            }
            "flexible" | "灵活教室"
                if !candidates.is_empty() && preferred.is_empty() && fallback.is_empty() =>
            {
                Some(ImportedRoomPolicy::Flexible {
                    candidate_room_codes: candidates,
                })
            }
            _ => invalid(self),
        }
    }

    fn teacher_assignment(&mut self) -> Option<ImportedTeacherAssignment> {
        let assignment = self.text("teacher_assignment")?.to_ascii_lowercase();
        let codes = self.required_list("teacher_codes")?;
        match assignment.as_str() {
            "fixed" | "固定教师" if codes.len() == 1 => {
                Some(ImportedTeacherAssignment::Fixed {
                    teacher_code: codes.into_iter().next().expect("one teacher"),
                })
            }
            "candidates" | "候选教师" if !codes.is_empty() => {
                Some(ImportedTeacherAssignment::Candidates {
                    teacher_codes: codes,
                })
            }
            _ => {
                self.problem(
                    ImportProblemCode::ImportInvalidTeacherAssignment,
                    "teacher_assignment",
                );
                None
            }
        }
    }

    fn problem(&mut self, code: ImportProblemCode, field: &'static str) {
        self.problems.push(ImportProblem::new(
            code,
            ImportLocation::cell(self.kind, self.row, field),
        ));
    }
}
