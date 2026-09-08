#![forbid(unsafe_code)]

use std::error::Error;
use std::io;

use solver_contract::framing::{DEFAULT_MAX_FRAME_LEN, read_frame, write_frame};
use solver_contract::{
    MeetingAssignment, SolveResponse, SolverEnvelope, SolverStatistics, SolverStatus,
    solver_envelope,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mode = std::env::args().nth(1).ok_or("missing mode")?;
    let request: SolverEnvelope = read_frame(&mut io::stdin().lock(), DEFAULT_MAX_FRAME_LEN)?;
    let Some(solver_envelope::Payload::SolveRequest(solve_request)) = request.payload.as_ref()
    else {
        return Err("expected solve request".into());
    };
    let problem = solve_request.problem.as_ref().ok_or("missing problem")?;
    let parameters = solve_request.parameters.ok_or("missing parameters")?;
    if mode == "worker-crash"
        || (mode == "feasible-then-crash" && request.request_id.ends_with(":sectioning:2"))
    {
        return Err("intentional protocol fixture process failure".into());
    }
    let (status, status_detail_code, assignments) = if mode == "unknown" || mode == "timeout" {
        (
            if mode == "unknown" {
                SolverStatus::Unknown
            } else {
                SolverStatus::Timeout
            },
            "SOLVER_NO_INCUMBENT",
            Vec::new(),
        )
    } else if mode == "proven-infeasible" {
        (
            SolverStatus::ProvenInfeasible,
            "SOLVER.PROVEN_INFEASIBLE",
            Vec::new(),
        )
    } else if mode == "cancel-after-feasible" && request.request_id.ends_with(":sectioning:2") {
        // Deliver cancellation through the real framed protocol after the first candidate's
        // validated feasible result, without a timer racing process startup in the test.
        (SolverStatus::Cancelled, "SOLVER_CANCELLED", Vec::new())
    } else if mode == "cancel-after-feasible" || mode == "feasible-then-crash" {
        // This fixture has only single-period sections, one teacher, and one room.
        // Sequential starts provide a valid first candidate before cancellation of the next.
        let assignments = sequential_assignments(problem)?;
        (SolverStatus::Feasible, "SOLVER_FEASIBLE", assignments)
    } else {
        let second_start = if mode == "invalid-conflict" { 1 } else { 2 };
        (
            SolverStatus::Feasible,
            "SOLVER_FEASIBLE",
            vec![
                MeetingAssignment {
                    activity_id: 1,
                    start_timeslot_id: 1,
                    room_id: 1,
                    teacher_id: 1,
                    duration_periods: 1,
                },
                MeetingAssignment {
                    activity_id: 2,
                    start_timeslot_id: second_start,
                    room_id: 2,
                    teacher_id: 2,
                    duration_periods: 1,
                },
            ],
        )
    };
    let response = SolverEnvelope {
        protocol_version: request.protocol_version,
        request_id: if mode == "protocol-wrong-request" {
            "wrong-request".to_owned()
        } else {
            request.request_id
        },
        payload: Some(solver_envelope::Payload::SolveResponse(SolveResponse {
            engine_version: solve_request.required_engine_version.clone(),
            status: status as i32,
            status_detail_code: status_detail_code.to_owned(),
            objective: None,
            statistics: Some(SolverStatistics {
                wall_time_millis: 1,
                deterministic_time: 0.0,
                conflicts: 0,
                branches: 1,
                propagations: 1,
                peak_memory_bytes: 0,
                worker_count: parameters.worker_count,
                seed: parameters.seed,
            }),
            assignments,
            section_room_assignments: Vec::new(),
            diagnostic_groups: Vec::new(),
            output_hash: Vec::new(),
            effective_parameters: Some(parameters),
            input_snapshot_hash: problem.snapshot_hash.clone(),
        })),
    };
    write_frame(&mut io::stdout().lock(), &response, DEFAULT_MAX_FRAME_LEN)?;
    Ok(())
}

fn sequential_assignments(
    problem: &solver_contract::SchedulingProblemSnapshot,
) -> Result<Vec<MeetingAssignment>, Box<dyn Error>> {
    problem
        .activities
        .iter()
        .enumerate()
        .map(|(index, activity)| {
            Ok(MeetingAssignment {
                activity_id: activity.activity_id,
                start_timeslot_id: *activity
                    .allowed_start_timeslot_ids
                    .get(index)
                    .ok_or("fixture has insufficient starts")?,
                room_id: 1,
                teacher_id: 1,
                duration_periods: activity.duration_periods,
            })
        })
        .collect()
}
