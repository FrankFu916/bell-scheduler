import { invoke, isTauri } from "@tauri-apps/api/core";

export interface SelectedCsv {
  readonly file: File;
  readonly dataset: string;
}

export interface SelectedWorkbook {
  readonly file: File;
  readonly sheetMappings: readonly { readonly sheetName: string; readonly dataset: string }[];
}

export type SelectedImportFile = SelectedCsv | SelectedWorkbook;

export function isWorkbookSelection(file: SelectedImportFile): file is SelectedWorkbook {
  return "sheetMappings" in file;
}

async function readSelectedFile(file: File): Promise<ArrayBuffer> {
  let bytes: ArrayBuffer;
  try {
    bytes = await file.arrayBuffer();
  } catch {
    throw localCommandError("DESKTOP_FILE_READ_FAILED", "无法完整读取所选文件。文件可能已移动、修改或尚未下载到本机；请重新选择本机可读文件。", { fileName: file.name });
  }
  if (bytes.byteLength !== file.size) {
    throw localCommandError("DESKTOP_FILE_CHANGED", "读取时文件大小发生变化，请重新选择全部文件后再试。", { fileName: file.name });
  }
  return bytes;
}

export async function selectImportFiles(files: readonly File[], catalog: readonly DatasetTemplate[]): Promise<SelectedImportFile[]> {
  if (files.reduce((total, file) => total + file.size, 0) > MAXIMUM_FRONTEND_IMPORT_BYTES) {
    throw localCommandError("DESKTOP_IMPORT_TOO_LARGE", "所选文件总大小不能超过 32 MiB。");
  }
  const selected: SelectedImportFile[] = [];
  for (const file of files) {
    if (/\.xlsx$/iu.test(file.name)) {
      const response = await desktopInvoke("list_workbook_sheets", { bytes: Array.from(new Uint8Array(await readSelectedFile(file))) });
      if (!isRecord(response) || response.schemaVersion !== 1 || !Array.isArray(response.sheetNames) ||
          response.sheetNames.length === 0 || !response.sheetNames.every((name: unknown) => typeof name === "string")) {
        throw localCommandError("DESKTOP_INVALID_RESPONSE", "无法读取工作簿的数据表目录。");
      }
      selected.push({ file, sheetMappings: (response.sheetNames as string[]).map((sheetName) => ({
        sheetName, dataset: catalog.find((entry) => entry.dataset === sheetName)?.dataset ?? "",
      })) });
    } else if (/\.csv$/iu.test(file.name)) {
      selected.push(...selectCsvFiles([file], catalog));
    } else {
      throw localCommandError("DESKTOP_UNSUPPORTED_FILE_FORMAT", "请选择 .csv 或 .xlsx 文件。旧版 .xls 请在表格软件中另存为 .xlsx。", { fileName: file.name });
    }
  }
  return selected;
}

export interface DatasetTemplate {
  readonly dataset: string;
  readonly label: string;
  readonly required: boolean;
  readonly requiredHeaders: readonly string[];
  readonly optionalHeaders: readonly string[];
}

export async function desktopInvoke(command: string, args?: Record<string, unknown>): Promise<unknown> {
  if (!isTauri()) {
    throw localCommandError("DESKTOP_RUNTIME_REQUIRED", "当前页面没有连接本机 Rust 服务。请从 Tauri 桌面应用打开；单独运行前端网页只能预览界面，无法导入或保存。开发启动命令见 README。 ");
  }
  return invoke<unknown>(command, args);
}

export async function loadImportCatalog(): Promise<readonly DatasetTemplate[]> {
  const response = await desktopInvoke("get_import_catalog");
  if (!isRecord(response) || response.schemaVersion !== 1 || !Array.isArray(response.datasets) ||
      response.datasets.length === 0 || !response.datasets.every((item: unknown) =>
        isRecord(item) && typeof item.dataset === "string" && typeof item.label === "string" &&
        typeof item.required === "boolean" && Array.isArray(item.requiredHeaders) &&
        item.requiredHeaders.every((field: unknown) => typeof field === "string") &&
        Array.isArray(item.optionalHeaders) && item.optionalHeaders.every((field: unknown) => typeof field === "string"))) {
    throw localCommandError("DESKTOP_INVALID_RESPONSE", "无法读取 Rust 导入模板目录，请重新启动与当前前端版本一致的桌面应用。");
  }
  return response.datasets as DatasetTemplate[];
}

export function selectCsvFiles(files: readonly File[], catalog: readonly DatasetTemplate[]): SelectedCsv[] {
  return files.map((file) => ({
    file,
    dataset: catalog.find((entry) => entry.dataset === file.name.toLowerCase().replace(/\.csv$/u, ""))?.dataset ?? "",
  }));
}

