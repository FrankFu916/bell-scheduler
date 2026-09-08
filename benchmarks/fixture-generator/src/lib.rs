#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Deterministic synthetic benchmark data for a 480-student Chinese senior-high grade.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use class_schedule_import::DatasetKind;

const ADMIN_CLASS_COUNT: usize = 12;
const STUDENTS_PER_CLASS: usize = 40;
const SECTION_BUCKETS: usize = 4;

const ELECTIVES: [Elective; 6] = [
    Elective::new("PHY", "physics", "物理", "physics_lab", "science", 1),
    Elective::new("CHEM", "chemistry", "化学", "chemistry_lab", "science", 2),
    Elective::new("BIO", "biology", "生物", "biology_lab", "science", 3),
    Elective::new("HIS", "history", "历史", "multimedia", "humanities", 1),
    Elective::new("POL", "politics", "思想政治", "multimedia", "humanities", 2),
    Elective::new("GEO", "geography", "地理", "multimedia", "humanities", 3),
];

const ADMIN_PLANS: [AdminPlan; 9] = [
    AdminPlan::new(
        "CHINESE",
        "chinese",
        "语文",
        5,
        "1;1;1;1;1",
        1,
        "multimedia",
    ),
    AdminPlan::new(
        "MATH",
        "mathematics",
        "数学",
        5,
        "1;1;1;1;1",
        1,
        "multimedia",
    ),
    AdminPlan::new(
        "ENGLISH",
        "english",
        "英语",
        5,
        "1;1;1;1;1",
        1,
        "multimedia",
    ),
    AdminPlan::new("PE", "physical_education", "体育", 2, "1;1", 2, "sports"),
    AdminPlan::new(
        "INFO",
        "information_technology",
        "信息技术",
        2,
        "1;1",
        1,
        "computer",
    ),
    AdminPlan::new("ART", "art", "美术", 1, "1", 0, "art"),
    AdminPlan::new("MUSIC", "music", "音乐", 1, "1", 0, "music"),
    AdminPlan::new("LABOR", "labor", "劳动", 1, "1", 0, "workshop"),
    AdminPlan::new("MEETING", "class_meeting", "班会", 1, "1", 0, "multimedia"),
];

#[derive(Clone, Copy, Debug)]
struct Elective {
    abbreviation: &'static str,
    subject_code: &'static str,
    name: &'static str,
    feature: &'static str,
    track: &'static str,
    monday_period: u16,
}

