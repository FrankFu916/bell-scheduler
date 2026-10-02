import { desktopInvoke } from "./api.ts";
import { parseScenarioReceipt, type ScenarioReceipt } from "./scenarioApi.ts";
import { scenarioTimetableKey } from "./scenarioTimetableApi.ts";
import { isTimetableQualityTier, isTimetableRow, type TimetableDay, type TimetableQualityTier, type TimetableRow } from "./timetableApi.ts";

export type ScenarioEditOperation =
  | { readonly kind: "move"; readonly activityId: string; readonly startTimeslotId: string; readonly lockAfter: boolean }
  | { readonly kind: "swap_starts"; readonly leftActivityId: string; readonly rightActivityId: string }
  | { readonly kind: "lock_current" | "unlock"; readonly activityId: string };
export interface EditAssignment {
  readonly startTimeslotId: string; readonly durationPeriods: number; readonly roomId: string; readonly teacherId: string;
}
export interface ScenarioActivityChange {
  readonly activityId: string; readonly before: EditAssignment; readonly after: EditAssignment;
  readonly wasUserLocked: boolean; readonly isUserLocked: boolean;
}
export interface ScenarioEditTimeslot {
  readonly timeslotId: string; readonly day: TimetableDay; readonly dayLabel: string;
  readonly periodIndex: number; readonly periodLabel: string;
}
export interface ScenarioEditDiagnostic {
  readonly code: string; readonly message: string; readonly activityIds: readonly string[]; readonly activitiesTruncated: boolean;
}
export interface ScenarioEditContext {
  readonly activities: readonly TimetableRow[]; readonly calendar: readonly ScenarioEditTimeslot[];
  readonly diagnostics: readonly ScenarioEditDiagnostic[]; readonly totalDiagnostics: number;
  readonly diagnosticsTruncated: boolean; readonly activitiesTruncated: boolean;
}
interface EditHardProblem {
  readonly code: string; readonly message: string; readonly activityIndices: readonly number[];
  readonly entityIndices: Readonly<Record<string, number>>; readonly parameters: Readonly<Record<string, string>>;
}
export interface ScenarioEditPreview {
  readonly schemaVersion: 1; readonly receipt: ScenarioReceipt; readonly sourceIsCurrent: boolean;
  readonly status: "valid" | "no_change" | "hard_rejected"; readonly canCommit: boolean;
  readonly changes: readonly ScenarioActivityChange[];
  readonly validation: { readonly passed: boolean; readonly hardProblems: readonly EditHardProblem[];
    readonly totalHardProblems: number; readonly hardProblemsTruncated: boolean };
  readonly beforeQuality: readonly TimetableQualityTier[]; readonly afterQuality: readonly TimetableQualityTier[] | null;
  readonly context: ScenarioEditContext;
}
export interface ScenarioEditCommitResult {
  readonly schemaVersion: 1; readonly status: "committed" | "no_change"; readonly receipt: ScenarioReceipt;
}

const record = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);
const text = (value: unknown): value is string => typeof value === "string" && value.trim().length > 0;
const uuid = (value: unknown): value is string => typeof value === "string" && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/u.test(value);
const integer = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && value <= 0xffff_ffff;
const day = (value: unknown): value is TimetableDay => typeof value === "string" && ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"].includes(value);
const unique = (values: readonly unknown[]) => new Set(values).size === values.length;
const quality = (value: unknown): value is readonly TimetableQualityTier[] => Array.isArray(value) && value.every(isTimetableQualityTier);

function invalidResponse(): never {
  throw { schemaVersion: 1, code: "DESKTOP_SCENARIO_EDIT_INVALID_RESPONSE",
    message: "调课结果的身份、版本或校验信息不一致。请重新打开方案核对，当前结果不能用于确认提交。", details: null };
}

function validOperation(value: unknown): value is ScenarioEditOperation {
  if (!record(value)) return false;
  const keys = Object.keys(value).sort().join(",");
  if (value.kind === "move") return keys === "activityId,kind,lockAfter,startTimeslotId" && uuid(value.activityId) && uuid(value.startTimeslotId) && typeof value.lockAfter === "boolean";
  if (value.kind === "swap_starts") return keys === "kind,leftActivityId,rightActivityId" && uuid(value.leftActivityId) && uuid(value.rightActivityId);
  return keys === "activityId,kind" && (value.kind === "lock_current" || value.kind === "unlock") && uuid(value.activityId);
}

export function scenarioEditRequest(receipt: ScenarioReceipt, operation: ScenarioEditOperation): Record<string, unknown> {
  parseScenarioReceipt(receipt);
  if (!validOperation(operation)) throw { schemaVersion: 1, code: "DESKTOP_SCENARIO_EDIT_INVALID_REQUEST",
    message: "请选择有效课次、操作和目标课节后预览。", details: null };
  return { schemaVersion: 1, scenarioId: receipt.scenarioId, expectedScenarioRevision: receipt.scenarioRevision,
    expectedTimetableRevision: receipt.timetableRevision, operation };
}