export type InspectionInputMode = "existing_sections" | "auto_sectioning";

export interface InspectionConfig {
  readonly projectStableKey: string;
  readonly inputMode: InspectionInputMode;
  readonly exactSubjectChoices: number;
  readonly periodsPerDay: number;
  readonly breakAfterPeriod: number;
  readonly minimumSize: number;
  readonly targetSize: number;
  readonly maximumSize: number;
  readonly seed: number;
  readonly candidateCount: number;
}

export interface ValidationProblem {
  readonly code: string;
  readonly message?: string;
  readonly activityIndices: readonly number[];
  readonly entityIndices: Readonly<Record<string, number>>;
  readonly parameters: Readonly<Record<string, string>>;
}

export interface ValidationReport {
  readonly passed: boolean;
  readonly hardProblems: readonly ValidationProblem[];
}

export interface SectioningDiagnostic {
  readonly code: string;
  readonly severity: "error" | "warning";
  readonly studentId: string | null;
  readonly gradeId: string | null;
  readonly subjectId: string | null;
  readonly sectionId: string | null;
  readonly expectedMin: string | null;
  readonly expectedMax: string | null;
  readonly actual: string | null;
}

export interface Objective {
  readonly targetSizeDeviation: string;
  readonly sizeImbalance: string;
  readonly timetableFeasibility: {
    readonly concentratedAudienceOverlap: string;
    readonly resourceAvailabilityPenalty: string;
    readonly roomCapacityTightness: string;
  };
}

export interface CandidateInspection {
  readonly candidateIndex: number;
  readonly candidateHash: string | null;
  readonly sectioningObjective: Objective | null;
  readonly generatedSections: number;
  readonly generatedEnrollments: number;
  readonly problemCounts: {
    readonly students: number;
    readonly teachers: number;
    readonly rooms: number;
    readonly timeslots: number;
    readonly activities: number;
    readonly studentConflictEdges: number;
  };
  readonly staticValidation: ValidationReport;
}

export interface InspectCsvBundleResponse {
  readonly schemaVersion: number;
  readonly inputMode: InspectionInputMode;
  readonly importCounts: {
    readonly students: number;
    readonly administrativeClasses: number;
    readonly subjectChoices: number;
    readonly teachers: number;
    readonly rooms: number;
    readonly coursePlans: number;
    readonly teachingSections: number;
    readonly sectionEnrollments: number;
  };
  readonly sectioning: null | {
    readonly algorithmVersion: string;
    readonly inputHash: string;
    readonly seed: string;
    readonly profile: string;
    readonly requestedCandidates: number;
    readonly generatedCandidates: number;
    readonly diagnostics: readonly SectioningDiagnostic[];
  };
  readonly candidates: readonly CandidateInspection[];
}

export interface CommandError {
  readonly schemaVersion: number;
  readonly code: string;
  readonly message: string;
  readonly details: unknown;
}

export type ImportCommitIntent =
  | { readonly mode: "create" }
  | { readonly mode: "replace"; readonly expectedRevision: string };

export interface ImportedProjectReceipt {
  readonly schemaVersion: number;
  readonly projectId: string;
  readonly revision: string;
  readonly displayName: string;
  readonly projectStableKey: string;
  readonly documentSchemaVersion: number;
  readonly payloadHash: string;
  readonly payloadHashAlgorithm: "blake3";
  readonly sectioningRequired: boolean;
  readonly databasePath: string;
}

export interface ImportCommitTarget {
  readonly projectId: string;
  readonly displayName: string;
  readonly intent: ImportCommitIntent;
}

export interface LocalProjectSummary {
  readonly projectId: string;
  readonly displayName: string;
  readonly revision: string;
  readonly updatedAt: string;
  readonly canOpen: boolean;
}

export interface LocalProjectPage {
  readonly schemaVersion: number;
  readonly projects: readonly LocalProjectSummary[];
  readonly hasMore: boolean;
  readonly nextOffset: number | null;
}

