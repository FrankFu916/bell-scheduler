import { desktopInvoke } from "./api.ts";
import { parseScenarioReceipt, type ScenarioReceipt } from "./scenarioApi.ts";
import { TIMETABLE_VIEWS, isTimetableEntityPageContent, isTimetablePageContent,
  type TimetableEntityPageContent, type TimetablePageContent, type TimetableView } from "./timetableApi.ts";

interface ScenarioTimetableEnvelope {
  readonly schemaVersion: 1;
  readonly receipt: ScenarioReceipt;
  readonly sourceIsCurrent: boolean;
  readonly scenarioDisplayName: string;
}
export interface ScenarioTimetableEntityPage extends ScenarioTimetableEnvelope, TimetableEntityPageContent {}
export interface ScenarioTimetablePage extends ScenarioTimetableEnvelope, TimetablePageContent {}

const record = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);
const uuid = (value: unknown): value is string => typeof value === "string" && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/u.test(value);
const integer = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && value <= 0xffff_ffff;

function invalidResponse(): never {
  throw { schemaVersion: 1, code: "DESKTOP_SCENARIO_TIMETABLE_INVALID_RESPONSE",
    message: "方案课表格式、身份或版本不一致，请重新打开已保存方案。", details: null };
}

function envelope(value: unknown): value is ScenarioTimetableEnvelope {
  if (!record(value) || value.schemaVersion !== 1 || typeof value.sourceIsCurrent !== "boolean" ||
      typeof value.scenarioDisplayName !== "string" || value.scenarioDisplayName.trim() === "" ||
      "runId" in value || "adopted" in value) return false;
  try { parseScenarioReceipt(value.receipt); return true; }
  catch { return false; }
}

/** Exact immutable receipt identity; scenario and timetable revisions never pass through Number. */
export function scenarioTimetableKey(receipt: ScenarioReceipt): string {
  return JSON.stringify([receipt.projectId, receipt.sourceProjectRevision, receipt.sourcePayloadHash,
    receipt.scenarioId, receipt.scenarioRevision, receipt.scenarioPayloadHash,
    receipt.timetableId, receipt.timetableRevision, receipt.timetablePayloadHash,
    receipt.originRunId, receipt.originArtifactHash, receipt.createdAt]);
}

export function scenarioTimetableRequest(receipt: ScenarioReceipt, view: TimetableView, offset: number, limit: number,
  entityId?: string): Record<string, unknown> {
  parseScenarioReceipt(receipt);
  if (!TIMETABLE_VIEWS.some((entry) => entry.value === view) || !integer(offset) || !integer(limit) ||
      limit < 1 || limit > 100 || offset + limit > 0xffff_ffff || entityId !== undefined && !uuid(entityId)) {
    throw { schemaVersion: 1, code: "DESKTOP_SCENARIO_TIMETABLE_INVALID_REQUEST",
      message: "请选择有效的方案课表视图、对象和分页范围。", details: null };
  }
  return { schemaVersion: 1, scenarioId: receipt.scenarioId, expectedScenarioRevision: receipt.scenarioRevision,
    expectedTimetableRevision: receipt.timetableRevision, view, offset, limit, ...(entityId === undefined ? {} : { entityId }) };
}

export function parseScenarioTimetableEntityPage(value: unknown): ScenarioTimetableEntityPage {
  if (!envelope(value) || !isTimetableEntityPageContent(value)) return invalidResponse();
  return value;
}

export function parseScenarioTimetablePage(value: unknown): ScenarioTimetablePage {
  if (!envelope(value) || !isTimetablePageContent(value)) return invalidResponse();
  return value;
}

export function checkedScenarioTimetableEntities(value: unknown, receipt: ScenarioReceipt, view: TimetableView,
  offset: number, limit: number): ScenarioTimetableEntityPage {
  const response = parseScenarioTimetableEntityPage(value);
  if (scenarioTimetableKey(response.receipt) !== scenarioTimetableKey(receipt) || response.view !== view ||
      response.offset !== offset || response.entities.length > limit ||
      response.hasMore && response.nextOffset !== offset + limit) return invalidResponse();
  return response;
}

export function checkedScenarioTimetablePage(value: unknown, receipt: ScenarioReceipt, entityId: string,
  offset: number, limit: number): ScenarioTimetablePage {
  const response = parseScenarioTimetablePage(value);
  if (scenarioTimetableKey(response.receipt) !== scenarioTimetableKey(receipt) || response.selection.id !== entityId ||
      response.offset !== offset || response.rows.length > limit ||
      response.hasMore && response.nextOffset !== offset + limit) return invalidResponse();
  return response;
}

export async function loadScenarioTimetableEntities(receipt: ScenarioReceipt, view: TimetableView, offset = 0,
  limit = 20): Promise<ScenarioTimetableEntityPage> {
  const value = await desktopInvoke("query_scenario_timetable_entities", {
    request: scenarioTimetableRequest(receipt, view, offset, limit),
  });
  return checkedScenarioTimetableEntities(value, receipt, view, offset, limit);
}

export async function loadScenarioTimetable(receipt: ScenarioReceipt, view: TimetableView, entityId: string,
  offset = 0, limit = 100): Promise<ScenarioTimetablePage> {
  const value = await desktopInvoke("query_scenario_timetable", {
    request: scenarioTimetableRequest(receipt, view, offset, limit, entityId),
  });
  return checkedScenarioTimetablePage(value, receipt, entityId, offset, limit);
}
