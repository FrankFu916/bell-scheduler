//! Transport mapping for the shared, revision-checked import use case.

use std::path::{Path, PathBuf};

use class_schedule_application::{
    AutoSectioningError, CompileError, ImportCommandError, ImportCommitCommand, ImportCommitIntent,
    ImportCommitReceipt, commit_prepared_import, load_imported_project,
    prepare_tabular_import_commit,
};
use class_schedule_domain::SchoolProjectId;
use class_schedule_import::{CsvSource, WorkbookProblem, WorkbookProblemCode};
use class_schedule_persistence::{PersistenceError, SqliteStore};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::Manager;

use crate::{
    COMMAND_SCHEMA_VERSION, CommandError, InspectCsvBundleRequest, ResolvedWorkbook, audit_options,
    import_problem_dto, resolve_sources, resolve_workbooks, static_validation_dto,
    validate_request, validate_tabular_size,
};

#[cfg(test)]
#[path = "../../../../crates/application/tests/support/workbook_fixture.rs"]
mod workbook_fixture;

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommitIntentDto {
    Create {},
    Replace {
        #[serde(rename = "expectedRevision")]
        expected_revision: String,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitCsvBundleRequest {
    pub schema_version: u32,
    pub project_id: String,
    pub display_name: String,
    pub intent: CommitIntentDto,
    pub import: InspectCsvBundleRequest,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadImportedProjectRequest {
    pub schema_version: u32,
    pub project_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedProjectReceiptDto {
    pub schema_version: u32,
    pub project_id: String,
    pub revision: String,
    pub display_name: String,
    pub project_stable_key: String,
    pub document_schema_version: u32,
    pub payload_hash: String,
    pub payload_hash_algorithm: &'static str,
    pub sectioning_required: bool,
    pub database_path: String,
}

#[tauri::command]
pub async fn commit_csv_bundle(
    app: tauri::AppHandle,
    request: CommitCsvBundleRequest,
) -> Result<ImportedProjectReceiptDto, CommandError> {
    tauri::async_runtime::spawn_blocking(move || {
        commit_csv_bundle_inner(request, || database_path(&app))
    })
    .await
    .map_err(|_| background_error())?
}

#[tauri::command]
pub async fn load_imported_project_receipt(
    app: tauri::AppHandle,
    request: LoadImportedProjectRequest,
) -> Result<ImportedProjectReceiptDto, CommandError> {
    tauri::async_runtime::spawn_blocking(move || {
        load_imported_project_inner(&request, || database_path(&app))
    })
    .await
    .map_err(|_| background_error())?
}

fn background_error() -> CommandError {
    CommandError::new(
        "DESKTOP_BACKGROUND_TASK_FAILED",
        "本地任务未正常完成；请按项目 ID 重新读取状态后再决定是否提交。",
    )
}

pub(super) fn database_path(app: &tauri::AppHandle) -> Result<PathBuf, CommandError> {
    app.path()
        .app_data_dir()
        .map(|directory| directory.join("projects.sqlite3"))
        .map_err(|_| {
            CommandError::new(
                "DESKTOP_DATABASE_LOCATION_UNAVAILABLE",
                "无法定位本机应用数据目录。",
            )
        })
}

pub(super) fn open_store(path: &Path) -> Result<SqliteStore, CommandError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| {
            CommandError::new(
                "DESKTOP_DATABASE_DIRECTORY_UNAVAILABLE",
                "无法创建本机应用数据目录。",
            )
        })?;
    }
    SqliteStore::open(path).map_err(|error| application_error(error.into()))
}

fn commit_csv_bundle_inner(
    request: CommitCsvBundleRequest,
    resolve_database: impl FnOnce() -> Result<PathBuf, CommandError>,
) -> Result<ImportedProjectReceiptDto, CommandError> {
    validate_schema(request.schema_version)?;
    validate_request(&request.import)?;
    let project_id = parse_project_id(&request.project_id)?;
    let intent = match request.intent {
        CommitIntentDto::Create {} => ImportCommitIntent::Create,
        CommitIntentDto::Replace { expected_revision } => ImportCommitIntent::Replace {
            expected_revision: parse_revision(&expected_revision)?,
        },
    };
    let command = ImportCommitCommand {
        project_id,
        display_name: request.display_name,
        intent,
        options: audit_options(&request.import)?,
    };
    validate_tabular_size(&request.import.datasets, &request.import.workbooks)?;
    let sources = resolve_sources(request.import.datasets)?;
    let workbooks = resolve_workbooks(request.import.workbooks)?;
    let prepared = prepare_tabular_import_commit(
        &command,
        sources
            .iter()
            .map(|(kind, bytes)| CsvSource::new(*kind, bytes)),
        workbooks.iter().map(ResolvedWorkbook::source),
    )
    .map_err(application_error)?;
    // Resolving/creating the database is deliberately after all shared preparation.
    let path = resolve_database()?;
    let mut store = open_store(&path)?;
    let receipt = commit_prepared_import(&mut store, prepared).map_err(application_error)?;
    Ok(receipt_dto(
        receipt,
        command.display_name.trim().to_owned(),
        command.options.project_stable_key.trim().to_owned(),
        &path,
    ))
}

fn load_imported_project_inner(
    request: &LoadImportedProjectRequest,
    resolve_database: impl FnOnce() -> Result<PathBuf, CommandError>,
) -> Result<ImportedProjectReceiptDto, CommandError> {
    validate_schema(request.schema_version)?;
    let project_id = parse_project_id(&request.project_id)?;
    let path = resolve_database()?;
    let store = open_store(&path)?;
    let loaded = load_imported_project(&store, project_id).map_err(application_error)?;
    Ok(receipt_dto(
        loaded.receipt,
        loaded.display_name,
        loaded.document.project_stable_key,
        &path,
    ))
}

pub(super) fn validate_schema(version: u32) -> Result<(), CommandError> {
    if version != COMMAND_SCHEMA_VERSION {
        return Err(CommandError::new(
            "DESKTOP_UNSUPPORTED_COMMAND_SCHEMA",
            "不支持的桌面命令版本。",
        ));
    }
    Ok(())
}

pub(super) fn parse_project_id(value: &str) -> Result<SchoolProjectId, CommandError> {
    value
        .parse()
        .map_err(|_| CommandError::new("DESKTOP_INVALID_PROJECT_ID", "项目 ID 必须是 UUID。"))
}

pub(super) fn parse_revision(value: &str) -> Result<u64, CommandError> {
    let parsed = value.parse::<u64>().ok();
    if !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
        && let Some(revision) = parsed.filter(|revision| i64::try_from(*revision).is_ok())
    {
        return Ok(revision);
    }
    Err(CommandError::new(
        "DESKTOP_INVALID_EXPECTED_REVISION",
        "expected revision 必须是 SQLite 整数范围内的规范十进制字符串。",
    ))
}

fn receipt_dto(
    receipt: ImportCommitReceipt,
    display_name: String,
    project_stable_key: String,
    path: &Path,
) -> ImportedProjectReceiptDto {
    ImportedProjectReceiptDto {
        schema_version: COMMAND_SCHEMA_VERSION,
        project_id: receipt.project_id.to_string(),
        revision: receipt.revision.to_string(),
        display_name,
        project_stable_key,
        document_schema_version: receipt.document_schema_version,
        payload_hash: receipt.payload_hash,
        payload_hash_algorithm: "blake3",
        sectioning_required: receipt.sectioning_required,
        database_path: path.to_string_lossy().into_owned(),
    }
}

pub(super) fn application_error(error: ImportCommandError) -> CommandError {
    let code = error.code();
    let message = application_error_message(&error);
    CommandError::new(code, message).with_details(application_error_details(error))
}

fn application_error_message(error: &ImportCommandError) -> &'static str {
    match error {
        ImportCommandError::Import(_) => {
            "数据表未通过校验。请按下方表名、行号和列名修正后重新审计。"
        }
        ImportCommandError::InvalidField { .. } => "导入设置无效，请检查指出的字段。",
        ImportCommandError::Workbook(_) => {
            "Excel 工作簿未通过校验。请按工作表、行号、列号和问题说明修正后重新审计。"
        }
        ImportCommandError::ImportResourceLimit { .. } => {
            "导入文件总量或 Excel 转换后的数据总量超过 32 MiB，请减少本次导入范围。"
        }
        ImportCommandError::UnsupportedChoicePolicy => {
            "本版导入提交要求每名学生恰好选择 3 个选考学科。"
        }
        ImportCommandError::Compile(error) => compile_error_message(error),
        ImportCommandError::Sectioning(error) => sectioning_error_message(error),
        ImportCommandError::PrecheckRejected { .. } => {
            "所有本次审计候选均存在明确的 Hard 矛盾，无法提交。请查看各候选的问题并修正输入。"
        }
        ImportCommandError::StableKeyMismatch => {
            "替换导入必须沿用已保存项目的稳定键，请重新读取项目后再替换。"
        }
        ImportCommandError::Document(_) | ImportCommandError::DocumentVersionMismatch => {
            "项目文档格式或版本不兼容，无法安全读取或写入。"
        }
        ImportCommandError::UnsupportedMaterialization => {
            "本版不能载入未经独立选择流程确认的自动成班结果。"
        }
        ImportCommandError::Persistence(error) => persistence_error_message(error),
    }
}