export async function listLocalProjects(offset = 0): Promise<LocalProjectPage> {
  const value = await desktopInvoke("list_imported_projects", { request: { schemaVersion: 1, limit: 20, offset } });
  if (!isRecord(value) || value.schemaVersion !== 1 || !Array.isArray(value.projects) ||
      !value.projects.every((item: unknown) => isRecord(item) && typeof item.projectId === "string" &&
        typeof item.displayName === "string" && typeof item.revision === "string" && /^(?:0|[1-9][0-9]*)$/u.test(item.revision) &&
        typeof item.updatedAt === "string" && typeof item.canOpen === "boolean") ||
      typeof value.hasMore !== "boolean" || !(value.nextOffset === null || isFiniteNonNegativeInteger(value.nextOffset)) ||
      value.hasMore !== (value.nextOffset !== null)) {
    throw localCommandError("DESKTOP_INVALID_RESPONSE", "本机项目列表格式不符合约定，请重新读取。");
  }
  return value as unknown as LocalProjectPage;
}

const COMMAND_SCHEMA_VERSION = 1;
const MAXIMUM_FRONTEND_IMPORT_BYTES = 32 * 1024 * 1024;
const MAXIMUM_U16 = 65_535;
const MAXIMUM_SECTIONING_CANDIDATES = 16;

function localCommandError(
  code: string,
  message: string,
  details: unknown = null,
): CommandError {
  return {
    schemaVersion: COMMAND_SCHEMA_VERSION,
    code,
    message,
    details,
  };
}

function integerProblem(
  label: string,
  value: number,
  minimum: number,
  maximum: number,
): CommandError | null {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    return localCommandError(
      "DESKTOP_INVALID_NUMERIC_INPUT",
      `${label}必须是 ${minimum} 到 ${maximum} 之间的整数。`,
      { label, minimum, maximum, actual: value },
    );
  }
  return null;
}

export function validateInspectionInput(
  files: readonly File[],
  config: InspectionConfig,
): CommandError | null {
  if (files.length === 0) {
    return localCommandError(
      "DESKTOP_NO_IMPORT_FILES",
      "至少选择一个 CSV 或 XLSX 文件。",
    );
  }
  if (config.projectStableKey.trim().length === 0) {
    return localCommandError(
      "DESKTOP_BLANK_PROJECT_STABLE_KEY",
      "项目稳定键不能为空。",
    );
  }

  const commonNumericProblems = [
    integerProblem("每生选科数", config.exactSubjectChoices, 1, MAXIMUM_U16),
    integerProblem("每日课时", config.periodsPerDay, 2, MAXIMUM_U16),
    integerProblem("午休前最后一节", config.breakAfterPeriod, 1, MAXIMUM_U16),
  ];
  const commonProblem = commonNumericProblems.find(
    (problem): problem is CommandError => problem !== null,
  );
  if (commonProblem !== undefined) {
    return commonProblem;
  }
  if (config.breakAfterPeriod >= config.periodsPerDay) {
    return localCommandError(
      "DESKTOP_INVALID_BREAK_POSITION",
      "午休分界必须早于当天最后一节课。",
      {
        breakAfterPeriod: config.breakAfterPeriod,
        periodsPerDay: config.periodsPerDay,
      },
    );
  }

  if (config.inputMode === "auto_sectioning") {
    const sectioningNumericProblems = [
      integerProblem("最小班额", config.minimumSize, 1, MAXIMUM_U16),
      integerProblem("目标班额", config.targetSize, 1, MAXIMUM_U16),
      integerProblem("最大班额", config.maximumSize, 1, MAXIMUM_U16),
      integerProblem(
        "候选数量",
        config.candidateCount,
        1,
        MAXIMUM_SECTIONING_CANDIDATES,
      ),
      integerProblem("随机种子", config.seed, 0, Number.MAX_SAFE_INTEGER),
    ];
    const sectioningProblem = sectioningNumericProblems.find(
      (problem): problem is CommandError => problem !== null,
    );
    if (sectioningProblem !== undefined) {
      return sectioningProblem;
    }
    if (
      config.minimumSize > config.targetSize ||
      config.targetSize > config.maximumSize
    ) {
      return localCommandError(
        "DESKTOP_INVALID_SECTION_SIZE_ORDER",
        "班额必须满足最小班额 ≤ 目标班额 ≤ 最大班额。",
        {
          minimumSize: config.minimumSize,
          targetSize: config.targetSize,
          maximumSize: config.maximumSize,
        },
      );
    }
  }

  let totalBytes = 0;
  for (const file of files) {
    totalBytes += file.size;
    if (
      !Number.isSafeInteger(totalBytes) ||
      totalBytes > MAXIMUM_FRONTEND_IMPORT_BYTES
    ) {
      return localCommandError(
        "DESKTOP_IMPORT_TOO_LARGE",
        "所选文件总大小不能超过 32 MiB。",
        {
          maximumBytes: MAXIMUM_FRONTEND_IMPORT_BYTES,
          actualBytes: totalBytes,
        },
      );
    }
  }

  return null;
}

