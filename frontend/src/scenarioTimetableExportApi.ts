import { desktopInvoke } from "./api.ts";
import { parseScenarioReceipt, type ScenarioReceipt } from "./scenarioApi.ts";
import { scenarioTimetableKey } from "./scenarioTimetableApi.ts";
import { TIMETABLE_VIEWS, type TimetableEntity, type TimetableView } from "./timetableApi.ts";

export type ScenarioExportFormat = "xlsx" | "csv";
export interface ScenarioExportContext {
  readonly receipt: ScenarioReceipt;
  readonly view: TimetableView;
  readonly selection: TimetableEntity;
  readonly totalRows: number;
}
export interface ScenarioExportSaved {
  readonly schemaVersion: 1; readonly outcome: "saved"; readonly receipt: ScenarioReceipt;
  readonly sourceIsCurrent: boolean; readonly scenarioDisplayName: string;
  readonly view: TimetableView; readonly selection: TimetableEntity; readonly format: ScenarioExportFormat;
  readonly exportedActivityCount: number; readonly fileName: string; readonly generatedAt: string;
  readonly byteLength: string; readonly payloadHash: string; readonly payloadHashAlgorithm: "blake3";
}
export type ScenarioExportResult = ScenarioExportSaved | { readonly schemaVersion: 1; readonly outcome: "cancelled" };

const record = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);
const text = (value: unknown): value is string => typeof value === "string" && value.trim().length > 0;
const uuid = (value: unknown): value is string => typeof value === "string" && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/u.test(value);
const integer = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && value <= 0xffff_ffff;
const entity = (value: unknown): value is TimetableEntity => record(value) && uuid(value.id) && text(value.code) && text(value.label);
const view = (value: unknown): value is TimetableView => TIMETABLE_VIEWS.some((item) => item.value === value);
const format = (value: unknown): value is ScenarioExportFormat => value === "xlsx" || value === "csv";
const byteLength = (value: unknown): value is string => typeof value === "string" && /^[1-9][0-9]*$/u.test(value) &&
  (value.length < 20 || value.length === 20 && value <= "18446744073709551615");

function invalidResponse(): never {
  throw { schemaVersion: 1, code: "DESKTOP_SCENARIO_EXPORT_INVALID_RESPONSE",
    message: "导出回执的身份、格式或完整课次数不一致，无法确认导出结果。请检查所选保存位置。", details: null };
}

export function scenarioExportContextKey(context: ScenarioExportContext): string {
  return JSON.stringify([scenarioTimetableKey(context.receipt), context.view, context.selection.id,
    context.selection.code, context.selection.label, context.totalRows]);
}

export function scenarioExportRequest(context: ScenarioExportContext, selectedFormat: ScenarioExportFormat): Record<string, unknown> {
  parseScenarioReceipt(context.receipt);
  if (!view(context.view) || !entity(context.selection) || !integer(context.totalRows) || !format(selectedFormat)) {
    throw { schemaVersion: 1, code: "DESKTOP_SCENARIO_EXPORT_INVALID_REQUEST",
      message: "请打开已保存方案并选择有效课表对象后导出。", details: null };
  }
  return { schemaVersion: 1, scenarioId: context.receipt.scenarioId,
    expectedScenarioRevision: context.receipt.scenarioRevision, expectedTimetableRevision: context.receipt.timetableRevision,
    view: context.view, entityId: context.selection.id, format: selectedFormat };
}

function saved(value: unknown): value is ScenarioExportSaved {
  if (!record(value) || value.schemaVersion !== 1 || value.outcome !== "saved" ||
      typeof value.sourceIsCurrent !== "boolean" || !text(value.scenarioDisplayName) ||
      !view(value.view) || !entity(value.selection) || !format(value.format) || !integer(value.exportedActivityCount) ||
      !text(value.fileName) || /[/\\\u0000-\u001f\u007f]/u.test(value.fileName) || !value.fileName.toLowerCase().endsWith(`.${value.format}`) ||
      !text(value.generatedAt) || !Number.isFinite(Date.parse(value.generatedAt)) || !byteLength(value.byteLength) ||
      typeof value.payloadHash !== "string" || !/^[0-9a-f]{64}$/u.test(value.payloadHash) || value.payloadHashAlgorithm !== "blake3" ||
      "runId" in value || "error" in value) return false;
  try { parseScenarioReceipt(value.receipt); return true; }
  catch { return false; }
}

export function checkedScenarioExportResult(value: unknown, context: ScenarioExportContext,
  selectedFormat: ScenarioExportFormat): ScenarioExportResult {
  if (record(value) && value.schemaVersion === 1 && value.outcome === "cancelled") {
    if (Object.keys(value).some((key) => key !== "schemaVersion" && key !== "outcome")) return invalidResponse();
    return { schemaVersion: 1, outcome: "cancelled" };
  }
  if (!saved(value) || scenarioTimetableKey(value.receipt) !== scenarioTimetableKey(context.receipt) ||
      value.view !== context.view || value.selection.id !== context.selection.id ||
      value.selection.code !== context.selection.code || value.selection.label !== context.selection.label ||
      value.format !== selectedFormat || value.exportedActivityCount !== context.totalRows) return invalidResponse();
  return value;
}

export async function exportScenarioTimetable(context: ScenarioExportContext, selectedFormat: ScenarioExportFormat): Promise<ScenarioExportResult> {
  const result = await desktopInvoke("export_scenario_timetable", { request: scenarioExportRequest(context, selectedFormat) });
  return checkedScenarioExportResult(result, context, selectedFormat);
}

/** One live request per mounted selection; invalidated requests cannot update its successor. */
export function createScenarioExportGuard() {
  let active: symbol | null = null;
  return {
    begin(): symbol | null {
      if (active !== null) return null;
      active = Symbol("scenario export");
      return active;
    },
    accepts(token: symbol): boolean { return active === token; },
    finish(token: symbol): void { if (active === token) active = null; },
    invalidate(): void { active = null; },
  };
}
