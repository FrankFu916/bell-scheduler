#![forbid(unsafe_code)]

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut arguments = env::args_os();
    let _program = arguments.next();
    let Some(output) = arguments.next() else {
        eprintln!("usage: class-schedule-fixture-generator <new-or-empty-output-directory>");
        return ExitCode::from(2);
    };
    if arguments.next().is_some() {
        eprintln!("expected exactly one output directory");
        return ExitCode::from(2);
    }
    match class_schedule_fixture_generator::write_medium_fixture(PathBuf::from(output)) {
        Ok(stats) => {
            println!(
                "generated medium fixture: students={}, teachers={}, rooms={}, sections={}",
                stats.students, stats.teachers, stats.rooms, stats.sections
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("fixture generation failed: {error}");
            ExitCode::from(1)
        }
    }
}
