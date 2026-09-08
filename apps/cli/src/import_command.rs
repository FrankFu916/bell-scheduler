use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use class_schedule_application::{
    AutoSectioningPolicy, CalendarDefinition, CsvImportAuditOptions, CsvImportMode,
    ImportCommandError, ImportCommitCommand, ImportCommitIntent, MAXIMUM_TABULAR_IMPORT_BYTES,
    SectioningProfile, WorkbookImportSource, commit_prepared_import, prepare_tabular_import_commit,
};
use class_schedule_domain::SchoolProjectId;
use class_schedule_import::{CsvSource, DatasetKind, WorkbookProblemCode};
use class_schedule_persistence::{PersistenceError, SqliteStore};
use serde_json::json;

use crate::{CliFailure, DEFAULT_MAXIMUM_INPUT_BYTES, InputMode, RunOutcome, SUCCESS_EXIT_CODE};

#[cfg(test)]
#[path = "../../../crates/application/tests/support/workbook_fixture.rs"]
mod workbook_fixture;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Operation {
    Create,
    Replace,
}

#[derive(Clone, Debug, Args)]
pub(super) struct ImportArgs {
    /// Directory containing canonical `<dataset>.csv` files; may be combined with workbooks.
    #[arg(long, required_unless_present = "workbooks")]
    input_dir: Option<PathBuf>,
    /// XLSX file with canonical worksheet names. Repeat for multiple files; custom/Chinese sheet
    /// name mappings are not supported by this CLI command.
    #[arg(
        long = "workbook",
        value_name = "PATH",
        required_unless_present = "input_dir"
    )]
    workbooks: Vec<PathBuf>,
    /// Local `SQLite` file. Its parent directory must already exist.
    #[arg(long)]
    database: PathBuf,
    #[arg(long, value_enum)]
    operation: Operation,
    /// Internal project UUID; create never overwrites an existing UUID.
    #[arg(long)]
    project_id: SchoolProjectId,
    #[arg(long)]
    project_key: String,
    #[arg(long)]
    display_name: String,
    /// Required only for replace; no automatic conflict retry.
    #[arg(long)]
    expected_revision: Option<u64>,
    #[arg(long, value_enum)]
    input_mode: InputMode,
    #[arg(long, default_value_t = 8)]
    periods_per_day: u16,
    #[arg(long, default_value_t = 4)]
    break_after_period: u16,
    #[arg(long)]
    section_min_size: Option<u16>,
    #[arg(long)]
    section_target_size: Option<u16>,
    #[arg(long)]
    section_max_size: Option<u16>,
    #[arg(long)]
    sectioning_candidates: Option<u8>,
    #[arg(long)]
    seed: Option<u64>,
    /// Aggregate source-byte budget. The shared import also caps source and normalized data at
    /// 32 MiB; this option can impose a smaller limit.
    #[arg(long, default_value_t = DEFAULT_MAXIMUM_INPUT_BYTES)]
    maximum_input_bytes: u64,
}