fn compile_error_message(error: &CompileError) -> &'static str {
    match error {
        CompileError::UnresolvedSectioning { .. } => {
            "文件中有学生选科，但没有正式教学班及成员关系。若学校尚未成班，请选择 B · 自动成班；若已成班，请补齐教学班和成员表。"
        }
        CompileError::CalendarInvalid { .. } => "校历设置无效。请检查每日课时数和午休位置。",
        CompileError::CalendarReference { .. } => {
            "数据引用了校历中不存在的星期或节次，请检查教师不可用时间及固定活动表。"
        }
        CompileError::NoLegalStart { .. } => {
            "课程的课时组合或固定活动没有合法开始节次。请检查连堂、午休和每日课时设置。"
        }
        CompileError::EmptyAudience { .. } => {
            "有行政班或教学班没有学生，请检查班级表及对应成员关系。"
        }
        CompileError::MissingReference { .. } => {
            "课程编译时无法找到引用的资源，请检查课程、班级、教师和教室的对应关系。"
        }
        CompileError::SectioningModeConflict => {
            "自动成班候选不能与已导入的正式教学班混用，请核对 A/B 输入方式。"
        }
        CompileError::SectioningProjectKeyMismatch => {
            "成班候选所属项目与当前项目不一致，请重新审计当前项目。"
        }
        CompileError::IndexOverflow => "数据规模超过当前编译器支持的索引范围。",
        CompileError::Scheduling(_) => "课程编译后的约束结构无效，请检查课程计划及固定活动设置。",
    }
}