export function scenarioEditKey(receipt: ScenarioReceipt, operation: ScenarioEditOperation): string {
  return JSON.stringify([scenarioTimetableKey(receipt), scenarioEditRequest(receipt, operation)]);
}

function assignment(value: unknown): value is EditAssignment {
  return record(value) && uuid(value.startTimeslotId) && integer(value.durationPeriods) && value.durationPeriods > 0 &&
    value.durationPeriods <= 255 && uuid(value.roomId) && uuid(value.teacherId);
}

function change(value: unknown): value is ScenarioActivityChange {
  return record(value) && uuid(value.activityId) && assignment(value.before) && assignment(value.after) &&
    typeof value.wasUserLocked === "boolean" && typeof value.isUserLocked === "boolean" &&
    value.before.durationPeriods === value.after.durationPeriods && value.before.roomId === value.after.roomId && value.before.teacherId === value.after.teacherId;
}

function timeslot(value: unknown): value is ScenarioEditTimeslot {
  return record(value) && uuid(value.timeslotId) && day(value.day) && text(value.dayLabel) && integer(value.periodIndex) &&
    value.periodIndex > 0 && value.periodIndex <= 65535 && text(value.periodLabel);
}

function diagnostic(value: unknown): value is ScenarioEditDiagnostic {
  return record(value) && text(value.code) && text(value.message) && Array.isArray(value.activityIds) &&
    value.activityIds.length <= 202 && value.activityIds.every(uuid) && unique(value.activityIds) && typeof value.activitiesTruncated === "boolean";
}

function context(value: unknown): value is ScenarioEditContext {
  if (!record(value) || !Array.isArray(value.activities) || value.activities.length > 202 || !value.activities.every(isTimetableRow) ||
      !Array.isArray(value.calendar) || value.calendar.length === 0 || value.calendar.length > 4096 || !value.calendar.every(timeslot) ||
      !Array.isArray(value.diagnostics) || value.diagnostics.length > 100 || !value.diagnostics.every(diagnostic) ||
      !integer(value.totalDiagnostics) || value.diagnostics.length !== Math.min(value.totalDiagnostics, 100) ||
      value.diagnosticsTruncated !== (value.totalDiagnostics > value.diagnostics.length) || typeof value.activitiesTruncated !== "boolean") return false;
  const rows = value.activities as TimetableRow[];
  const calendar = value.calendar as ScenarioEditTimeslot[];
  const ids = new Set(rows.map((row) => row.activityId));
  return ids.size === rows.length && unique(calendar.map((slot) => slot.timeslotId)) && unique(calendar.map((slot) => `${slot.day}:${slot.periodIndex}`)) &&
    value.diagnostics.every((item: ScenarioEditDiagnostic) => item.activityIds.every((id) => ids.has(id))) &&
    (!value.diagnostics.some((item: ScenarioEditDiagnostic) => item.activitiesTruncated) || value.activitiesTruncated);
}

function hard(value: unknown): value is EditHardProblem {
  return record(value) && text(value.code) && text(value.message) && Array.isArray(value.activityIndices) && value.activityIndices.every(integer) &&
    record(value.entityIndices) && Object.values(value.entityIndices).every(integer) && record(value.parameters) && Object.values(value.parameters).every((item) => typeof item === "string");
}

function sameAssignment(left: EditAssignment, right: EditAssignment): boolean {
  return left.startTimeslotId === right.startTimeslotId && left.durationPeriods === right.durationPeriods &&
    left.teacherId === right.teacherId && left.roomId === right.roomId;
}

function matchesOperation(changes: readonly ScenarioActivityChange[], operation: ScenarioEditOperation, valid: boolean): boolean {
  if (changes.length === 0) return true;
  if (operation.kind === "swap_starts") {
    const left = changes.find((item) => item.activityId === operation.leftActivityId);
    const right = changes.find((item) => item.activityId === operation.rightActivityId);
    return changes.length === 2 && left !== undefined && right !== undefined &&
      left.after.startTimeslotId === right.before.startTimeslotId && right.after.startTimeslotId === left.before.startTimeslotId &&
      changes.every((item) => item.wasUserLocked === item.isUserLocked);
  }
  const item = changes[0];
  if (changes.length !== 1 || item === undefined || item.activityId !== operation.activityId) return false;
  if (operation.kind === "move") return item.after.startTimeslotId === operation.startTimeslotId &&
    (!valid || item.isUserLocked === (operation.lockAfter || item.wasUserLocked));
  return sameAssignment(item.before, item.after) && (operation.kind === "lock_current"
    ? !item.wasUserLocked && item.isUserLocked : item.wasUserLocked && !item.isUserLocked);
}