pub(super) fn run(args: &ImportArgs) -> Result<RunOutcome, CliFailure> {
    let intent = match (args.operation, args.expected_revision) {
        (Operation::Create, None) => ImportCommitIntent::Create,
        (Operation::Replace, Some(expected_revision)) => {
            ImportCommitIntent::Replace { expected_revision }
        }
        _ => {
            return Err(CliFailure::new(
                "CLI_IMPORT_REVISION_REQUIRED_FOR_REPLACE_ONLY",
                "expected-revision is required for replace and forbidden for create",
            ));
        }
    };
    let mode = input_mode(args)?;
    let calendar =
        CalendarDefinition::weekday_with_break(args.periods_per_day, args.break_after_period)
            .map_err(|error| CliFailure::new(error.code(), "calendar definition is invalid"))?;
    let sources = read_import_inputs(args)?;
    let command = ImportCommitCommand {
        project_id: args.project_id,
        display_name: args.display_name.clone(),
        intent,
        options: CsvImportAuditOptions {
            project_stable_key: args.project_key.clone(),
            calendar,
            exact_subject_choices: 3,
            mode,
        },
    };
    let prepared = prepare_tabular_import_commit(
        &command,
        sources
            .csv
            .iter()
            .map(|(kind, bytes)| CsvSource::new(*kind, bytes)),
        sources.workbooks.iter().map(|bytes| WorkbookImportSource {
            bytes,
            mappings: &[],
        }),
    )
    .map_err(command_failure)?;
    // Even database creation/migrations happen only after every CSV/XLSX input is fully read,
    // normalized, validated and serialized into the immutable prepared command.
    let mut store =
        SqliteStore::open(&args.database).map_err(|error| command_failure(error.into()))?;
    let receipt = commit_prepared_import(&mut store, prepared).map_err(command_failure)?;
    let rendered_summary = serde_json::to_string_pretty(&json!({
        "schemaVersion": 1,
        "projectId": receipt.project_id.to_string(),
        "revision": receipt.revision.to_string(),
        "documentSchemaVersion": receipt.document_schema_version,
        "payloadHash": receipt.payload_hash,
        "payloadHashAlgorithm": "blake3",
        "sectioningRequired": receipt.sectioning_required,
    }))
    .map_err(|_| {
        CliFailure::new(
            "CLI_SUMMARY_SERIALIZATION_FAILED",
            "receipt serialization failed",
        )
    })?;
    Ok(RunOutcome {
        rendered_summary,
        exit_code: SUCCESS_EXIT_CODE,
    })
}

#[derive(Debug)]
struct ImportFiles {
    csv: Vec<(DatasetKind, Vec<u8>)>,
    workbooks: Vec<Vec<u8>>,
}

fn read_import_inputs(args: &ImportArgs) -> Result<ImportFiles, CliFailure> {
    if args.input_dir.is_none() && args.workbooks.is_empty() {
        return Err(CliFailure::new(
            "CLI_IMPORT_SOURCE_REQUIRED",
            "input-dir or workbook is required",
        ));
    }
    let maximum = args
        .maximum_input_bytes
        .min(MAXIMUM_TABULAR_IMPORT_BYTES as u64);
    let mut remaining = maximum;
    let mut csv = Vec::new();
    if let Some(directory) = &args.input_dir {
        for (kind, path) in csv_paths(directory)? {
            let bytes = read_bounded_file(&path, remaining, maximum)?;
            remaining -= bytes.len() as u64;
            csv.push((kind, bytes));
        }
    }
    let mut workbooks = Vec::with_capacity(args.workbooks.len());
    for path in &args.workbooks {
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("xlsx"))
        {
            return Err(CliFailure::new(
                "CLI_UNSUPPORTED_WORKBOOK_FORMAT",
                "workbook inputs must be .xlsx files",
            ));
        }
        let bytes = read_bounded_file(path, remaining, maximum)?;
        remaining -= bytes.len() as u64;
        workbooks.push(bytes);
    }
    Ok(ImportFiles { csv, workbooks })
}

