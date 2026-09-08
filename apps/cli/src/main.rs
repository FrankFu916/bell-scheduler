#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let cli = class_schedule_cli::Cli::parse();
    match class_schedule_cli::run(cli) {
        Ok(outcome) => {
            println!("{}", outcome.rendered_summary);
            ExitCode::from(outcome.exit_code)
        }
        Err(error) => {
            let rendered = serde_json::to_string_pretty(&error.as_report())
                .unwrap_or_else(|_| format!(r#"{{"code":"{}"}}"#, error.code()));
            eprintln!("{rendered}");
            ExitCode::from(2)
        }
    }
}