impl Elective {
    const fn new(
        abbreviation: &'static str,
        subject_code: &'static str,
        name: &'static str,
        feature: &'static str,
        track: &'static str,
        monday_period: u16,
    ) -> Self {
        Self {
            abbreviation,
            subject_code,
            name,
            feature,
            track,
            monday_period,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct AdminPlan {
    abbreviation: &'static str,
    subject_code: &'static str,
    name: &'static str,
    weekly_periods: u16,
    pattern: &'static str,
    minimum_gap_days: u8,
    feature: &'static str,
}

impl AdminPlan {
    const fn new(
        abbreviation: &'static str,
        subject_code: &'static str,
        name: &'static str,
        weekly_periods: u16,
        pattern: &'static str,
        minimum_gap_days: u8,
        feature: &'static str,
    ) -> Self {
        Self {
            abbreviation,
            subject_code,
            name,
            weekly_periods,
            pattern,
            minimum_gap_days,
            feature,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixtureStats {
    pub students: usize,
    pub teachers: usize,
    pub rooms: usize,
    pub sections: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedFixture {
    pub csv: BTreeMap<DatasetKind, String>,
    pub stats: FixtureStats,
}

/// Materializes the deterministic medium fixture in memory.
#[must_use]
pub fn generate_medium_fixture() -> GeneratedFixture {
    let mut csv = BTreeMap::new();
    csv.insert(DatasetKind::Students, students_csv());
    csv.insert(
        DatasetKind::AdministrativeClasses,
        administrative_classes_csv(),
    );
    csv.insert(
        DatasetKind::StudentSubjectChoices,
        student_subject_choices_csv(),
    );
    csv.insert(DatasetKind::Teachers, teachers_csv());
    csv.insert(
        DatasetKind::TeacherUnavailability,
        teacher_unavailability_csv(),
    );
    csv.insert(DatasetKind::Rooms, rooms_csv());
    csv.insert(DatasetKind::CoursePlans, course_plans_csv());
    csv.insert(DatasetKind::TeachingSections, teaching_sections_csv());
    csv.insert(DatasetKind::SectionEnrollments, section_enrollments_csv());
    csv.insert(DatasetKind::CourseOfferings, course_offerings_csv());
    csv.insert(DatasetKind::FixedActivities, fixed_activities_csv());
    GeneratedFixture {
        csv,
        stats: FixtureStats {
            students: ADMIN_CLASS_COUNT * STUDENTS_PER_CLASS,
            teachers: 81,
            rooms: 43,
            sections: ELECTIVES.len() * SECTION_BUCKETS,
        },
    }
}

/// Writes the complete bundle through a sibling staging directory and refuses to overwrite data.
///
/// # Errors
///
/// Returns an I/O error when the output is not a new/empty directory or publication fails.
pub fn write_medium_fixture(output: impl AsRef<Path>) -> io::Result<FixtureStats> {
    let output = output.as_ref();
    if output.exists()
        && (!fs::metadata(output)?.is_dir() || fs::read_dir(output)?.next().transpose()?.is_some())
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "output must be a new or empty directory",
        ));
    }
    let parent = output.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "output has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;
    let name = output
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output name is not UTF-8"))?;
    let staging = staging_path(parent, name)?;
    fs::create_dir(&staging)?;
    let generated = generate_medium_fixture();
    let result = (|| {
        for (kind, contents) in &generated.csv {
            fs::write(staging.join(format!("{}.csv", kind.as_str())), contents)?;
        }
        fs::write(staging.join("README.md"), medium_readme())?;
        if output.exists() {
            fs::remove_dir(output)?;
        }
        fs::rename(&staging, output)?;
        Ok(generated.stats)
    })();
    if result.is_err() && staging.exists() {
        let _ignored = fs::remove_dir_all(staging);
    }
    result
}

fn staging_path(parent: &Path, name: &str) -> io::Result<PathBuf> {
    for attempt in 0..100_u32 {
        let path = parent.join(format!(".{name}.stage-{}-{attempt}", std::process::id()));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate staging directory",
    ))
}

fn students_csv() -> String {
    let mut output = String::from("student_code,name,administrative_class_code\n");
    for class in 1..=ADMIN_CLASS_COUNT {
        for seat in 1..=STUDENTS_PER_CLASS {
            let student = student_number(class, seat);
            writeln!(output, "S{student:04},学生{student:04},AC{class:02}")
                .expect("String writes do not fail");
        }
    }
    output
}

fn administrative_classes_csv() -> String {
    let mut output = String::from("administrative_class_code,name,grade_code,home_room_code\n");
    for class in 1..=ADMIN_CLASS_COUNT {
        writeln!(output, "AC{class:02},高三{class}班,G12,R-HOME-{class:02}")
            .expect("String writes do not fail");
    }
    output
}

fn student_subject_choices_csv() -> String {
    let mut output = String::from("student_code,subject_code\n");
    for class in 1..=ADMIN_CLASS_COUNT {
        for seat in 1..=STUDENTS_PER_CLASS {
            let student = student_number(class, seat);
            for elective in electives_for_seat(seat) {
                writeln!(output, "S{student:04},{}", elective.subject_code)
                    .expect("String writes do not fail");
            }
        }
    }
    output
}

fn teachers_csv() -> String {
    let mut output = String::from("teacher_code,name\n");
    for (abbreviation, name) in [("CHN", "语文"), ("MATH", "数学"), ("ENG", "英语")] {
        for class in 1..=ADMIN_CLASS_COUNT {
            writeln!(output, "T-{abbreviation}-{class:02},{name}教师{class:02}")
                .expect("String writes do not fail");
        }
    }
    for elective in ELECTIVES {
        for bucket in 1..=SECTION_BUCKETS {
            writeln!(
                output,
                "T-{}-{bucket:02},{}教师{bucket:02}",
                elective.abbreviation, elective.name
            )
            .expect("String writes do not fail");
        }
    }
    for (abbreviation, name, count) in [
        ("PE", "体育", 6),
        ("INFO", "信息技术", 6),
        ("ART", "美术", 3),
        ("MUSIC", "音乐", 3),
        ("LABOR", "劳动", 3),
    ] {
        for index in 1..=count {
            writeln!(output, "T-{abbreviation}-{index:02},{name}教师{index:02}")
                .expect("String writes do not fail");
        }
    }
    output
}

fn teacher_unavailability_csv() -> String {
    let teachers = teachers_csv();
    let mut lines = teachers.lines();
    let _header = lines.next();
    let mut output = String::from("teacher_code,day,period\n");
    for line in lines {
        let code = line.split_once(',').map_or(line, |(code, _name)| code);
        writeln!(output, "{code},friday,8").expect("String writes do not fail");
    }
    output
}

fn rooms_csv() -> String {
    let mut output = String::from("room_code,name,building_code,capacity,features\n");
    for class in 1..=ADMIN_CLASS_COUNT {
        writeln!(
            output,
            "R-HOME-{class:02},高三{class}班教室,B-TEACH,40,multimedia"
        )
        .expect("String writes do not fail");
    }
    for elective in ELECTIVES.iter().filter(|item| item.track == "science") {
        for bucket in 1..=SECTION_BUCKETS {
            writeln!(
                output,
                "R-{}-{bucket:02},{}实验室{bucket:02},B-SCI,65,{};multimedia",
                elective.abbreviation, elective.name, elective.feature
            )
            .expect("String writes do not fail");
        }
    }
    for bucket in 1..=SECTION_BUCKETS {
        writeln!(
            output,
            "R-HUM-{bucket:02},人文选科教室{bucket:02},B-TEACH,65,multimedia"
        )
        .expect("String writes do not fail");
    }
    for index in 1..=3 {
        for (code, name, building, feature, capacity) in [
            ("GYM", "体育馆", "B-SPORT", "sports", 200),
            ("COMP", "计算机教室", "B-TECH", "computer", 45),
            ("ART", "美术教室", "B-ART", "art", 45),
            ("MUSIC", "音乐教室", "B-ART", "music", 45),
            ("WORK", "劳动工坊", "B-TECH", "workshop", 45),
        ] {
            writeln!(
                output,
                "R-{code}-{index:02},{name}{index:02},{building},{capacity},{feature}"
            )
            .expect("String writes do not fail");
        }
    }
    output
}

fn course_plans_csv() -> String {
    let mut output = String::from(concat!(
        "course_plan_code,name,grade_code,subject_code,audience_kind,weekly_periods,",
        "meeting_pattern,min_days_between,max_periods_per_day,may_cross_breaks,",
        "required_room_features,teacher_assignment,teacher_codes,room_policy,",
        "room_candidates,preferred_rooms,fallback_rooms\n"
    ));
    for plan in ADMIN_PLANS {
        let teacher_codes = teacher_candidates_for_admin_plan(plan.abbreviation);
        let (room_policy, rooms) = room_policy_for_admin_plan(plan.abbreviation);
        writeln!(
            output,
            "CP-{},{},G12,{},administrative_class,{},{},{},1,false,{},candidates,{},{},{},,",
            plan.abbreviation,
            plan.name,
            plan.subject_code,
            plan.weekly_periods,
            plan.pattern,
            plan.minimum_gap_days,
            plan.feature,
            teacher_codes,
            room_policy,
            rooms
        )
        .expect("String writes do not fail");
    }
    for elective in ELECTIVES {
        let teachers = joined_codes("T", elective.abbreviation, SECTION_BUCKETS);
        writeln!(
            output,
            "CP-{},{},G12,{},teaching_section,5,1;1;1;1;1,1,1,false,{},candidates,{},,,,",
            elective.abbreviation, elective.name, elective.subject_code, elective.feature, teachers
        )
        .expect("String writes do not fail");
    }
    output
}

fn teaching_sections_csv() -> String {
    let mut output = String::from(concat!(
        "section_code,name,grade_code,subject_code,min_size,target_size,max_size,",
        "room_policy,room_candidates,preferred_rooms,fallback_rooms,teacher_assignment,",
        "teacher_codes\n"
    ));
    for elective in ELECTIVES {
        for bucket in 1..=SECTION_BUCKETS {
            let next = bucket % SECTION_BUCKETS + 1;
            let rooms = if elective.track == "science" {
                format!(
                    "R-{}-{bucket:02};R-{}-{next:02}",
                    elective.abbreviation, elective.abbreviation
                )
            } else {
                format!("R-HUM-{bucket:02};R-HUM-{next:02}")
            };
            writeln!(
                output,
                "SEC-{}-{bucket:02},{}选科{bucket:02}班,G12,{},55,60,65,section_fixed,{},,,fixed,T-{}-{bucket:02}",
                elective.abbreviation,
                elective.name,
                elective.subject_code,
                rooms,
                elective.abbreviation
            )
            .expect("String writes do not fail");
        }
    }
    output
}

fn section_enrollments_csv() -> String {
    let mut output = String::from("section_code,student_code\n");
    for class in 1..=ADMIN_CLASS_COUNT {
        for seat in 1..=STUDENTS_PER_CLASS {
            let student = student_number(class, seat);
            let bucket = section_bucket(seat);
            for elective in electives_for_seat(seat) {
                writeln!(
                    output,
                    "SEC-{}-{bucket:02},S{student:04}",
                    elective.abbreviation
                )
                .expect("String writes do not fail");
            }
        }
    }
    output
}

fn course_offerings_csv() -> String {
    let mut output = String::from(concat!(
        "course_plan_code,audience_kind,audience_code,teacher_assignment,teacher_codes,",
        "room_policy,room_candidates,preferred_rooms,fallback_rooms\n"
    ));
    for class in 1..=ADMIN_CLASS_COUNT {
        for plan in ADMIN_PLANS {
            let teacher = assigned_admin_teacher(plan.abbreviation, class);
            writeln!(
                output,
                "CP-{},administrative_class,AC{class:02},fixed,{teacher},,,,",
                plan.abbreviation
            )
            .expect("String writes do not fail");
        }
    }
    output
}

fn fixed_activities_csv() -> String {
    let mut output = String::from(concat!(
        "course_plan_code,audience_kind,audience_code,meeting_ordinal,day,period,",
        "duration,room_code,teacher_code\n"
    ));
    for elective in ELECTIVES {
        for bucket in 1..=SECTION_BUCKETS {
            let room = if elective.track == "science" {
                format!("R-{}-{bucket:02}", elective.abbreviation)
            } else {
                format!("R-HUM-{bucket:02}")
            };
            writeln!(
                output,
                "CP-{},teaching_section,SEC-{}-{bucket:02},1,monday,{},1,{},T-{}-{bucket:02}",
                elective.abbreviation,
                elective.abbreviation,
                elective.monday_period,
                room,
                elective.abbreviation
            )
            .expect("String writes do not fail");
        }
    }
    output
}

fn teacher_candidates_for_admin_plan(abbreviation: &str) -> String {
    match abbreviation {
        "CHINESE" | "MEETING" => joined_codes("T", "CHN", ADMIN_CLASS_COUNT),
        "MATH" => joined_codes("T", "MATH", ADMIN_CLASS_COUNT),
        "ENGLISH" => joined_codes("T", "ENG", ADMIN_CLASS_COUNT),
        "PE" => joined_codes("T", "PE", 6),
        "INFO" => joined_codes("T", "INFO", 6),
        "ART" => joined_codes("T", "ART", 3),
        "MUSIC" => joined_codes("T", "MUSIC", 3),
        "LABOR" => joined_codes("T", "LABOR", 3),
        _ => unreachable!("all admin plans are covered"),
    }
}

fn room_policy_for_admin_plan(abbreviation: &str) -> (&'static str, String) {
    match abbreviation {
        "CHINESE" | "MATH" | "ENGLISH" | "MEETING" => ("admin_home_room", String::new()),
        "PE" => ("flexible", joined_room_codes("GYM")),
        "INFO" => ("flexible", joined_room_codes("COMP")),
        "ART" => ("flexible", joined_room_codes("ART")),
        "MUSIC" => ("flexible", joined_room_codes("MUSIC")),
        "LABOR" => ("flexible", joined_room_codes("WORK")),
        _ => unreachable!("all admin plans are covered"),
    }
}

fn assigned_admin_teacher(abbreviation: &str, class: usize) -> String {
    match abbreviation {
        "CHINESE" | "MEETING" => format!("T-CHN-{class:02}"),
        "MATH" => format!("T-MATH-{class:02}"),
        "ENGLISH" => format!("T-ENG-{class:02}"),
        "PE" => format!("T-PE-{:02}", (class - 1) % 6 + 1),
        "INFO" => format!("T-INFO-{:02}", (class - 1) % 6 + 1),
        "ART" => format!("T-ART-{:02}", (class - 1) % 3 + 1),
        "MUSIC" => format!("T-MUSIC-{:02}", (class - 1) % 3 + 1),
        "LABOR" => format!("T-LABOR-{:02}", (class - 1) % 3 + 1),
        _ => unreachable!("all admin plans are covered"),
    }
}

fn electives_for_seat(seat: usize) -> [Elective; 3] {
    if seat <= STUDENTS_PER_CLASS / 2 {
        [ELECTIVES[0], ELECTIVES[1], ELECTIVES[2]]
    } else {
        [ELECTIVES[3], ELECTIVES[4], ELECTIVES[5]]
    }
}

fn section_bucket(seat: usize) -> usize {
    let track_position = if seat <= STUDENTS_PER_CLASS / 2 {
        seat - 1
    } else {
        seat - STUDENTS_PER_CLASS / 2 - 1
    };
    track_position % SECTION_BUCKETS + 1
}

const fn student_number(class: usize, seat: usize) -> usize {
    (class - 1) * STUDENTS_PER_CLASS + seat
}

fn joined_codes(prefix: &str, abbreviation: &str, count: usize) -> String {
    (1..=count)
        .map(|index| format!("{prefix}-{abbreviation}-{index:02}"))
        .collect::<Vec<_>>()
        .join(";")
}

fn joined_room_codes(abbreviation: &str) -> String {
    (1..=3)
        .map(|index| format!("R-{abbreviation}-{index:02}"))
        .collect::<Vec<_>>()
        .join(";")
}

fn medium_readme() -> &'static str {
    concat!(
        "# Generated medium benchmark fixture\n\n",
        "Synthetic data only: 480 students, 12 administrative classes, six elective subjects,\n",
        "24 teaching sections, 81 teachers, 43 rooms and 40 weekly periods. Every student has\n",
        "exactly three choices. Science and humanities tracks keep the fixture feasible while\n",
        "actual section enrollments, fixed section rooms, specialist features, teacher\n",
        "unavailability and fixed activities remain active. Regenerate with:\n\n",
        "```text\n",
        "cargo run -p class-schedule-fixture-generator -- fixtures/medium\n",
        "```\n"
    )
}

#[cfg(test)]
mod tests {
    use class_schedule_application::{CalendarDefinition, compile_import_batch};
    use class_schedule_import::{CsvImporter, CsvSource, ImportConfig};
    use class_schedule_validation::static_feasibility_check;

    use super::*;

    #[test]
    fn generated_medium_bundle_is_deterministic_importable_and_statically_feasible() {
        let generated = generate_medium_fixture();
        assert_eq!(generated, generate_medium_fixture());
        let batch = CsvImporter::new(ImportConfig::default())
            .import(
                generated
                    .csv
                    .iter()
                    .map(|(kind, contents)| CsvSource::new(*kind, contents.as_bytes())),
            )
            .unwrap_or_else(|failure| panic!("generated import failed: {:?}", failure.problems()));
        let calendar = CalendarDefinition::weekday_with_break(8, 4).unwrap();
        let compiled = compile_import_batch(&batch, &calendar, "fixture-medium").unwrap();

        assert_eq!(compiled.problem.students().len(), 480);
        assert_eq!(compiled.problem.teachers().len(), 81);
        assert_eq!(compiled.problem.rooms().len(), 43);
        assert_eq!(compiled.problem.activities().len(), 396);
        assert_eq!(compiled.problem.meeting_patterns().len(), 132);
        assert_eq!(compiled.problem.locks().len(), 24);
        assert!(static_feasibility_check(&compiled.problem).is_valid());
    }

    #[test]
    fn writer_refuses_to_overwrite_existing_content() {
        let parent = tempfile::tempdir().expect("temporary directory");
        let output = parent.path().join("medium");
        fs::create_dir(&output).expect("output directory");
        fs::write(output.join("keep.txt"), b"keep").expect("existing content");

        let error = write_medium_fixture(&output).expect_err("must refuse overwrite");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(output.join("keep.txt")).unwrap(), b"keep");
    }
}