fn csv_paths(directory: &Path) -> Result<BTreeMap<DatasetKind, PathBuf>, CliFailure> {
    let metadata = fs::metadata(directory).map_err(|_| {
        CliFailure::new(
            "CLI_INPUT_DIRECTORY_NOT_ACCESSIBLE",
            "cannot access input directory",
        )
    })?;
    if !metadata.is_dir() {
        return Err(CliFailure::new(
            "CLI_INPUT_NOT_DIRECTORY",
            "input path is not a directory",
        ));
    }
    let mut paths = BTreeMap::new();
    let entries = fs::read_dir(directory).map_err(|_| {
        CliFailure::new(
            "CLI_INPUT_DIRECTORY_READ_FAILED",
            "cannot list input directory",
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|_| {
            CliFailure::new(
                "CLI_INPUT_DIRECTORY_READ_FAILED",
                "cannot inspect directory entry",
            )
        })?;
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
        {
            continue;
        }
        let name = entry.file_name();
        let kind = super::DATASETS
            .into_iter()
            .find(|kind| name == std::ffi::OsStr::new(&format!("{}.csv", kind.as_str())))
            .ok_or_else(|| {
                CliFailure::new(
                    "CLI_UNKNOWN_DATASET_FILE",
                    "unrecognized CSV dataset filename",
                )
            })?;
        paths.insert(kind, path);
    }
    Ok(paths)
}

fn read_bounded_file(path: &Path, remaining: u64, maximum: u64) -> Result<Vec<u8>, CliFailure> {
    // Match the existing CLI dataset policy: symbolic links and non-regular files are rejected.
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        CliFailure::new("CLI_INPUT_FILE_NOT_ACCESSIBLE", "cannot inspect input file")
    })?;
    if !metadata.is_file() {
        return Err(CliFailure::new(
            "CLI_INPUT_DATASET_NOT_FILE",
            "input is not a regular file",
        ));
    }
    if metadata.len() > remaining {
        return Err(input_too_large(maximum));
    }
    let file = fs::File::open(path)
        .map_err(|_| CliFailure::new("CLI_INPUT_FILE_READ_FAILED", "cannot open input file"))?;
    if !file
        .metadata()
        .map_err(|_| {
            CliFailure::new(
                "CLI_INPUT_FILE_NOT_ACCESSIBLE",
                "cannot inspect opened input file",
            )
        })?
        .is_file()
    {
        return Err(CliFailure::new(
            "CLI_INPUT_DATASET_NOT_FILE",
            "input is not a regular file",
        ));
    }
    read_limited(file, remaining, maximum)
}

fn read_limited(reader: impl Read, remaining: u64, maximum: u64) -> Result<Vec<u8>, CliFailure> {
    let sentinel_limit = remaining
        .checked_add(1)
        .ok_or_else(|| CliFailure::new("CLI_INPUT_SIZE_OVERFLOW", "input size limit overflowed"))?;
    let mut bytes = Vec::new();
    reader
        .take(sentinel_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            CliFailure::new(
                "CLI_INPUT_FILE_READ_FAILED",
                "cannot completely read input file",
            )
        })?;
    if bytes.len() as u64 > remaining {
        return Err(input_too_large(maximum));
    }
    Ok(bytes)
}

fn input_too_large(maximum: u64) -> CliFailure {
    CliFailure::new(
        "CLI_INPUT_TOO_LARGE",
        "aggregate CSV/XLSX input exceeds the byte limit",
    )
    .with_details(json!({ "maximum_bytes": maximum }))
}

fn input_mode(args: &ImportArgs) -> Result<CsvImportMode, CliFailure> {
    match args.input_mode {
        InputMode::ExistingSections => {
            if args.section_min_size.is_some()
                || args.section_target_size.is_some()
                || args.section_max_size.is_some()
                || args.sectioning_candidates.is_some()
                || args.seed.is_some()
            {
                return Err(CliFailure::new(
                    "CLI_IMPORT_SECTIONING_POLICY_NOT_ALLOWED",
                    "Input A does not accept sectioning parameters",
                ));
            }
            Ok(CsvImportMode::ExistingSections)
        }
        InputMode::AutoSectioning => {
            let (Some(minimum), Some(target), Some(maximum), Some(candidates), Some(seed)) = (
                args.section_min_size,
                args.section_target_size,
                args.section_max_size,
                args.sectioning_candidates,
                args.seed,
            ) else {
                return Err(CliFailure::new(
                    "CLI_IMPORT_SECTIONING_POLICY_REQUIRED",
                    "Input B requires explicit minimum/target/maximum size, candidate count and seed",
                ));
            };
            let policy = AutoSectioningPolicy::new(
                minimum,
                target,
                maximum,
                seed,
                SectioningProfile::Balanced,
                candidates,
            )
            .map_err(|error| command_failure(error.into()))?;
            Ok(CsvImportMode::Unsectioned(policy))
        }
    }
}