export async function csvImportRequest(
  selections: readonly SelectedImportFile[],
  config: InspectionConfig,
) {
  const files = selections.map((selection) => selection.file);
  const inputProblem = validateInspectionInput(files, config);
  if (inputProblem !== null) {
    throw inputProblem;
  }

  const datasets: Array<{ dataset: string; bytes: number[] }> = [];
  const workbooks: Array<{ bytes: number[]; sheetMappings: SelectedWorkbook["sheetMappings"] }> = [];
  const seen = new Set<string>();
  for (const selection of selections) {
    const { file } = selection;
    if (!(isWorkbookSelection(selection) ? /\.xlsx$/iu : /\.csv$/iu).test(file.name)) {
      throw localCommandError("DESKTOP_UNSUPPORTED_FILE_FORMAT", "文件扩展名与所选格式不一致，请重新添加 CSV 或 XLSX 文件。", { fileName: file.name });
    }
    const mappings = isWorkbookSelection(selection) ? selection.sheetMappings : [selection];
    if (mappings.length === 0) throw localCommandError("DESKTOP_DATASET_SELECTION_REQUIRED", "工作簿没有可导入的数据表。");
    for (const { dataset } of mappings) {
      if (dataset === "") {
        throw localCommandError("DESKTOP_DATASET_SELECTION_REQUIRED", "请为每个文件及工作表选择对应的数据表类型。", { fileName: file.name });
      }
      if (seen.has(dataset)) {
        throw localCommandError("DESKTOP_DUPLICATE_DATASET_SELECTION", "多个文件或工作表对应同一数据表，请保留一个完整数据表或修正对应关系。", { dataset });
      }
      seen.add(dataset);
    }
  }
  for (const selection of selections) {
    const bytes = Array.from(new Uint8Array(await readSelectedFile(selection.file)));
    if (isWorkbookSelection(selection)) {
      workbooks.push({ bytes, sheetMappings: selection.sheetMappings });
    } else {
      datasets.push({ dataset: selection.dataset, bytes });
    }
  }
  const autoSectioning =
    config.inputMode === "auto_sectioning"
      ? {
          minimumSize: config.minimumSize,
          targetSize: config.targetSize,
          maximumSize: config.maximumSize,
          seed: config.seed,
          candidateCount: config.candidateCount,
        }
      : null;
  return {
    schemaVersion: COMMAND_SCHEMA_VERSION,
    projectStableKey: config.projectStableKey,
    inputMode: config.inputMode,
    exactSubjectChoices: config.exactSubjectChoices,
    periodsPerDay: config.periodsPerDay,
    breakAfterPeriod: config.breakAfterPeriod,
    autoSectioning,
    datasets,
    workbooks,
  };
}

export async function inspectCsvFiles(
  files: readonly SelectedImportFile[],
  config: InspectionConfig,
): Promise<InspectCsvBundleResponse> {
  const request = await csvImportRequest(files, config);
  const response = await desktopInvoke("inspect_csv_bundle", { request });
  if (!isInspectionResponse(response)) {
    throw localCommandError(
      "DESKTOP_INVALID_RESPONSE",
      "本地 Rust 返回了不符合约定的导入审计结果。",
    );
  }
  return response;
}

export async function commitCsvFiles(
  files: readonly SelectedImportFile[],
  config: InspectionConfig,
  target: ImportCommitTarget,
): Promise<ImportedProjectReceipt> {
  const imported = await csvImportRequest(files, config);
  const response = await desktopInvoke("commit_csv_bundle", {
    request: {
      schemaVersion: COMMAND_SCHEMA_VERSION,
      ...target,
      import: imported,
    },
  });
  return checkedReceipt(response);
}

export async function loadImportedProject(projectId: string): Promise<ImportedProjectReceipt> {
  const response = await desktopInvoke("load_imported_project_receipt", {
    request: { schemaVersion: COMMAND_SCHEMA_VERSION, projectId },
  });
  return checkedReceipt(response);
}

