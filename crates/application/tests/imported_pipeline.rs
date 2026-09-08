use std::collections::BTreeMap;
use std::time::Duration;

use class_schedule_application::{
    AutoSectioningPolicy, AutoSectioningSolveStatus, CalendarDefinition, ImportedProjectSolve,
    SectioningProfile, SolveContext, SolveExecution, SolveOptions, solve_imported_project,
};
use class_schedule_import::{CsvImporter, CsvSource, DatasetKind, ImportBatch, ImportConfig};
use solver_client::{CancellationToken, SidecarSpec, SolverClient, SolverRunStatus};

fn input_b_batch() -> ImportBatch {
    let mut files = BTreeMap::<DatasetKind, Vec<u8>>::new();
    files.insert(
        DatasetKind::Students,
        concat!(
            "student_code,name,administrative_class_code\n",
            "S1,Student 1,AC1\n",
            "S2,Student 2,AC1\n",
        )
        .as_bytes()
        .to_vec(),
    );
    files.insert(
        DatasetKind::AdministrativeClasses,
        concat!(
            "administrative_class_code,name,grade_code,home_room_code\n",
            "AC1,Class 1,G12,R1\n",
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
        b"teacher_code,name\nT1,Teacher 1\n".to_vec(),
    );
    files.insert(
        DatasetKind::Rooms,
        b"room_code,name,building_code,capacity,features\nR1,Room 1,B1,2,lab\n".to_vec(),
    );
    files.insert(
        DatasetKind::CoursePlans,
        concat!(
            "course_plan_code,name,grade_code,subject_code,audience_kind,weekly_periods,meeting_pattern,min_days_between,max_periods_per_day,may_cross_breaks,required_room_features,teacher_assignment,teacher_codes,room_policy,room_candidates,preferred_rooms,fallback_rooms\n",
            "P1,Physics,G12,physics,teaching_section,1,1,0,1,false,lab,fixed,T1,section_fixed,R1,,\n",
        )
        .as_bytes()
        .to_vec(),
    );
    CsvImporter::new(ImportConfig::default().with_exact_subject_choices(Some(1)))
        .import(
            files
                .iter()
                .map(|(&kind, bytes)| CsvSource::new(kind, bytes)),
        )
        .unwrap_or_else(|failure| panic!("import failed: {:?}", failure.problems()))
}

fn context() -> SolveContext {
    SolveContext {
        project_id: "pipeline-project".to_owned(),
        project_revision: 1,
        scenario_id: "baseline".to_owned(),
        scenario_revision: 1,
        request_id: "pipeline-request".to_owned(),
    }
}

#[test]
fn finite_infeasible_candidate_results_are_not_promoted_to_global_infeasibility() {
    let batch = input_b_batch();
    let policy = AutoSectioningPolicy::new(1, 1, 2, 99, SectioningProfile::Balanced, 2)
        .expect("valid policy");
    let client = SolverClient::new(
        SidecarSpec::new(env!("CARGO_BIN_EXE_application-test-worker")).arg("proven-infeasible"),
    );

    let result = solve_imported_project(
        &batch,
        &CalendarDefinition::weekday_with_break(4, 2).expect("calendar"),
        "pipeline-project",
        Some(policy),
        &context(),
        &SolveOptions::reproducible(99, Duration::from_secs(1)),
        &client,
        &CancellationToken::new(),
    )
    .expect("pipeline completes");
    let ImportedProjectSolve::AutoSectioned(result) = result else {
        panic!("expected auto sectioning");
    };

    assert_eq!(
        result.status,
        AutoSectioningSolveStatus::CandidateBudgetExhausted
    );
    assert_eq!(result.attempts.len(), 2);
    assert!(result.selected_attempt_index.is_none());
    assert!(
        result
            .attempts
            .iter()
            .all(|attempt| { attempt.solver_status() == Some(SolverRunStatus::ProvenInfeasible) })
    );
}

#[test]
fn cancellation_after_a_feasible_candidate_clears_selection_and_preserves_attempts() {
    let client = SolverClient::new(
        SidecarSpec::new(env!("CARGO_BIN_EXE_application-test-worker"))
            .arg("cancel-after-feasible"),
    );
    let cancellation = CancellationToken::new();
    // The second real one-shot worker returns a framed Cancelled response. Pipeline request
    // sequencing provides the synchronization, independent of compilation/startup wall time.
    let result = solve_imported_project(
        &input_b_batch(),
        &CalendarDefinition::weekday_with_break(4, 2).unwrap(),
        "pipeline-project",
        Some(AutoSectioningPolicy::new(1, 1, 2, 99, SectioningProfile::Balanced, 2).unwrap()),
        &context(),
        &SolveOptions::reproducible(99, Duration::from_secs(1)),
        &client,
        &cancellation,
    );
    let ImportedProjectSolve::AutoSectioned(result) = result.unwrap() else {
        panic!("expected Input B attempts");
    };
    assert!(!cancellation.is_cancelled());
    assert_eq!(result.status, AutoSectioningSolveStatus::Cancelled);
    assert!(result.selected_attempt_index.is_none());
    assert!(result.selected_attempt().is_none());
    assert_eq!(result.attempts.len(), 2);
    assert_eq!(
        result.attempts[0].solver_status(),
        Some(SolverRunStatus::Feasible)
    );
    assert!(result.attempts[0].quality.is_some());
    assert_eq!(
        result.attempts[1].solver_status(),
        Some(SolverRunStatus::Cancelled)
    );
    assert!(result.attempts[1].quality.is_none());
    let SolveExecution::Completed(cancelled) = &result.attempts[1].execution else {
        panic!("second worker must execute");
    };
    assert!(cancelled.process.exit_success);
    assert_eq!(
        cancelled.response.as_ref().unwrap().status_detail_code,
        "SOLVER_CANCELLED"
    );
}