pub(super) fn command_failure(error: ImportCommandError) -> CliFailure {
    let code = error.code();
    let message = error.to_string();
    let details = match error {
        ImportCommandError::Import(failure) => return super::import_failure_to_cli(&failure),
        ImportCommandError::Workbook(failure) => {
            let message = if failure
                .problems()
                .iter()
                .any(|problem| problem.code == WorkbookProblemCode::WorkbookUnknownSheet)
            {
                "CLI workbooks require canonical worksheet names; custom/Chinese sheet mappings are not supported".to_owned()
            } else {
                message
            };
            return CliFailure::new(code, message)
                .with_details(json!({ "problems": failure.problems() }));
        }
        ImportCommandError::ImportResourceLimit {
            stage,
            maximum_bytes,
        } => {
            json!({ "stage": stage, "maximumBytes": maximum_bytes })
        }
        ImportCommandError::PrecheckRejected { reports } => json!({ "reports": reports }),
        ImportCommandError::Persistence(PersistenceError::RevisionConflict {
            project_id,
            expected_revision,
            actual_revision,
        }) => {
            json!({ "projectId": project_id, "expectedRevision": expected_revision.to_string(), "actualRevision": actual_revision.to_string() })
        }
        _ => json!({}),
    };
    CliFailure::new(code, message).with_details(details)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::Cli;

    fn fixture_sources() -> Vec<(DatasetKind, Vec<u8>)> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/small");
        crate::DATASETS
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    fs::read(root.join(format!("{}.csv", kind.as_str()))).unwrap(),
                )
            })
            .collect()
    }

    fn write_workbook(path: &Path, sources: &[(DatasetKind, Vec<u8>)]) {
        let sheets = sources
            .iter()
            .map(|(kind, bytes)| (kind.as_str(), bytes.as_slice()))
            .collect::<Vec<_>>();
        fs::write(path, workbook_fixture::from_csv(&sheets)).unwrap();
    }

    fn arguments(
        database: &Path,
        sources: &[&str],
        operation: &str,
        expected: Option<u64>,
    ) -> Vec<String> {
        let mut arguments = [
            "class-schedule",
            "import",
            "--database",
            database.to_str().unwrap(),
            "--operation",
            operation,
            "--project-id",
            "11111111-1111-4111-8111-111111111111",
            "--project-key",
            "cli-workbook",
            "--display-name",
            "测试高中",
            "--input-mode",
            "existing-sections",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        arguments.extend(sources.iter().map(|value| (*value).to_owned()));
        if let Some(expected) = expected {
            arguments.extend(["--expected-revision".to_owned(), expected.to_string()]);
        }
        arguments
    }

    fn invoke(
        database: &Path,
        sources: &[&str],
        operation: &str,
        expected: Option<u64>,
    ) -> Result<RunOutcome, CliFailure> {
        crate::run(Cli::try_parse_from(arguments(database, sources, operation, expected)).unwrap())
    }

    #[test]
    fn import_requires_csv_directory_or_workbook() {
        let error =
            Cli::try_parse_from(arguments(Path::new("unused.sqlite3"), &[], "create", None))
                .unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn canonical_workbook_commits_and_reloads_the_real_small_fixture() {
        let directory = tempfile::tempdir().unwrap();
        let workbook = directory.path().join("高中 课程.xlsx");
        let database = directory.path().join("school.sqlite3");
        write_workbook(&workbook, &fixture_sources());
        let outcome = invoke(
            &database,
            &["--workbook", workbook.to_str().unwrap()],
            "create",
            None,
        )
        .unwrap();
        let receipt: serde_json::Value = serde_json::from_str(&outcome.rendered_summary).unwrap();
        assert_eq!(receipt["revision"], "0");
        let store = SqliteStore::open(&database).unwrap();
        let loaded = class_schedule_application::load_imported_project(
            &store,
            "11111111-1111-4111-8111-111111111111".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(loaded.document.import_batch.students().len(), 24);
        assert_eq!(loaded.compiled.unwrap().problem.activities().len(), 34);
    }

    #[test]
    fn csv_directory_and_repeated_workbooks_form_one_complete_atomic_import() {
        let directory = tempfile::tempdir().unwrap();
        let csv_directory = directory.path().join("csv");
        fs::create_dir(&csv_directory).unwrap();
        let sources = fixture_sources();
        for (kind, bytes) in &sources[..3] {
            fs::write(csv_directory.join(format!("{}.csv", kind.as_str())), bytes).unwrap();
        }
        let first = directory.path().join("资源.xlsx");
        let second = directory.path().join("课程.xlsx");
        write_workbook(&first, &sources[3..7]);
        write_workbook(&second, &sources[7..]);
        let database = directory.path().join("school.sqlite3");
        let outcome = invoke(
            &database,
            &[
                "--input-dir",
                csv_directory.to_str().unwrap(),
                "--workbook",
                first.to_str().unwrap(),
                "--workbook",
                second.to_str().unwrap(),
            ],
            "create",
            None,
        )
        .unwrap();
        let receipt: serde_json::Value = serde_json::from_str(&outcome.rendered_summary).unwrap();
        assert_eq!(receipt["revision"], "0");
        assert_eq!(receipt["sectioningRequired"], false);
        assert_eq!(
            SqliteStore::open(&database)
                .unwrap()
                .load_project("11111111-1111-4111-8111-111111111111")
                .unwrap()
                .revision,
            0
        );
    }

    #[test]
    fn workbook_errors_keep_coordinates_and_never_create_a_database_or_revision() {
        let directory = tempfile::tempdir().unwrap();
        let valid = directory.path().join("valid.xlsx");
        let invalid = directory.path().join("formula.xlsx");
        let unknown = directory.path().join("unknown.xlsx");
        write_workbook(&valid, &fixture_sources());
        let xml = concat!(
            "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>",
            "<row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>student_code</t></is></c>",
            "<c r=\"B1\" t=\"inlineStr\"><is><t>name</t></is></c>",
            "<c r=\"C1\" t=\"inlineStr\"><is><t>administrative_class_code</t></is></c></row>",
            "<row r=\"2\"><c r=\"A2\" t=\"inlineStr\"><is><t>S1</t></is></c>",
            "<c r=\"B2\"><f>1+1</f><v>2</v></c></row></sheetData></worksheet>",
        );
        fs::write(
            &invalid,
            workbook_fixture::from_sheet_xml(&[("students", xml)]),
        )
        .unwrap();
        fs::write(
            &unknown,
            workbook_fixture::from_sheet_xml(&[("学生名单", xml)]),
        )
        .unwrap();
        let never_created = directory.path().join("never-created.sqlite3");
        let error = invoke(
            &never_created,
            &[
                "--workbook",
                valid.to_str().unwrap(),
                "--workbook",
                invalid.to_str().unwrap(),
            ],
            "create",
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "APPLICATION_WORKBOOK_REJECTED");
        assert_eq!(
            error.details["problems"][0]["code"],
            "WORKBOOK_FORMULA_CELL"
        );
        assert_eq!(error.details["problems"][0]["sheetName"], "students");
        assert_eq!(error.details["problems"][0]["row"], 2);
        assert_eq!(error.details["problems"][0]["column"], 2);
        assert!(!never_created.exists());
        let error = invoke(
            &never_created,
            &["--workbook", unknown.to_str().unwrap()],
            "create",
            None,
        )
        .unwrap_err();
        assert_eq!(
            error.details["problems"][0]["code"],
            "WORKBOOK_UNKNOWN_SHEET"
        );
        assert_eq!(error.details["problems"][0]["sheetName"], "学生名单");
        assert!(error.message.contains("mappings are not supported"));
        assert!(!never_created.exists());

        let database = directory.path().join("existing.sqlite3");
        invoke(
            &database,
            &["--workbook", valid.to_str().unwrap()],
            "create",
            None,
        )
        .unwrap();
        let store = SqliteStore::open(&database).unwrap();
        let original = store
            .load_project("11111111-1111-4111-8111-111111111111")
            .unwrap();
        invoke(
            &database,
            &["--workbook", invalid.to_str().unwrap()],
            "replace",
            Some(0),
        )
        .unwrap_err();
        assert_eq!(store.load_project(&original.project_id).unwrap(), original);
    }

    #[test]
    fn duplicate_datasets_across_formats_are_rejected_before_database_open() {
        let directory = tempfile::tempdir().unwrap();
        let csv_directory = directory.path().join("csv");
        fs::create_dir(&csv_directory).unwrap();
        let sources = fixture_sources();
        fs::write(csv_directory.join("students.csv"), &sources[0].1).unwrap();
        let workbook = directory.path().join("complete.xlsx");
        write_workbook(&workbook, &sources);
        let database = directory.path().join("never-created.sqlite3");
        let error = invoke(
            &database,
            &[
                "--input-dir",
                csv_directory.to_str().unwrap(),
                "--workbook",
                workbook.to_str().unwrap(),
            ],
            "create",
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "CLI_IMPORT_REJECTED");
        assert!(
            error.details["problems"]
                .as_array()
                .unwrap()
                .iter()
                .any(|problem| problem["code"] == "IMPORT_DUPLICATE_DATASET")
        );
        assert!(!database.exists());
    }

    #[test]
    fn combined_input_budget_and_bounded_read_sentinel_reject_excess_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.xlsx");
        let second = directory.path().join("second.xlsx");
        let sources = fixture_sources();
        write_workbook(&first, &sources[..5]);
        write_workbook(&second, &sources[5..]);
        let limit = fs::metadata(&first).unwrap().len() + fs::metadata(&second).unwrap().len() - 1;
        let database = directory.path().join("never-created.sqlite3");
        let error = invoke(
            &database,
            &[
                "--workbook",
                first.to_str().unwrap(),
                "--workbook",
                second.to_str().unwrap(),
                "--maximum-input-bytes",
                &limit.to_string(),
            ],
            "create",
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "CLI_INPUT_TOO_LARGE");
        assert!(!database.exists());
        assert_eq!(
            read_limited(&b"ab"[..], 1, 1).unwrap_err().code,
            "CLI_INPUT_TOO_LARGE"
        );
        assert_eq!(read_limited(&b"a"[..], 1, 1).unwrap(), b"a");
    }

    #[cfg(unix)]
    #[test]
    fn workbook_symlinks_are_rejected_like_csv_dataset_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let workbook = directory.path().join("real.xlsx");
        let symlink = directory.path().join("linked.xlsx");
        write_workbook(&workbook, &fixture_sources());
        std::os::unix::fs::symlink(&workbook, &symlink).unwrap();
        let database = directory.path().join("never-created.sqlite3");
        let error = invoke(
            &database,
            &["--workbook", symlink.to_str().unwrap()],
            "create",
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "CLI_INPUT_DATASET_NOT_FILE");
        assert!(!database.exists());
    }

    #[test]
    fn cli_import_uses_atomic_application_commit_and_structured_revision_conflicts() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("school.sqlite3");
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/small");
        let invoke = |operation: &str, expected: Option<&str>| {
            let mut arguments = vec![
                "class-schedule",
                "import",
                "--input-dir",
                fixture.to_str().unwrap(),
                "--database",
                database.to_str().unwrap(),
                "--operation",
                operation,
                "--project-id",
                "11111111-1111-4111-8111-111111111111",
                "--project-key",
                "cli-import",
                "--display-name",
                "测试高中",
                "--input-mode",
                "existing-sections",
            ];
            if let Some(expected) = expected {
                arguments.extend(["--expected-revision", expected]);
            }
            crate::run(Cli::try_parse_from(arguments).unwrap())
        };
        let created = invoke("create", None).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&created.rendered_summary).unwrap()["revision"],
            "0"
        );
        assert_eq!(
            invoke("create", None).unwrap_err().code,
            "PERSISTENCE_PROJECT_ALREADY_EXISTS"
        );
        invoke("replace", Some("0")).unwrap();
        let error = invoke("replace", Some("0")).unwrap_err();
        assert_eq!(error.code, "PERSISTENCE_REVISION_CONFLICT");
        assert_eq!(error.details["actualRevision"], "1");
    }
}