fn sectioning_error_message(error: &AutoSectioningError) -> &'static str {
    match error {
        AutoSectioningError::ExistingSectioning => {
            "B · 自动成班不能同时导入已有教学班或成员关系。请改用 A · 学校已成班，或移除已有教学班及成员表。"
        }
        AutoSectioningError::InvalidSizePolicy { .. } => {
            "班额设置需满足：最小班额 > 0，最小班额 ≤ 目标班额 ≤ 最大班额。"
        }
        AutoSectioningError::InvalidCandidateCount { .. } => "成班候选数量必须在 1 到 16 之间。",
        AutoSectioningError::MissingStudentGrade { .. } => {
            "无法从学生所在行政班确定年级，请检查学生和行政班表。"
        }
        AutoSectioningError::MissingTeachingSectionPlan { .. } => {
            "有选考学科缺少对应年级的教学班课程计划，请补齐 course_plans 表。"
        }
        AutoSectioningError::NoCommonTeacherCandidate { .. } => {
            "有年级学科的课程计划之间没有共同可用教师，请检查教师候选配置。"
        }
        AutoSectioningError::NoCommonRoomCandidate { .. } => {
            "有年级学科的课程计划之间没有共同可用教室，请检查教室候选、容量和设施要求。"
        }
        AutoSectioningError::NoFeasibleSectionCount { .. } => {
            "选科人数、班额上下限和可用教室容量无法组成完整教学班。请核对人数与实际资源后调整成班设置。"
        }
        AutoSectioningError::GenerationFailed { .. } => {
            "本次成班未生成可用候选，请查看成班诊断。这不表示所有成班方案均不可行。"
        }
        AutoSectioningError::CandidateValidationFailed { .. }
        | AutoSectioningError::GeneratedReferenceMissing => {
            "生成的成班候选未通过独立校验，已拒绝使用。请保留错误代码以便排查。"
        }
        AutoSectioningError::CardinalityOverflow => "成班数据规模超过当前支持的整数范围。",
    }
}

fn persistence_error_message(error: &PersistenceError) -> &'static str {
    match error {
        PersistenceError::RevisionConflict { .. } => {
            "项目已被其他操作更新。请重新读取最新 revision，再审计并决定是否替换。"
        }
        PersistenceError::ProjectAlreadyExists { .. } => {
            "此项目 ID 已存在。请读取后明确选择替换，或生成新项目 ID。"
        }
        PersistenceError::ProjectNotFound { .. } => {
            "本机数据库没有此项目 ID，请检查 ID 或先新建项目。"
        }
        _ => {
            "本机数据库操作失败。请检查应用数据目录的可写权限、磁盘空间及数据库状态；若提交回执中断，请先重新读取项目。"
        }
    }
}

