use class_schedule_application::{
    AdoptRunCommand, AutoSectioningPolicy, CalendarDefinition, CsvImportAuditOptions,
    CsvImportMode, ImportCommitCommand, ImportCommitIntent, ScenarioReceipt, SectioningProfile,
    SolveOptions, StoredProjectSolveCommand, StoredProjectSolveMode, commit_csv_import,
    commit_prepared_scenario_creation, execute_durable_stored_project_solve, prepare_adopt_run,
    prepare_stored_project_solve, save_prepared_solve_artifact,
};
use class_schedule_domain::{ScenarioId, SchoolProjectId};
use class_schedule_import::{CsvSource, DatasetKind};
use class_schedule_persistence::SqliteStore;
use solver_client::{CancellationToken, SidecarSpec, SolverClient};
use std::{collections::BTreeMap, fmt::Write, time::Duration};

pub fn files(unsectioned: bool) -> BTreeMap<DatasetKind, Vec<u8>> {
    let mut files: BTreeMap<_, _> = [
        (DatasetKind::Students, "student_code,name,administrative_class_code\nS1,Student one,AC1\nS2,Student two,AC1\nS3,Student three,AC2\nS4,Student four,AC2\n"),
        (DatasetKind::AdministrativeClasses, "administrative_class_code,name,grade_code,home_room_code\nAC1,Class one,G12,R1\nAC2,Class two,G12,R1\n"),
        (DatasetKind::Teachers, "teacher_code,name\nT1,Teacher one\nT2,Unused teacher\n"),
        (DatasetKind::Rooms, "room_code,name,building_code,capacity,features\nR1,Room one,B1,4,lab\nR2,Unused room,B1,4,lab\n"),
        (DatasetKind::CoursePlans, concat!(
            "course_plan_code,name,grade_code,subject_code,audience_kind,weekly_periods,meeting_pattern,min_days_between,max_periods_per_day,may_cross_breaks,required_room_features,teacher_assignment,teacher_codes,room_policy,room_candidates,preferred_rooms,fallback_rooms\n",
            "P0,Chinese,G12,chinese,administrative_class,1,1,0,1,false,,fixed,T1,admin_home_room,,,\n",
            "P1,Physics,G12,physics,teaching_section,1,1,0,1,false,lab,fixed,T1,section_fixed,R1,,\n",
            "P2,Chemistry,G12,chemistry,teaching_section,1,1,0,1,false,lab,fixed,T1,section_fixed,R1,,\n",
            "P3,Biology,G12,biology,teaching_section,2,2,0,2,false,lab,fixed,T1,section_fixed,R1,,\n",
        )),
    ].into_iter().map(|(kind, value)| (kind, value.as_bytes().to_vec())).collect();
    let mut choices = "student_code,subject_code\n".to_owned();
    for student in ["S1", "S2", "S3", "S4"] {
        for subject in ["physics", "chemistry", "biology"] {
            writeln!(choices, "{student},{subject}").unwrap();
        }
    }
    files.insert(DatasetKind::StudentSubjectChoices, choices.into_bytes());
    if !unsectioned {
        files.insert(DatasetKind::TeachingSections, concat!(
            "section_code,name,grade_code,subject_code,min_size,target_size,max_size,room_policy,room_candidates,preferred_rooms,fallback_rooms,teacher_assignment,teacher_codes\n",
            "PHY1,Physics one,G12,physics,1,2,4,section_fixed,R1,,,fixed,T1\n",
            "PHY2,Physics two,G12,physics,1,2,4,section_fixed,R1,,,fixed,T1\n",
            "CHEM,Chemistry,G12,chemistry,1,4,4,section_fixed,R1,,,fixed,T1\n",
            "BIO,Biology,G12,biology,1,4,4,section_fixed,R1,,,fixed,T1\n",
        ).as_bytes().to_vec());
        files.insert(
            DatasetKind::SectionEnrollments,
            concat!(
                "section_code,student_code\nPHY1,S1\nPHY1,S3\nPHY2,S2\nPHY2,S4\n",
                "CHEM,S1\nCHEM,S2\nCHEM,S3\nCHEM,S4\nBIO,S1\nBIO,S2\nBIO,S3\nBIO,S4\n",
            )
            .as_bytes()
            .to_vec(),
        );
    }
    files
}

fn saved_run(
    store: &mut SqliteStore,
    unsectioned: bool,
    worker_mode: &str,
    candidates: u8,
) -> (SchoolProjectId, String) {
    let project_id = SchoolProjectId::new_v4();
    let policy =
        AutoSectioningPolicy::new(1, 2, 4, 99, SectioningProfile::Balanced, candidates).unwrap();
    let mut files = files(unsectioned);
    if worker_mode == "fixed-edit" {
        files.insert(DatasetKind::FixedActivities, b"course_plan_code,audience_kind,audience_code,meeting_ordinal,day,period,duration,room_code,teacher_code\nP0,administrative_class,AC1,1,monday,1,1,R1,T1\n".to_vec());
    }
    let command = ImportCommitCommand {
        project_id,
        display_name: "Read model fixture".to_owned(),
        intent: ImportCommitIntent::Create,
        options: CsvImportAuditOptions {
            project_stable_key: "read-model-fixture".to_owned(),
            calendar: CalendarDefinition::weekday_with_break(4, 2).unwrap(),
            exact_subject_choices: 3,
            mode: if unsectioned {
                CsvImportMode::Unsectioned(policy)
            } else {
                CsvImportMode::ExistingSections
            },
        },
    };
    commit_csv_import(
        store,
        &command,
        files
            .iter()
            .map(|(&kind, bytes)| CsvSource::new(kind, bytes)),
    )
    .unwrap();
    let command = StoredProjectSolveCommand {
        project_id,
        expected_revision: 0,
        mode: if unsectioned {
            StoredProjectSolveMode::AutoSectioning(policy)
        } else {
            StoredProjectSolveMode::ExistingSections
        },
    };
    let prepared = prepare_stored_project_solve(store, &command).unwrap();
    let client = SolverClient::new(
        SidecarSpec::new(env!("CARGO_BIN_EXE_application-test-worker")).arg(
            if worker_mode == "fixed-edit" {
                "cancel-after-feasible"
            } else {
                worker_mode
            },
        ),
    );
    let artifact = execute_durable_stored_project_solve(
        prepared,
        &SolveOptions::reproducible(99, Duration::from_secs(10)),
        &client,
        &CancellationToken::new(),
    )
    .unwrap();
    let receipt = save_prepared_solve_artifact(store, &artifact).unwrap();
    (project_id, receipt.run_id)
}

pub fn saved_scenario(store: &mut SqliteStore, unsectioned: bool) -> ScenarioReceipt {
    scenario_with_mode(store, unsectioned, "cancel-after-feasible")
}
pub fn scenario_with_mode(
    store: &mut SqliteStore,
    unsectioned: bool,
    mode: &str,
) -> ScenarioReceipt {
    let (project_id, run_id) = saved_run(store, unsectioned, mode, 1);
    let prepared = prepare_adopt_run(
        store,
        &AdoptRunCommand {
            project_id,
            expected_source_revision: 0,
            run_id: run_id.parse().unwrap(),
            scenario_id: ScenarioId::new_v4(),
            display_name: "采用方案".to_owned(),
        },
    )
    .unwrap();
    commit_prepared_scenario_creation(store, prepared).unwrap()
}