function checkedReceipt(value: unknown): ImportedProjectReceipt {
  if (
    !isRecord(value) ||
    value.schemaVersion !== COMMAND_SCHEMA_VERSION ||
    typeof value.projectId !== "string" ||
    !/^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/u.test(value.projectId) ||
    typeof value.revision !== "string" ||
    !/^(?:0|[1-9][0-9]*)$/u.test(value.revision) ||
    value.revision.length > 19 ||
    BigInt(value.revision) > 9_223_372_036_854_775_807n ||
    typeof value.displayName !== "string" ||
    typeof value.projectStableKey !== "string" ||
    !isFiniteNonNegativeInteger(value.documentSchemaVersion) ||
    value.documentSchemaVersion === 0 ||
    typeof value.payloadHash !== "string" ||
    !/^[0-9a-f]{64}$/u.test(value.payloadHash) ||
    value.payloadHashAlgorithm !== "blake3" ||
    typeof value.sectioningRequired !== "boolean" ||
    typeof value.databasePath !== "string" ||
    value.databasePath.length === 0
  ) {
    throw localCommandError(
      "DESKTOP_INVALID_RESPONSE",
      "无法确认本地项目回执。请按项目 ID 重新读取状态后再决定是否提交。",
    );
  }
  return value as unknown as ImportedProjectReceipt;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isFiniteNonNegativeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function hasCountFields(value: unknown, fields: readonly string[]): boolean {
  return (
    isRecord(value) &&
    fields.every((field) => isFiniteNonNegativeInteger(value[field]))
  );
}

function isValidationReport(value: unknown): boolean {
  if (!isRecord(value) || typeof value.passed !== "boolean") {
    return false;
  }
  if (!Array.isArray(value.hardProblems)) {
    return false;
  }
  return value.hardProblems.every((problem) => {
    if (!isRecord(problem)) {
      return false;
    }
    return (
      typeof problem.code === "string" &&
      (problem.message === undefined || typeof problem.message === "string") &&
      Array.isArray(problem.activityIndices) &&
      problem.activityIndices.every(isFiniteNonNegativeInteger) &&
      isRecord(problem.entityIndices) &&
      Object.values(problem.entityIndices).every(isFiniteNonNegativeInteger) &&
      isRecord(problem.parameters) &&
      Object.values(problem.parameters).every(
        (parameter) => typeof parameter === "string",
      )
    );
  });
}

function isCandidateInspection(value: unknown): value is CandidateInspection {
  if (!isRecord(value) || !isFiniteNonNegativeInteger(value.candidateIndex)) {
    return false;
  }
  if (value.candidateHash !== null && typeof value.candidateHash !== "string") {
    return false;
  }
  return (
    (value.sectioningObjective === null || isRecord(value.sectioningObjective)) &&
    isFiniteNonNegativeInteger(value.generatedSections) &&
    isFiniteNonNegativeInteger(value.generatedEnrollments) &&
    hasCountFields(value.problemCounts, [
      "students",
      "teachers",
      "rooms",
      "timeslots",
      "activities",
      "studentConflictEdges",
    ]) &&
    isValidationReport(value.staticValidation)
  );
}

function isInspectionResponse(value: unknown): value is InspectCsvBundleResponse {
  if (!isRecord(value) || value.schemaVersion !== COMMAND_SCHEMA_VERSION) {
    return false;
  }
  if (value.inputMode !== "existing_sections" && value.inputMode !== "auto_sectioning") {
    return false;
  }
  if (
    !hasCountFields(value.importCounts, [
      "students",
      "administrativeClasses",
      "subjectChoices",
      "teachers",
      "rooms",
      "coursePlans",
      "teachingSections",
      "sectionEnrollments",
    ]) ||
    !Array.isArray(value.candidates) ||
    !value.candidates.every(isCandidateInspection)
  ) {
    return false;
  }
  return value.sectioning === null || isRecord(value.sectioning);
}

export function commandError(error: unknown): CommandError {
  if (typeof error === "string" && error.trim().startsWith("{")) {
    try {
      const parsed: unknown = JSON.parse(error);
      if (isRecord(parsed)) return commandError(parsed);
    } catch {
      // A non-JSON transport message is still displayed below.
    }
  }
  if (
    typeof error === "object" &&
    error !== null &&
    "code" in error &&
    typeof error.code === "string" &&
    "message" in error &&
    typeof error.message === "string"
  ) {
    return {
      schemaVersion:
        "schemaVersion" in error && typeof error.schemaVersion === "number"
          ? error.schemaVersion
          : 1,
      code: error.code,
      message: error.message.trim() || "操作未完成，后端未返回说明。请保留错误代码并重新读取项目状态。",
      details: "details" in error ? error.details : null,
    };
  }
  return {
    schemaVersion: 1,
    code: "DESKTOP_TRANSPORT_ERROR",
    message: isRecord(error) && typeof error.message === "string" && error.message.trim() !== ""
      ? error.message
      : typeof error === "string" && error.trim() !== "" && error.trim() !== "{}"
        ? error
        : "本机操作未完成，未收到可识别的错误说明。请确认正在使用桌面应用，重新选择本机文件；若已点击提交，请先按项目 ID 重新读取状态。",
    details: null,
  };
}