export function checkedScenarioEditPreview(value: unknown, receipt: ScenarioReceipt, operation: ScenarioEditOperation): ScenarioEditPreview {
  scenarioEditRequest(receipt, operation);
  if (!record(value) || value.schemaVersion !== 1 || typeof value.sourceIsCurrent !== "boolean" || typeof value.canCommit !== "boolean" ||
      !["valid", "no_change", "hard_rejected"].includes(String(value.status)) || !Array.isArray(value.changes) || value.changes.length > 2 || !value.changes.every(change) ||
      !record(value.validation) || typeof value.validation.passed !== "boolean" || !Array.isArray(value.validation.hardProblems) ||
      !value.validation.hardProblems.every(hard) || !quality(value.beforeQuality) || !(value.afterQuality === null || quality(value.afterQuality)) || !context(value.context)) return invalidResponse();
  const baseline = parseScenarioReceipt(value.receipt);
  if (scenarioTimetableKey(baseline) !== scenarioTimetableKey(receipt) || "runId" in value || "error" in value) return invalidResponse();
  const validation = value.validation;
  const hardProblems = value.validation.hardProblems;
  const info = value.context;
  if (validation.totalHardProblems !== info.totalDiagnostics || validation.hardProblemsTruncated !== info.diagnosticsTruncated ||
      hardProblems.length !== info.diagnostics.length ||
      !hardProblems.every((problem, index) => problem.code === info.diagnostics[index]?.code) ||
      validation.passed !== (info.totalDiagnostics === 0)) return invalidResponse();
  if (value.status === "hard_rejected" ? validation.passed || value.afterQuality !== null || value.canCommit :
      !validation.passed || value.afterQuality === null || (value.status === "no_change" ? value.changes.length !== 0 || value.canCommit :
        value.changes.length === 0 || value.canCommit !== value.sourceIsCurrent)) return invalidResponse();
  const activityIds = operation.kind === "swap_starts" ? [operation.leftActivityId, operation.rightActivityId] : [operation.activityId];
  if (!matchesOperation(value.changes, operation, value.status === "valid") || !unique(value.changes.map((item) => item.activityId)) || value.changes.some((item) => !activityIds.includes(item.activityId) ||
      !info.activities.some((row) => row.activityId === item.activityId) ||
      !info.calendar.some((slot) => slot.timeslotId === item.before.startTimeslotId) ||
      !info.calendar.some((slot) => slot.timeslotId === item.after.startTimeslotId))) return invalidResponse();
  return value as unknown as ScenarioEditPreview;
}

function fixedIdentity(receipt: ScenarioReceipt): string {
  return JSON.stringify([receipt.projectId, receipt.sourceProjectRevision, receipt.sourcePayloadHash, receipt.scenarioId,
    receipt.timetableId, receipt.originRunId, receipt.originArtifactHash, receipt.createdAt]);
}

export function checkedScenarioEditCommit(value: unknown, baseline: ScenarioReceipt): ScenarioEditCommitResult {
  if (!record(value) || value.schemaVersion !== 1 || (value.status !== "committed" && value.status !== "no_change") || "error" in value || "runId" in value) return invalidResponse();
  const receipt = parseScenarioReceipt(value.receipt);
  if (fixedIdentity(receipt) !== fixedIdentity(baseline)) return invalidResponse();
  if (value.status === "no_change" ? scenarioTimetableKey(receipt) !== scenarioTimetableKey(baseline) :
      BigInt(receipt.scenarioRevision) !== BigInt(baseline.scenarioRevision) + 1n || BigInt(receipt.timetableRevision) !== BigInt(baseline.timetableRevision) + 1n ||
      receipt.scenarioPayloadHash === baseline.scenarioPayloadHash || receipt.timetablePayloadHash === baseline.timetablePayloadHash) return invalidResponse();
  return value as unknown as ScenarioEditCommitResult;
}

export async function previewScenarioEdit(receipt: ScenarioReceipt, operation: ScenarioEditOperation): Promise<ScenarioEditPreview> {
  const result = await desktopInvoke("preview_scenario_edit", { request: scenarioEditRequest(receipt, operation) });
  return checkedScenarioEditPreview(result, receipt, operation);
}

export function scenarioEditCommitRequest(preview: ScenarioEditPreview, operation: ScenarioEditOperation): Record<string, unknown> {
  const checked = checkedScenarioEditPreview(preview, preview.receipt, operation);
  if (!checked.canCommit) throw { schemaVersion: 1, code: "DESKTOP_SCENARIO_EDIT_PREVIEW_REQUIRED",
    message: "请先完成当前操作的有效预览，再确认提交。", details: null };
  return { ...scenarioEditRequest(checked.receipt, operation), expectedScenarioPayloadHash: checked.receipt.scenarioPayloadHash,
    expectedTimetablePayloadHash: checked.receipt.timetablePayloadHash };
}

export async function commitScenarioEdit(preview: ScenarioEditPreview, operation: ScenarioEditOperation): Promise<ScenarioEditCommitResult> {
  const result = await desktopInvoke("commit_scenario_edit", { request: scenarioEditCommitRequest(preview, operation) });
  return checkedScenarioEditCommit(result, preview.receipt);
}
