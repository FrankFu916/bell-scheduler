#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Tauri transport for the local scheduling workbench.
//!
//! Command DTOs remain separate from the Rust domain. The command implemented here performs a
//! real import, semantic compilation, optional student sectioning, and independent static Hard
//! validation through the same crates used by the CLI.

use class_schedule_application::{
    AutoSectioningPolicy, CalendarDefinition, CsvImportAuditOptions, CsvImportMode,
    DiagnosticSeverity, ImportCandidateAudit, ImportCommandError, SectioningDiagnostic,
    SectioningObjective, SectioningProfile, WorkbookImportSource, audit_tabular_import,
};
use class_schedule_import::{
    CsvSource, DatasetKind, ImportBatch, ImportProblem, WorkbookSheetMapping, XlsxWorkbook,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::Manager;

mod import_catalog;
mod import_commit;
pub mod managed_worker;
mod problem_messages;
mod project_queries;
mod scenario_commands;
mod solve_commands;
mod timetable_queries;

const COMMAND_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_IPC_IMPORT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionInputMode {
    ExistingSections,
    AutoSectioning,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvDatasetPayload {
    pub dataset: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkbookSheetMappingDto {
    pub sheet_name: String,
    pub dataset: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkbookPayload {
    pub bytes: Vec<u8>,
    pub sheet_mappings: Vec<WorkbookSheetMappingDto>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookSheetsResponse {
    pub schema_version: u32,
    pub sheet_names: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSectioningRequest {
    pub minimum_size: u16,
    pub target_size: u16,
    pub maximum_size: u16,
    pub seed: u64,
    pub candidate_count: u8,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectCsvBundleRequest {
    pub schema_version: u32,
    pub project_stable_key: String,
    pub input_mode: InspectionInputMode,
    pub exact_subject_choices: usize,
    pub periods_per_day: u16,
    pub break_after_period: u16,
    pub auto_sectioning: Option<AutoSectioningRequest>,
    pub datasets: Vec<CsvDatasetPayload>,
    #[serde(default)]
    pub workbooks: Vec<WorkbookPayload>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCounts {
    pub students: usize,
    pub administrative_classes: usize,
    pub subject_choices: usize,
    pub teachers: usize,
    pub rooms: usize,
    pub course_plans: usize,
    pub teaching_sections: usize,
    pub section_enrollments: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticProblemCounts {
    pub students: usize,
    pub teachers: usize,
    pub rooms: usize,
    pub timeslots: usize,
    pub activities: usize,
    pub student_conflict_edges: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateInspection {
    pub candidate_index: usize,
    pub candidate_hash: Option<String>,
    pub sectioning_objective: Option<SectioningObjectiveDto>,
    pub generated_sections: usize,
    pub generated_enrollments: usize,
    pub problem_counts: SemanticProblemCounts,
    pub static_validation: StaticValidationDto,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SectioningObjectiveDto {
    pub target_size_deviation: String,
    pub size_imbalance: String,
    pub timetable_feasibility: TimetableFeasibilityDto,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimetableFeasibilityDto {
    pub concentrated_audience_overlap: String,
    pub resource_availability_penalty: String,
    pub room_capacity_tightness: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HardProblemDto {
    pub code: String,
    pub message: String,
    pub activity_indices: Vec<u32>,
    pub entity_indices: std::collections::BTreeMap<String, u32>,
    pub parameters: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticValidationDto {
    pub passed: bool,
    pub hard_problems: Vec<HardProblemDto>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SectioningDiagnosticDto {
    pub code: String,
    pub severity: String,
    pub student_id: Option<String>,
    pub grade_id: Option<String>,
    pub subject_id: Option<String>,
    pub section_id: Option<String>,
    pub expected_min: Option<String>,
    pub expected_max: Option<String>,
    pub actual: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SectioningInspection {
    pub algorithm_version: String,
    pub input_hash: String,
    pub seed: String,
    pub profile: String,
    pub requested_candidates: u8,
    pub generated_candidates: u8,
    pub diagnostics: Vec<SectioningDiagnosticDto>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectCsvBundleResponse {
    pub schema_version: u32,
    pub input_mode: InspectionInputMode,
    pub import_counts: ImportCounts,
    pub sectioning: Option<SectioningInspection>,
    pub candidates: Vec<CandidateInspection>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub schema_version: u32,
    pub code: String,
    pub message: String,
    pub details: Value,
}

impl CommandError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            schema_version: COMMAND_SCHEMA_VERSION,
            code: code.into(),
            message: message.into(),
            details: Value::Null,
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

/// Parses and audits a CSV import using shared production logic without persisting or solving it.
///
/// The frontend only supplies bytes and displays the returned DTO. It does not evaluate conflicts,
/// section memberships, resource policies, or Hard validity.
#[tauri::command]
async fn inspect_csv_bundle(
    request: InspectCsvBundleRequest,
) -> Result<InspectCsvBundleResponse, CommandError> {
    tauri::async_runtime::spawn_blocking(move || inspect_csv_bundle_inner(request))
        .await
        .map_err(|_| {
            CommandError::new(
                "DESKTOP_BACKGROUND_TASK_FAILED",
                "the import inspection task did not complete normally",
            )
        })?
}

fn inspect_csv_bundle_inner(
    request: InspectCsvBundleRequest,
) -> Result<InspectCsvBundleResponse, CommandError> {
    validate_request(&request)?;
    let options = audit_options(&request)?;
    validate_tabular_size(&request.datasets, &request.workbooks)?;
    let sources = resolve_sources(request.datasets)?;
    let workbooks = resolve_workbooks(request.workbooks)?;
    let audit = audit_tabular_import(
        sources
            .iter()
            .map(|(kind, bytes)| CsvSource::new(*kind, bytes)),
        workbooks.iter().map(ResolvedWorkbook::source),
        &options,
    )
    .map_err(|error| {
        let mut mapped = import_commit::application_error(error);
        // Preserve the established read-only audit v1 error identity.
        if mapped.code == "APPLICATION_IMPORT_REJECTED" {
            "DESKTOP_IMPORT_REJECTED".clone_into(&mut mapped.code);
        }
        mapped
    })?;
    let candidates = audit
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let generated = audit
                .sectioning
                .as_ref()
                .map(|value| &value.candidates[index]);
            candidate_inspection(
                index + 1,
                generated.map(|value| value.candidate().provenance.candidate_hash.to_hex()),
                generated.map(|value| value.candidate().objective),
                generated.map_or(0, |value| value.generated_sections().len()),
                generated.map_or(0, |value| value.generated_enrollments().len()),
                candidate,
            )
        })
        .collect();
    Ok(InspectCsvBundleResponse {
        schema_version: COMMAND_SCHEMA_VERSION,
        input_mode: request.input_mode,
        import_counts: import_counts(&audit.batch),
        sectioning: audit.sectioning.map(|prepared| SectioningInspection {
            algorithm_version: prepared.provenance.algorithm_version,
            input_hash: prepared.provenance.input_hash.to_hex(),
            seed: prepared.provenance.seed.to_string(),
            profile: sectioning_profile_name(prepared.provenance.profile).to_owned(),
            requested_candidates: prepared.provenance.requested_candidates,
            generated_candidates: prepared.provenance.generated_candidates,
            diagnostics: prepared
                .diagnostics
                .iter()
                .map(sectioning_diagnostic_dto)
                .collect(),
        }),
        candidates,
    })
}

fn audit_options(request: &InspectCsvBundleRequest) -> Result<CsvImportAuditOptions, CommandError> {
    let calendar =
        CalendarDefinition::weekday_with_break(request.periods_per_day, request.break_after_period)
            .map_err(|error| {
                import_commit::application_error(ImportCommandError::Compile(error))
            })?;
    let mode = match request.input_mode {
        InspectionInputMode::ExistingSections => {
            if request.auto_sectioning.is_some() {
                return Err(CommandError::new(
                    "DESKTOP_SECTIONING_POLICY_NOT_ALLOWED",
                    "学校已成班模式不能同时提供自动成班策略。",
                ));
            }
            CsvImportMode::ExistingSections
        }
        InspectionInputMode::AutoSectioning => {
            let parameters = request.auto_sectioning.ok_or_else(|| {
                CommandError::new(
                    "DESKTOP_SECTIONING_POLICY_REQUIRED",
                    "请明确提供用于审计的班额与候选策略。",
                )
            })?;
            let policy = AutoSectioningPolicy::new(
                parameters.minimum_size,
                parameters.target_size,
                parameters.maximum_size,
                parameters.seed,
                SectioningProfile::Balanced,
                parameters.candidate_count,
            )
            .map_err(|error| {
                import_commit::application_error(ImportCommandError::Sectioning(error))
            })?;
            CsvImportMode::Unsectioned(policy)
        }
    };
    Ok(CsvImportAuditOptions {
        project_stable_key: request.project_stable_key.clone(),
        calendar,
        exact_subject_choices: request.exact_subject_choices,
        mode,
    })
}

fn validate_request(request: &InspectCsvBundleRequest) -> Result<(), CommandError> {
    if request.schema_version != COMMAND_SCHEMA_VERSION {
        return Err(CommandError::new(
            "DESKTOP_UNSUPPORTED_COMMAND_SCHEMA",
            format!(
                "expected command schema version {COMMAND_SCHEMA_VERSION}, got {}",
                request.schema_version
            ),
        ));
    }
    if request.project_stable_key.trim().is_empty() {
        return Err(CommandError::new(
            "DESKTOP_BLANK_PROJECT_STABLE_KEY",
            "project stable key must not be blank",
        ));
    }
    if request.exact_subject_choices == 0 {
        return Err(CommandError::new(
            "DESKTOP_INVALID_SUBJECT_CHOICE_COUNT",
            "exact subject choice count must be positive",
        ));
    }
    Ok(())
}

#[tauri::command]
async fn list_workbook_sheets(bytes: Vec<u8>) -> Result<WorkbookSheetsResponse, CommandError> {
    tauri::async_runtime::spawn_blocking(move || list_workbook_sheets_inner(&bytes))
        .await
        .map_err(|_| {
            CommandError::new(
                "DESKTOP_BACKGROUND_TASK_FAILED",
                "本地工作簿读取任务未正常完成，请重新选择文件。",
            )
        })?
}

fn list_workbook_sheets_inner(bytes: &[u8]) -> Result<WorkbookSheetsResponse, CommandError> {
    validate_payload_lengths(std::iter::once(bytes.len()))?;
    let sheet_names = XlsxWorkbook::sheet_names(bytes)
        .map_err(|error| import_commit::application_error(error.into()))?;
    Ok(WorkbookSheetsResponse {
        schema_version: COMMAND_SCHEMA_VERSION,
        sheet_names,
    })
}

fn validate_tabular_size(
    datasets: &[CsvDatasetPayload],
    workbooks: &[WorkbookPayload],
) -> Result<(), CommandError> {
    validate_payload_lengths(
        datasets
            .iter()
            .map(|dataset| dataset.bytes.len())
            .chain(workbooks.iter().map(|workbook| workbook.bytes.len())),
    )
}

fn validate_payload_lengths(lengths: impl IntoIterator<Item = usize>) -> Result<(), CommandError> {
    let mut total = 0_u64;
    for length in lengths {
        let length = u64::try_from(length).map_err(|_| {
            CommandError::new(
                "DESKTOP_IMPORT_SIZE_OVERFLOW",
                "导入文件大小超出本机支持的范围。",
            )
        })?;
        total = total.checked_add(length).ok_or_else(|| {
            CommandError::new(
                "DESKTOP_IMPORT_SIZE_OVERFLOW",
                "导入文件总量超出本机支持的范围。",
            )
        })?;
        if total > MAXIMUM_IPC_IMPORT_BYTES {
            return Err(CommandError::new(
                "DESKTOP_IMPORT_TOO_LARGE",
                "CSV 与 Excel 文件合计大小不能超过 32 MiB。",
            )
            .with_details(
                json!({"maximumBytes": MAXIMUM_IPC_IMPORT_BYTES, "actualBytes": total}),
            ));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ResolvedWorkbook {
    bytes: Vec<u8>,
    mappings: Vec<WorkbookSheetMapping>,
}

impl ResolvedWorkbook {
    fn source(&self) -> WorkbookImportSource<'_> {
        WorkbookImportSource {
            bytes: &self.bytes,
            mappings: &self.mappings,
        }
    }
}

fn resolve_workbooks(
    workbooks: Vec<WorkbookPayload>,
) -> Result<Vec<ResolvedWorkbook>, CommandError> {
    workbooks
        .into_iter()
        .map(|workbook| {
            let mappings = workbook
                .sheet_mappings
                .into_iter()
                .map(|mapping| {
                    Ok(WorkbookSheetMapping {
                        sheet_name: mapping.sheet_name,
                        dataset: parse_dataset_kind(&mapping.dataset)?,
                    })
                })
                .collect::<Result<_, CommandError>>()?;
            Ok(ResolvedWorkbook {
                bytes: workbook.bytes,
                mappings,
            })
        })
        .collect()
}

fn resolve_sources(
    datasets: Vec<CsvDatasetPayload>,
) -> Result<Vec<(DatasetKind, Vec<u8>)>, CommandError> {
    let mut total = 0_u64;
    let mut sources = Vec::with_capacity(datasets.len());
    for dataset in datasets {
        let length = u64::try_from(dataset.bytes.len()).map_err(|_| {
            CommandError::new(
                "DESKTOP_IMPORT_SIZE_OVERFLOW",
                "dataset byte length exceeded u64",
            )
        })?;
        total = total.checked_add(length).ok_or_else(|| {
            CommandError::new(
                "DESKTOP_IMPORT_SIZE_OVERFLOW",
                "aggregate dataset byte length overflowed",
            )
        })?;
        if total > MAXIMUM_IPC_IMPORT_BYTES {
            return Err(CommandError::new(
                "DESKTOP_IMPORT_TOO_LARGE",
                "aggregate CSV payload exceeds the desktop command limit",
            )
            .with_details(json!({
                "maximumBytes": MAXIMUM_IPC_IMPORT_BYTES,
                "actualBytes": total,
            })));
        }
        sources.push((parse_dataset_kind(&dataset.dataset)?, dataset.bytes));
    }
    Ok(sources)
}

fn parse_dataset_kind(value: &str) -> Result<DatasetKind, CommandError> {
    match value {
        "students" => Ok(DatasetKind::Students),
        "administrative_classes" => Ok(DatasetKind::AdministrativeClasses),
        "student_subject_choices" => Ok(DatasetKind::StudentSubjectChoices),
        "teachers" => Ok(DatasetKind::Teachers),
        "teacher_unavailability" => Ok(DatasetKind::TeacherUnavailability),
        "rooms" => Ok(DatasetKind::Rooms),
        "course_plans" => Ok(DatasetKind::CoursePlans),
        "teaching_sections" => Ok(DatasetKind::TeachingSections),
        "section_enrollments" => Ok(DatasetKind::SectionEnrollments),
        "course_offerings" => Ok(DatasetKind::CourseOfferings),
        "fixed_activities" => Ok(DatasetKind::FixedActivities),
        _ => Err(CommandError::new(
            "DESKTOP_UNKNOWN_DATASET",
            "无法确定文件对应的数据表。请选择数据表类型，或使用标准模板中的 CSV 文件名。",
        )),
    }
}

fn import_counts(batch: &ImportBatch) -> ImportCounts {
    ImportCounts {
        students: batch.students().len(),
        administrative_classes: batch.administrative_classes().len(),
        subject_choices: batch.student_subject_choices().len(),
        teachers: batch.teachers().len(),
        rooms: batch.rooms().len(),
        course_plans: batch.course_plans().len(),
        teaching_sections: batch.teaching_sections().len(),
        section_enrollments: batch.section_enrollments().len(),
    }
}

fn candidate_inspection(
    candidate_index: usize,
    candidate_hash: Option<String>,
    sectioning_objective: Option<SectioningObjective>,
    generated_sections: usize,
    generated_enrollments: usize,
    candidate: &ImportCandidateAudit,
) -> CandidateInspection {
    let problem = &candidate.compiled.problem;
    CandidateInspection {
        candidate_index,
        candidate_hash,
        sectioning_objective: sectioning_objective.as_ref().map(sectioning_objective_dto),
        generated_sections,
        generated_enrollments,
        problem_counts: SemanticProblemCounts {
            students: problem.students().len(),
            teachers: problem.teachers().len(),
            rooms: problem.rooms().len(),
            timeslots: problem.timeslots().len(),
            activities: problem.activities().len(),
            student_conflict_edges: problem.student_conflict_edges().len(),
        },
        static_validation: static_validation_dto(candidate.static_validation.clone()),
    }
}

fn sectioning_profile_name(profile: SectioningProfile) -> &'static str {
    match profile {
        SectioningProfile::Fast => "fast",
        SectioningProfile::Balanced => "balanced",
        SectioningProfile::BestQuality => "best_quality",
    }
}

fn sectioning_objective_dto(objective: &SectioningObjective) -> SectioningObjectiveDto {
    SectioningObjectiveDto {
        target_size_deviation: objective.target_size_deviation.to_string(),
        size_imbalance: objective.size_imbalance.to_string(),
        timetable_feasibility: TimetableFeasibilityDto {
            concentrated_audience_overlap: objective
                .timetable_feasibility
                .concentrated_audience_overlap
                .to_string(),
            resource_availability_penalty: objective
                .timetable_feasibility
                .resource_availability_penalty
                .to_string(),
            room_capacity_tightness: objective
                .timetable_feasibility
                .room_capacity_tightness
                .to_string(),
        },
    }
}

fn static_validation_dto(
    report: class_schedule_validation::ValidationReport,
) -> StaticValidationDto {
    StaticValidationDto {
        passed: report.is_valid(),
        hard_problems: report
            .hard_problems
            .into_iter()
            .map(|problem| HardProblemDto {
                code: problem.code.as_str().to_owned(),
                message: problem_messages::hard_problem_message(problem.code).to_owned(),
                activity_indices: problem
                    .activities
                    .into_iter()
                    .map(|activity| activity.0)
                    .collect(),
                entity_indices: problem.entity_indices,
                parameters: problem.parameters,
            })
            .collect(),
    }
}

fn sectioning_diagnostic_dto(diagnostic: &SectioningDiagnostic) -> SectioningDiagnosticDto {
    SectioningDiagnosticDto {
        code: diagnostic.code.as_str().to_owned(),
        severity: match diagnostic.severity {
            DiagnosticSeverity::Error => "error",
            DiagnosticSeverity::Warning => "warning",
        }
        .to_owned(),
        student_id: diagnostic.student_id.map(|id| id.to_string()),
        grade_id: diagnostic.grade_id.map(|id| id.to_string()),
        subject_id: diagnostic.subject_id.map(|id| id.to_string()),
        section_id: diagnostic.section_id.map(|id| id.to_string()),
        expected_min: diagnostic.expected_min.map(|value| value.to_string()),
        expected_max: diagnostic.expected_max.map(|value| value.to_string()),
        actual: diagnostic.actual.map(|value| value.to_string()),
    }
}

fn import_problem_dto(problem: &ImportProblem) -> Value {
    json!({
        "code": problem.code().as_str(),
        "message": import_problem_message(problem.code()),
        "dataset": problem.location().dataset_kind().as_str(),
        "row": problem.location().row_number().map(|value| value.to_string()),
        "column": problem.location().column(),
        "relatedRow": problem.related_row().map(|value| value.to_string()),
        "expected": problem.expected().map(|value| value.to_string()),
        "actual": problem.actual().map(|value| value.to_string()),
    })
}

fn import_problem_message(code: class_schedule_import::ImportProblemCode) -> &'static str {
    use class_schedule_import::ImportProblemCode as Code;
    match code {
        Code::ImportMissingDataset => "缺少核心数据表，请添加对应文件。",
        Code::ImportDuplicateDataset => "同一数据表被提供多次，请检查文件对应关系。",
        Code::ImportInvalidUtf8 => "文件不是有效 UTF-8 编码，请从表格软件另存为 CSV UTF-8。",
        Code::ImportCsvSyntax => "CSV 行的列数或格式不正确，请检查分隔符及引号。",
        Code::ImportMissingHeader => "文件缺少首行列名。",
        Code::ImportEmptyHeader => "首行含空列名，请删除多余空列或填写标准列名。",
        Code::ImportDuplicateHeader => "列名重复，请为每列使用唯一列名。",
        Code::ImportUnknownHeader => "存在不支持的列名，请对照模板修正，或检查文件对应的数据表。",
        Code::ImportMissingRequiredHeader => "缺少此必需列，请按模板补齐。",
        Code::ImportInvalidColumnMapping | Code::ImportDuplicateMappedField => {
            "列对应关系无效或重复，请检查列名映射。"
        }
        Code::ImportEmptyRequiredValue => "必填单元格为空，请补齐数据。",
        Code::ImportInvalidUnsignedInteger => "需要范围内的非负整数，不能填写小数、负数或文字。",
        Code::ImportInvalidBoolean => "布尔字段需填写 true 或 false。",
        Code::ImportInvalidEnumValue => "字段值不在支持的选项内，请对照模板说明。",
        Code::ImportInvalidList => "列表格式不正确，请使用模板规定的分隔符并避免空项。",
        Code::ImportDuplicateExternalCode => "标识代码重复，请检查本表唯一代码。",
        Code::ImportDuplicateRelation => "同一关系被重复填写，请删除重复记录。",
        Code::ImportMissingReference => "引用的代码不存在，请检查关联表是否完整、代码是否一致。",
        Code::ImportSubjectChoiceCount => "学生选考学科数量不符合本次设置。",
        Code::ImportDuplicateSubjectChoice => "同一学生重复选择了同一学科。",
        Code::ImportSectionSubjectNotSelected => "学生加入了未选择学科的教学班。",
        Code::ImportMultipleSectionsForSubject => "同一学生在同一学科加入了多个教学班。",
        Code::ImportMissingSectionForSubject => "学生所选学科缺少对应教学班成员关系。",
        Code::ImportSectionGradeMismatch => "学生所在年级与教学班年级不一致。",
        Code::ImportSectionBelowMinimum => "教学班实际人数低于规定最小班额。",
        Code::ImportSectionAboveMaximum => "教学班实际人数超过规定最大班额。",
        Code::ImportInvalidClassSizeRange => "班额需满足最小值 ≤ 目标值 ≤ 最大值，且最小值为正数。",
        Code::ImportInvalidMeetingPattern => "课时组合无效，需与每周课时总数一致。",
        Code::ImportInvalidRoomPolicy => "教室策略与候选教室字段不匹配。",
        Code::ImportInvalidTeacherAssignment => "教师分配方式与固定/候选教师字段不匹配。",
        Code::ImportCourseOfferingMismatch => "实际开课配置与课程计划或授课对象不一致。",
        Code::ImportFixedActivityMismatch => "固定活动与对应开课的课次、时长或资源不一致。",
        _ => "数据未通过校验，请按错误代码和位置检查。",
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Starts the local desktop runtime and installs the versioned command boundary.
///
/// # Panics
///
/// Panics when Tauri cannot initialize or the native event loop exits with an error. At this
/// boundary there is no caller that can recover; command-level failures remain structured values.
pub fn run() {
    tauri::Builder::default()
        .manage(solve_commands::SolveJobs::default())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if solve_commands::defer_shutdown(window.app_handle()) {
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            inspect_csv_bundle,
            list_workbook_sheets,
            project_queries::list_imported_projects,
            import_catalog::get_import_catalog,
            import_commit::commit_csv_bundle,
            import_commit::load_imported_project_receipt,
            solve_commands::start_project_solve,
            solve_commands::query_solve_job,
            solve_commands::cancel_solve_job,
            solve_commands::list_project_runs,
            solve_commands::load_project_run,
            scenario_commands::adopt_run_as_scenario,
            scenario_commands::copy_saved_scenario,
            scenario_commands::load_saved_scenario,
            scenario_commands::list_saved_scenarios,
            timetable_queries::query_saved_timetable,
            timetable_queries::query_saved_timetable_entities,
        ])
        .build(tauri::generate_context!())
        .expect("Tauri desktop runtime failed")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event
                && solve_commands::defer_shutdown(app)
            {
                api.prevent_exit();
            }
        });
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    fn fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/small")
    }

    pub(super) fn payloads(kinds: &[&str]) -> Vec<CsvDatasetPayload> {
        let root = fixture_root();
        kinds
            .iter()
            .map(|kind| CsvDatasetPayload {
                dataset: (*kind).to_owned(),
                bytes: fs::read(root.join(format!("{kind}.csv"))).expect("fixture file"),
            })
            .collect()
    }

    #[test]
    fn desktop_command_uses_real_input_b_sectioning_compile_and_precheck() {
        let response = inspect_csv_bundle_inner(InspectCsvBundleRequest {
            schema_version: 1,
            project_stable_key: "desktop-input-b".to_owned(),
            input_mode: InspectionInputMode::AutoSectioning,
            exact_subject_choices: 3,
            periods_per_day: 8,
            break_after_period: 4,
            auto_sectioning: Some(AutoSectioningRequest {
                minimum_size: 10,
                target_size: 12,
                maximum_size: 16,
                seed: 20_260_904,
                candidate_count: 2,
            }),
            workbooks: Vec::new(),
            datasets: payloads(&[
                "students",
                "administrative_classes",
                "student_subject_choices",
                "teachers",
                "teacher_unavailability",
                "rooms",
                "course_plans",
            ]),
        })
        .expect("desktop inspection");

        assert_eq!(response.import_counts.students, 24);
        assert_eq!(response.candidates.len(), 2);
        assert!(response.sectioning.is_some());
        assert!(response.candidates.iter().all(|candidate| {
            candidate.generated_sections == 6
                && candidate.generated_enrollments == 72
                && candidate.problem_counts.activities == 34
                && candidate.static_validation.passed
        }));

        let value = serde_json::to_value(response).expect("desktop response JSON");
        let first_candidate = &value["candidates"][0];
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["sectioning"]["seed"], "20260904");
        assert_eq!(first_candidate["staticValidation"]["passed"], true);
        assert!(first_candidate["staticValidation"]["hardProblems"].is_array());
        assert!(first_candidate["sectioningObjective"]["targetSizeDeviation"].is_string());
    }

    #[test]
    fn desktop_command_uses_real_input_a_compile_and_precheck() {
        let response = inspect_csv_bundle_inner(InspectCsvBundleRequest {
            schema_version: 1,
            project_stable_key: "desktop-input-a".to_owned(),
            input_mode: InspectionInputMode::ExistingSections,
            exact_subject_choices: 3,
            periods_per_day: 8,
            break_after_period: 4,
            auto_sectioning: None,
            workbooks: Vec::new(),
            datasets: payloads(&[
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
        })
        .expect("desktop Input A inspection");

        assert_eq!(response.import_counts.students, 24);
        assert_eq!(response.import_counts.teaching_sections, 6);
        assert!(response.sectioning.is_none());
        assert_eq!(response.candidates.len(), 1);
        assert_eq!(response.candidates[0].problem_counts.activities, 34);
        assert!(response.candidates[0].static_validation.passed);
    }

    #[test]
    fn malformed_import_returns_stable_structured_problems() {
        let error = inspect_csv_bundle_inner(InspectCsvBundleRequest {
            schema_version: 1,
            project_stable_key: "desktop-malformed".to_owned(),
            input_mode: InspectionInputMode::ExistingSections,
            exact_subject_choices: 3,
            periods_per_day: 8,
            break_after_period: 4,
            auto_sectioning: None,
            workbooks: Vec::new(),
            datasets: vec![CsvDatasetPayload {
                dataset: "students".to_owned(),
                bytes: b"not_a_valid_header\nvalue\n".to_vec(),
            }],
        })
        .expect_err("malformed import must fail");

        assert_eq!(error.code, "DESKTOP_IMPORT_REJECTED");
        let problems = error.details["problems"]
            .as_array()
            .expect("structured import problems");
        assert!(!problems.is_empty());
        assert!(problems.iter().all(|problem| problem["code"].is_string()));
        assert!(
            problems
                .iter()
                .all(|problem| problem["dataset"].is_string())
        );
    }

    #[test]
    fn command_rejects_unknown_dataset_before_business_processing() {
        let error = inspect_csv_bundle_inner(InspectCsvBundleRequest {
            schema_version: 1,
            project_stable_key: "desktop-test".to_owned(),
            input_mode: InspectionInputMode::ExistingSections,
            exact_subject_choices: 3,
            periods_per_day: 8,
            break_after_period: 4,
            auto_sectioning: None,
            workbooks: Vec::new(),
            datasets: vec![CsvDatasetPayload {
                dataset: "student".to_owned(),
                bytes: Vec::new(),
            }],
        })
        .expect_err("unknown dataset must fail");

        assert_eq!(error.code, "DESKTOP_UNKNOWN_DATASET");
    }
}