fn application_error_details(error: ImportCommandError) -> serde_json::Value {
    match error {
        ImportCommandError::Workbook(failure) => json!({
            "problems": failure.problems().iter().map(workbook_problem_dto).collect::<Vec<_>>(),
        }),
        ImportCommandError::ImportResourceLimit {
            stage,
            maximum_bytes,
        } => json!({
            "stage": stage,
            "maximumBytes": maximum_bytes,
        }),
        ImportCommandError::Import(failure) => json!({
            "problems": failure.problems().iter().map(import_problem_dto).collect::<Vec<_>>(),
        }),
        ImportCommandError::InvalidField { field } => json!({ "field": field }),
        ImportCommandError::PrecheckRejected { reports } => json!({
            "reports": reports.into_iter().map(static_validation_dto).collect::<Vec<_>>(),
        }),
        ImportCommandError::Compile(CompileError::UnresolvedSectioning { choice_count }) => json!({
            "choiceCount": choice_count,
            "requiredDatasets": ["teaching_sections", "section_enrollments"],
        }),
        ImportCommandError::Sectioning(AutoSectioningError::InvalidSizePolicy {
            minimum_size,
            target_size,
            maximum_size,
        }) => {
            json!({"minimumSize": minimum_size, "targetSize": target_size, "maximumSize": maximum_size})
        }
        ImportCommandError::Sectioning(AutoSectioningError::InvalidCandidateCount {
            candidate_count,
        }) => json!({"candidateCount": candidate_count}),
        ImportCommandError::Sectioning(AutoSectioningError::NoFeasibleSectionCount {
            demand,
            ..
        }) => json!({"demand": demand}),
        ImportCommandError::Sectioning(AutoSectioningError::GenerationFailed {
            diagnostics,
            ..
        }) => json!({
            "diagnostics": diagnostics.iter().map(crate::sectioning_diagnostic_dto).collect::<Vec<_>>(),
        }),
        ImportCommandError::Persistence(PersistenceError::RevisionConflict {
            project_id,
            expected_revision,
            actual_revision,
        }) => json!({
            "projectId": project_id,
            "expectedRevision": expected_revision.to_string(),
            "actualRevision": actual_revision.to_string(),
        }),
        ImportCommandError::Persistence(
            PersistenceError::ProjectAlreadyExists { project_id }
            | PersistenceError::ProjectNotFound { project_id },
        ) => json!({ "projectId": project_id }),
        // Error Display can contain SQL, imported cell text or paths. Only allowlisted
        // structured details cross the IPC boundary; the stable code remains intact.
        _ => serde_json::Value::Null,
    }
}

