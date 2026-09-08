#![forbid(unsafe_code)]

use std::env;
use std::error::Error;
use std::io::{self, Write};
use std::process;
use std::thread;
use std::time::Duration;

use solver_contract::framing::{DEFAULT_MAX_FRAME_LEN, read_frame, write_frame};
use solver_contract::{
    MeetingAssignment, SolveResponse, SolverEnvelope, SolverStatistics, SolverStatus,
    solver_envelope,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mode = env::args().nth(1).ok_or("missing fake worker mode")?;
    let request: SolverEnvelope = read_frame(&mut io::stdin().lock(), DEFAULT_MAX_FRAME_LEN)?;

    match mode.as_str() {
        "timeout" | "cancel" => {
            thread::sleep(Duration::from_secs(10));
            Ok(())
        }
        "malformed" => {
            io::stdout().lock().write_all(&[0, 0, 0, 5, b'x'])?;
            Ok(())
        }
        "nonzero" => process::exit(23),
        "success"
        | "environment-cleared"
        | "trailing"
        | "internal"
        | "stderr"
        | "wrong-request-id"
        | "wrong-snapshot-hash"
        | "wrong-engine-name"
        | "wrong-engine-version"
        | "wrong-adapter-version"
        | "different-build-revision" => {
            if mode == "environment-cleared" {
                if env::vars_os().any(|(key, _)| key != "WORKER_EXPLICIT_TEST_VALUE") {
                    return Err("inherited environment reached the managed worker".into());
                }
                if env::var("WORKER_EXPLICIT_TEST_VALUE")?.as_str() != "explicit" {
                    return Err("explicit worker environment was lost".into());
                }
            }
            let response = response_for(&request, mode.as_str())?;
            let mut stdout = io::stdout().lock();
            write_frame(&mut stdout, &response, DEFAULT_MAX_FRAME_LEN)?;
            if mode == "trailing" {
                stdout.write_all(b"stdout-must-be-protocol-only")?;
            }
            stdout.flush()?;
            if mode == "stderr" {
                io::stderr().lock().write_all(&vec![b'd'; 8 * 1024])?;
            }
            Ok(())
        }
        _ => Err(format!("unknown fake worker mode: {mode}").into()),
    }
}

fn response_for(request: &SolverEnvelope, mode: &str) -> Result<SolverEnvelope, Box<dyn Error>> {
    let Some(solver_envelope::Payload::SolveRequest(solve_request)) = request.payload.as_ref()
    else {
        return Err("expected solve request".into());
    };
    let parameters = solve_request.parameters.ok_or("missing parameters")?;
    let input_snapshot_hash = solve_request
        .problem
        .as_ref()
        .ok_or("missing problem")?
        .snapshot_hash
        .clone();
    let status = if mode == "internal" {
        SolverStatus::InternalError
    } else {
        SolverStatus::Feasible
    };
    let mut response = SolveResponse {
        engine_version: solve_request.required_engine_version.clone(),
        status: status as i32,
        status_detail_code: if mode == "internal" {
            "SOLVER.INTERNAL.TEST_FAILURE".into()
        } else {
            "SOLVER.FEASIBLE".into()
        },
        objective: None,
        statistics: Some(SolverStatistics {
            wall_time_millis: 1,
            deterministic_time: 0.5,
            conflicts: 0,
            branches: 1,
            propagations: 2,
            peak_memory_bytes: 1,
            worker_count: parameters.worker_count,
            seed: parameters.seed,
        }),
        assignments: if status == SolverStatus::Feasible {
            vec![MeetingAssignment {
                activity_id: 1,
                start_timeslot_id: 1,
                room_id: 1,
                teacher_id: 1,
                duration_periods: 1,
            }]
        } else {
            vec![]
        },
        section_room_assignments: vec![],
        diagnostic_groups: vec![],
        output_hash: vec![],
        effective_parameters: Some(parameters),
        input_snapshot_hash,
    };
    if mode == "wrong-snapshot-hash" {
        response.input_snapshot_hash[0] ^= 1;
    }
    let engine = response.engine_version.as_mut().ok_or("missing engine")?;
    match mode {
        "wrong-engine-name" => engine.engine_name = "different-engine".into(),
        "wrong-engine-version" => engine.engine_version = "99.0.0".into(),
        "wrong-adapter-version" => engine.adapter_version = "99.0.0".into(),
        "different-build-revision" => engine.build_revision = "another-build".into(),
        _ => {}
    }
    Ok(SolverEnvelope {
        protocol_version: request.protocol_version,
        request_id: if mode == "wrong-request-id" {
            "different-request".into()
        } else {
            request.request_id.clone()
        },
        payload: Some(solver_envelope::Payload::SolveResponse(response)),
    })
}