fn workbook_problem_dto(problem: &WorkbookProblem) -> serde_json::Value {
    let message = match problem.code {
        WorkbookProblemCode::WorkbookInvalidArchive => {
            "文件不是可读取的 XLSX 工作簿，请确认文件格式且未损坏。"
        }
        WorkbookProblemCode::WorkbookResourceLimit => {
            "工作簿展开后的大小、工作表或单元格数量超出导入上限。"
        }
        WorkbookProblemCode::WorkbookUnsupportedArchiveEntry => {
            "工作簿包含不支持的压缩条目，请另存为标准 XLSX。"
        }
        WorkbookProblemCode::WorkbookInvalidXml | WorkbookProblemCode::WorkbookInvalidWorksheet => {
            "工作簿内部结构损坏，无法安全读取。"
        }
        WorkbookProblemCode::WorkbookDtdForbidden => "工作簿包含不允许的 DTD 声明。",
        WorkbookProblemCode::WorkbookInvalidSheetMapping => {
            "工作表对应关系重复或引用了不存在的工作表，请重新选择。"
        }
        WorkbookProblemCode::WorkbookUnknownSheet => {
            "该工作表尚未对应到标准数据表，请明确选择数据表类型。"
        }
        WorkbookProblemCode::WorkbookDuplicateDataset => "多个工作表对应同一种数据表，请消除重复。",
        WorkbookProblemCode::WorkbookMergedCells => "请取消合并单元格，并为每一行填写完整数据。",
        WorkbookProblemCode::WorkbookFormulaCell => {
            "不导入公式或公式缓存；请核对结果后将其转换为明确的值。"
        }
        WorkbookProblemCode::WorkbookErrorCell => "单元格含 Excel 错误值，请先修正。",
        WorkbookProblemCode::WorkbookDateCell => {
            "此表不接受日期或时间单元格，请使用标准模板要求的类型。"
        }
        WorkbookProblemCode::WorkbookTextRequired => {
            "编码、名称和列标题必须存为文本，避免丢失前导零。"
        }
        WorkbookProblemCode::WorkbookInvalidNumber => "数值必须是模板允许范围内的非负整数。",
        WorkbookProblemCode::WorkbookMultilineCell => "单元格不能包含换行，请整理为单行内容。",
        WorkbookProblemCode::WorkbookInvalidCellOrder => {
            "工作表含重复或乱序单元格，请重新另存工作簿。"
        }
        WorkbookProblemCode::WorkbookInvalidCellReference => {
            "工作表含无效单元格位置，请重新另存工作簿。"
        }
        WorkbookProblemCode::WorkbookMissingHeader => "第一行缺少列标题，请按标准模板填写。",
    };
    json!({
        "code": problem.code,
        "sheetName": problem.sheet_name,
        "row": problem.row,
        "column": problem.column,
        "message": message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AutoSectioningRequest, InspectionInputMode};

    fn request() -> CommitCsvBundleRequest {
        CommitCsvBundleRequest {
            schema_version: 1,
            project_id: SchoolProjectId::new_v4().to_string(),
            display_name: "本地测试项目".to_owned(),
            intent: CommitIntentDto::Create {},
            import: InspectCsvBundleRequest {
                schema_version: 1,
                project_stable_key: "desktop-persisted-small".to_owned(),
                input_mode: InspectionInputMode::ExistingSections,
                exact_subject_choices: 3,
                periods_per_day: 8,
                break_after_period: 4,
                auto_sectioning: None,
                workbooks: Vec::new(),
                datasets: crate::tests::payloads(&[
                    "students",
                    "administrative_classes",
                    "student_subject_choices",
                    "teachers",
                    "teacher_unavailability",
                    "rooms",
                    "course_plans",
                    "teaching_sections",
                    "section_enrollments",
                    "course_offerings",
                    "fixed_activities",
                ]),
            },
        }
    }

    fn input_b_request() -> CommitCsvBundleRequest {
        let mut command = request();
        command.import.input_mode = InspectionInputMode::AutoSectioning;
        command.import.auto_sectioning = Some(AutoSectioningRequest {
            minimum_size: 10,
            target_size: 12,
            maximum_size: 16,
            seed: 20_260_904,
            candidate_count: 2,
        });
        command.import.datasets.retain(|dataset| {
            !matches!(
                dataset.dataset.as_str(),
                "teaching_sections"
                    | "section_enrollments"
                    | "course_offerings"
                    | "fixed_activities"
            )
        });
        command
    }

    fn audit_and_commit_rejection(command: CommitCsvBundleRequest) -> CommandError {
        let audit = crate::inspect_csv_bundle_inner(command.import.clone())
            .expect_err("real CSV audit must reject this command");
        let commit = commit_csv_bundle_inner(command, || panic!("must not open database"))
            .expect_err("real CSV commit must reject before opening the database");
        assert_eq!(audit.code, commit.code);
        assert_eq!(audit.message, commit.message);
        assert_eq!(audit.details, commit.details);
        commit
    }

    #[test]
    fn legacy_csv_schema_one_defaults_to_no_workbooks() {
        let request: InspectCsvBundleRequest = serde_json::from_value(json!({
            "schemaVersion": 1,
            "projectStableKey": "legacy-csv",
            "inputMode": "existing_sections",
            "exactSubjectChoices": 3,
            "periodsPerDay": 8,
            "breakAfterPeriod": 4,
            "autoSectioning": null,
            "datasets": [],
        }))
        .unwrap();
        assert!(request.workbooks.is_empty());
    }

    #[test]
    fn mixed_csv_and_named_xlsx_sheet_commit_through_shared_application() {
        let mut command = request();
        let teachers_index = command
            .import
            .datasets
            .iter()
            .position(|dataset| dataset.dataset == "teachers")
            .unwrap();
        let teachers = command.import.datasets.remove(teachers_index);
        let workbook = workbook_fixture::from_csv(&[("教师名册", &teachers.bytes)]);
        let names = crate::list_workbook_sheets_inner(&workbook).unwrap();
        assert_eq!(names.schema_version, 1);
        assert_eq!(names.sheet_names, ["教师名册"]);
        command.import.workbooks.push(crate::WorkbookPayload {
            bytes: workbook,
            sheet_mappings: vec![crate::WorkbookSheetMappingDto {
                sheet_name: "教师名册".to_owned(),
                dataset: "teachers".to_owned(),
            }],
        });
        let audit = crate::inspect_csv_bundle_inner(command.import.clone()).unwrap();
        assert_eq!(audit.import_counts.students, 24);
        assert_eq!(audit.candidates[0].problem_counts.activities, 34);
        assert!(audit.candidates[0].static_validation.passed);
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("projects.sqlite3");
        let receipt = commit_csv_bundle_inner(command.clone(), || Ok(path.clone())).unwrap();
        let loaded = load_imported_project(
            &SqliteStore::open(path).unwrap(),
            command.project_id.parse().unwrap(),
        )
        .unwrap();
        assert_eq!(receipt.revision, "0");
        assert_eq!(receipt.payload_hash, loaded.receipt.payload_hash);
        assert!(!loaded.receipt.sectioning_required);
    }

    #[test]
    fn xlsx_input_b_commit_saves_original_choices_without_materialization() {
        let mut command = input_b_request();
        let workbook = workbook_fixture::from_csv(
            &command
                .import
                .datasets
                .iter()
                .map(|dataset| (dataset.dataset.as_str(), dataset.bytes.as_slice()))
                .collect::<Vec<_>>(),
        );
        command.import.datasets.clear();
        command.import.workbooks.push(crate::WorkbookPayload {
            bytes: workbook,
            sheet_mappings: Vec::new(),
        });
        let audit = crate::inspect_csv_bundle_inner(command.import.clone()).unwrap();
        assert_eq!(audit.import_counts.subject_choices, 72);
        assert_eq!(audit.candidates.len(), 2);
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("projects.sqlite3");
        let receipt = commit_csv_bundle_inner(command.clone(), || Ok(path.clone())).unwrap();
        let loaded = load_imported_project(
            &SqliteStore::open(path).unwrap(),
            command.project_id.parse().unwrap(),
        )
        .unwrap();
        assert!(receipt.sectioning_required);
        assert!(loaded.compiled.is_none());
        assert!(loaded.document.generated_sectioning.is_none());
        assert!(loaded.document.import_batch.teaching_sections().is_empty());
        assert_eq!(
            loaded.document.import_batch.student_subject_choices().len(),
            72
        );
    }

    #[test]
    fn invalid_workbook_and_formula_cells_fail_before_database_with_safe_locations() {
        let mut malformed = request();
        malformed.import.workbooks.push(crate::WorkbookPayload {
            bytes: b"CANARY_INVALID_ZIP".to_vec(),
            sheet_mappings: Vec::new(),
        });
        let error = audit_and_commit_rejection(malformed);
        assert_eq!(error.code, "APPLICATION_WORKBOOK_REJECTED");
        assert_eq!(
            error.details["problems"][0]["code"],
            "WORKBOOK_INVALID_ARCHIVE"
        );
        assert!(!serde_json::to_string(&error).unwrap().contains("CANARY"));
        let mut formula = request();
        let workbook = workbook_fixture::from_sheet_xml(&[(
            "teachers",
            concat!(
                "<row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>teacher_code</t></is></c>",
                "<c r=\"B1\" t=\"inlineStr\"><is><t>name</t></is></c></row>",
                "<row r=\"2\"><c r=\"A2\" t=\"inlineStr\"><is><t>T1</t></is></c>",
                "<c r=\"B2\" t=\"str\"><f>CANARY_FORMULA</f><v>CANARY_CACHED_NAME</v></c></row>",
            ),
        )]);
        assert_eq!(
            crate::list_workbook_sheets_inner(&workbook)
                .unwrap()
                .sheet_names,
            ["teachers"]
        );
        formula.import.workbooks.push(crate::WorkbookPayload {
            bytes: workbook,
            sheet_mappings: Vec::new(),
        });
        let error = audit_and_commit_rejection(formula);
        assert_eq!(error.code, "APPLICATION_WORKBOOK_REJECTED");
        assert_eq!(
            error.details["problems"][0]["code"],
            "WORKBOOK_FORMULA_CELL"
        );
        assert_eq!(error.details["problems"][0]["sheetName"], "teachers");
        assert_eq!(error.details["problems"][0]["row"], 2);
        assert_eq!(error.details["problems"][0]["column"], 2);
        assert!(
            error.details["problems"][0]["message"]
                .as_str()
                .unwrap()
                .contains("公式")
        );
        assert!(!serde_json::to_string(&error).unwrap().contains("CANARY"));
    }

    #[test]
    fn input_b_in_existing_sections_mode_explains_required_tables() {
        let mut command = input_b_request();
        command.import.input_mode = InspectionInputMode::ExistingSections;
        command.import.auto_sectioning = None;

        let error = audit_and_commit_rejection(command);

        assert_eq!(error.code, "APPLICATION_SECTIONING_REQUIRED");
        assert!(error.message.contains("B · 自动成班"));
        assert!(error.message.contains("补齐教学班和成员表"));
        assert_eq!(error.details["choiceCount"], 72);
        assert_eq!(
            error.details["requiredDatasets"],
            json!(["teaching_sections", "section_enrollments"])
        );
    }

    #[test]
    fn existing_sections_in_input_b_mode_explain_mode_conflict_without_empty_object() {
        let mut command = request();
        let input_b = input_b_request();
        command.import.input_mode = input_b.import.input_mode;
        command.import.auto_sectioning = input_b.import.auto_sectioning;

        let error = audit_and_commit_rejection(command);

        assert_eq!(error.code, "APPLICATION_SECTIONING_EXISTING_DATA_CONFLICT");
        assert!(error.message.contains("A · 学校已成班"));
        assert!(error.message.contains("已有教学班及成员表"));
        assert!(error.details.is_null());
    }

    #[test]
    fn impossible_size_policy_exposes_only_numeric_demand() {
        let mut command = input_b_request();
        let policy = command.import.auto_sectioning.as_mut().unwrap();
        policy.minimum_size = 25;
        policy.target_size = 25;
        policy.maximum_size = 30;
        for dataset in &mut command.import.datasets {
            let csv = String::from_utf8(dataset.bytes.clone()).unwrap();
            dataset.bytes = csv
                .replace("G12", "CANARY_PRIVATE_GRADE")
                .replace("biology", "CANARY_PRIVATE_SUBJECT")
                .into_bytes();
        }

        let error = audit_and_commit_rejection(command);

        assert_eq!(error.code, "APPLICATION_SECTIONING_SIZE_POLICY_INFEASIBLE");
        assert!(error.message.contains("班额上下限"));
        assert!(error.message.contains("可用教室容量"));
        assert_eq!(error.details, json!({"demand": 12}));
        assert!(!serde_json::to_string(&error).unwrap().contains("CANARY"));
    }

    #[test]
    fn text_bearing_errors_have_safe_messages_even_without_details() {
        let canary = "CANARY_STUDENT /private/path/private.csv SELECT secret";
        let errors = [
            ImportCommandError::Compile(CompileError::CalendarInvalid {
                detail: canary.to_owned(),
            }),
            ImportCommandError::Compile(CompileError::CalendarReference {
                detail: canary.to_owned(),
            }),
            ImportCommandError::Compile(CompileError::MissingReference {
                detail: canary.to_owned(),
            }),
            ImportCommandError::Compile(CompileError::EmptyAudience {
                audience: canary.to_owned(),
            }),
            ImportCommandError::Compile(CompileError::NoLegalStart {
                detail: canary.to_owned(),
            }),
            ImportCommandError::Sectioning(AutoSectioningError::MissingStudentGrade {
                student_code: canary.to_owned(),
            }),
            ImportCommandError::Sectioning(AutoSectioningError::MissingTeachingSectionPlan {
                grade_code: canary.to_owned(),
                subject_code: canary.to_owned(),
            }),
            ImportCommandError::Sectioning(AutoSectioningError::NoCommonTeacherCandidate {
                grade_code: canary.to_owned(),
                subject_code: canary.to_owned(),
            }),
            ImportCommandError::Sectioning(AutoSectioningError::NoCommonRoomCandidate {
                grade_code: canary.to_owned(),
                subject_code: canary.to_owned(),
            }),
            ImportCommandError::Persistence(PersistenceError::InvalidDocument {
                code: "PERSISTENCE_INVALID_JSON_PAYLOAD",
                detail: canary.to_owned(),
            }),
        ];
        for source in errors {
            let expected_code = source.code();
            let error = application_error(source);
            assert_eq!(error.code, expected_code);
            assert!(error.details.is_null());
            assert!(error.message.contains('请'));
            let json = serde_json::to_string(&error).unwrap();
            for private_text in ["CANARY", "/private/path", "SELECT secret"] {
                assert!(!json.contains(private_text));
            }
        }
    }

    #[test]
    fn create_replace_reopen_and_stale_revision_use_real_sqlite() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("projects.sqlite3");
        let mut command = request();
        let created = commit_csv_bundle_inner(command.clone(), || Ok(path.clone())).unwrap();
        let json = serde_json::to_value(&created).unwrap();
        assert_eq!(json["revision"], "0");
        assert_eq!(json["payloadHashAlgorithm"], "blake3");
        assert_eq!(json["sectioningRequired"], false);
        assert_eq!(json["payloadHash"].as_str().unwrap().len(), 64);
        assert_eq!(json["databasePath"], path.to_string_lossy().as_ref());
        let duplicate = commit_csv_bundle_inner(command.clone(), || Ok(path.clone())).unwrap_err();
        assert_eq!(duplicate.code, "PERSISTENCE_PROJECT_ALREADY_EXISTS");
        command.intent = CommitIntentDto::Replace {
            expected_revision: "0".to_owned(),
        };
        let updated = commit_csv_bundle_inner(command.clone(), || Ok(path.clone())).unwrap();
        assert_eq!(updated.revision, "1");
        let conflict = commit_csv_bundle_inner(command.clone(), || Ok(path.clone())).unwrap_err();
        assert_eq!(conflict.code, "PERSISTENCE_REVISION_CONFLICT");
        assert_eq!(conflict.details["expectedRevision"], "0");
        assert_eq!(conflict.details["actualRevision"], "1");
        let loaded = load_imported_project_inner(
            &LoadImportedProjectRequest {
                schema_version: 1,
                project_id: command.project_id,
            },
            || Ok(path),
        )
        .unwrap();
        assert_eq!(loaded.revision, "1");
        assert_eq!(loaded.payload_hash, updated.payload_hash);
    }

    #[test]
    fn input_b_receipt_is_unsectioned_after_reopening() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("projects.sqlite3");
        let command = input_b_request();
        let receipt = commit_csv_bundle_inner(command.clone(), || Ok(path.clone())).unwrap();
        assert!(receipt.sectioning_required);
        let loaded = load_imported_project(
            &SqliteStore::open(path).unwrap(),
            command.project_id.parse().unwrap(),
        )
        .unwrap();
        assert!(loaded.compiled.is_none());
        assert!(loaded.document.generated_sectioning.is_none());
        assert!(loaded.document.import_batch.teaching_sections().is_empty());
        assert_eq!(
            loaded.document.import_batch.student_subject_choices().len(),
            72
        );
    }

    #[test]
    fn malformed_and_static_hard_failures_do_not_even_open_database() {
        let mut malformed = request();
        malformed.import.datasets[0].bytes = b"private_name\nCANARY_STUDENT\n".to_vec();
        let failure =
            commit_csv_bundle_inner(malformed, || panic!("must not open database")).unwrap_err();
        assert_eq!(failure.code, "APPLICATION_IMPORT_REJECTED");
        assert!(
            !serde_json::to_string(&failure)
                .unwrap()
                .contains("CANARY_STUDENT")
        );
        let mut hard = request();
        hard.import.periods_per_day = 2;
        hard.import.break_after_period = 1;
        assert!(commit_csv_bundle_inner(hard, || panic!("must not open database")).is_err());
    }

    #[test]
    fn insufficient_rooms_have_readable_static_problems_and_cannot_commit() {
        let mut command = request();
        let rooms = command
            .import
            .datasets
            .iter_mut()
            .find(|dataset| dataset.dataset == "rooms")
            .unwrap();
        rooms.bytes = String::from_utf8(rooms.bytes.clone())
            .unwrap()
            .replace(",36,", ",1,")
            .replace(",18,", ",1,")
            .replace(",30,", ",1,")
            .into_bytes();
        let audit = crate::inspect_csv_bundle_inner(command.import.clone()).unwrap();
        let report = &audit.candidates[0].static_validation;
        assert!(!report.passed);
        let room_problem = report
            .hard_problems
            .iter()
            .find(|problem| problem.code == "PRECHECK_ACTIVITY_NO_ELIGIBLE_ROOM")
            .unwrap();
        assert!(room_problem.message.contains("容量"));
        assert!(!room_problem.activity_indices.is_empty());
        let failure =
            commit_csv_bundle_inner(command, || panic!("must not open database")).unwrap_err();
        assert_eq!(failure.code, "APPLICATION_IMPORT_PRECHECK_REJECTED");
        assert!(
            failure.details["reports"][0]["hardProblems"]
                .as_array()
                .unwrap()
                .iter()
                .all(|problem| !problem["message"].as_str().unwrap().is_empty())
        );
    }

    #[test]
    fn revision_and_intent_dto_fail_closed() {
        for value in [
            "",
            "00",
            "01",
            "+1",
            "-1",
            " 1",
            "1 ",
            "1.0",
            "1e1",
            "9223372036854775808",
            "18446744073709551616",
        ] {
            assert_eq!(
                parse_revision(value).unwrap_err().code,
                "DESKTOP_INVALID_EXPECTED_REVISION"
            );
        }
        assert_eq!(
            parse_revision("9007199254740993").unwrap(),
            9_007_199_254_740_993
        );
        assert!(serde_json::from_value::<CommitIntentDto>(json!({"mode":"replace"})).is_err());
        assert!(
            serde_json::from_value::<CommitIntentDto>(json!({
                "mode":"replace", "expectedRevision":0,
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<CommitIntentDto>(json!({
                "mode":"create", "expectedRevision":"0",
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<LoadImportedProjectRequest>(json!({
                "schemaVersion": 1,
                "projectId": SchoolProjectId::new_v4().to_string(),
                "databasePath": "/tmp/untrusted-database.sqlite3",
            }))
            .is_err()
        );
        let error = application_error(
            PersistenceError::InvalidDocument {
                code: "PERSISTENCE_INVALID_JSON_PAYLOAD",
                detail: "SQL secret CANARY_STUDENT".to_owned(),
            }
            .into(),
        );
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("CANARY_STUDENT")
        );
    }

    #[test]
    fn invalid_command_metadata_fails_before_database_resolution() {
        let mut invalid_id = request();
        invalid_id.project_id = "CANARY_STUDENT".to_owned();
        let error =
            commit_csv_bundle_inner(invalid_id, || panic!("must not open database")).unwrap_err();
        assert_eq!(error.code, "DESKTOP_INVALID_PROJECT_ID");
        let mut unknown_schema = request();
        unknown_schema.schema_version = 2;
        let error = commit_csv_bundle_inner(unknown_schema, || panic!("must not open database"))
            .unwrap_err();
        assert_eq!(error.code, "DESKTOP_UNSUPPORTED_COMMAND_SCHEMA");
        let mut choices = request();
        choices.import.exact_subject_choices = 2;
        let error =
            commit_csv_bundle_inner(choices, || panic!("must not open database")).unwrap_err();
        assert_eq!(error.code, "APPLICATION_IMPORT_CHOICE_POLICY_UNSUPPORTED");
    }
}
