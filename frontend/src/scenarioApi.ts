import { desktopInvoke, type ImportedProjectReceipt } from "./api.ts";
import type { LoadedRun } from "./solveApi.ts";
import type { TimetableQualityTier } from "./timetableApi.ts";

export interface ScenarioReceipt {
  readonly schemaVersion: 1; readonly projectId: string; readonly sourceProjectRevision: string;
  readonly sourcePayloadHash: string; readonly scenarioId: string; readonly scenarioRevision: string;
  readonly scenarioPayloadHash: string; readonly timetableId: string; readonly timetableRevision: string;
  readonly timetablePayloadHash: string; readonly originRunId: string; readonly originArtifactHash: string;
  readonly createdAt: string;
}
export interface LoadedScenario {
  readonly schemaVersion: 1; readonly receipt: ScenarioReceipt; readonly displayName: string;
  readonly sourceIsCurrent: boolean; readonly activityCount: number; readonly materializedSectionCount: number;
  readonly materializedEnrollmentCount: number; readonly quality: readonly TimetableQualityTier[];
  readonly clonedFrom: null | { readonly scenarioId: string; readonly scenarioRevision: string;
    readonly timetableId: string; readonly timetableRevision: string };
}
export interface ScenarioSummary {
  readonly scenarioId: string; readonly openable: boolean; readonly projectId: string; readonly displayName: string;
  readonly scenarioRevision: string; readonly timetableId: string; readonly timetableRevision: string;
  readonly sourceProjectRevision: string; readonly createdAt: string;
}
export interface ScenarioPage {
  readonly schemaVersion: 1; readonly projectId: string; readonly scenarios: readonly ScenarioSummary[];
  readonly hasMore: boolean; readonly nextOffset: number | null;
}

const record = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);
const text = (value: unknown): value is string => typeof value === "string" && value.length > 0;
const uuid = (value: unknown): value is string => typeof value === "string" && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/u.test(value);
const hash = (value: unknown): value is string => typeof value === "string" && /^[0-9a-f]{64}$/u.test(value);
const revision = (value: unknown): value is string => typeof value === "string" && /^(?:0|[1-9][0-9]*)$/u.test(value) &&
  (value.length < 19 || value.length === 19 && value <= "9223372036854775807");
const integer = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && value <= 0xffff_ffff;
const signed = (value: unknown): value is string => typeof value === "string" && /^-?(?:0|[1-9][0-9]*)$/u.test(value);
const instant = (value: unknown): value is string => text(value) && Number.isFinite(Date.parse(value));

function invalidResponse(): never {
  throw { schemaVersion: 1, code: "DESKTOP_SCENARIO_INVALID_RESPONSE", message: "方案数据格式或身份不一致，请刷新方案列表重新打开。", details: null };
}

function receipt(value: unknown): value is ScenarioReceipt {
  return record(value) && value.schemaVersion === 1 && uuid(value.projectId) && revision(value.sourceProjectRevision) &&
    hash(value.sourcePayloadHash) && uuid(value.scenarioId) && revision(value.scenarioRevision) && hash(value.scenarioPayloadHash) &&
    uuid(value.timetableId) && revision(value.timetableRevision) && hash(value.timetablePayloadHash) && uuid(value.originRunId) &&
    hash(value.originArtifactHash) && instant(value.createdAt);
}

export function parseScenarioReceipt(value: unknown): ScenarioReceipt {
  if (!receipt(value)) return invalidResponse();
  return value;
}

function tier(value: unknown): value is TimetableQualityTier {
  return record(value) && text(value.id) && integer(value.priority) && value.priority > 0 && signed(value.value) &&
    Array.isArray(value.metrics) && value.metrics.every((metric: unknown) => record(metric) && text(metric.code) &&
      signed(metric.rawValue) && integer(metric.weightWithinTier) && metric.weightWithinTier > 0 && signed(metric.weightedValue));
}

export function parseLoadedScenario(value: unknown): LoadedScenario {
  if (!record(value) || value.schemaVersion !== 1 || !receipt(value.receipt) || !text(value.displayName) ||
      typeof value.sourceIsCurrent !== "boolean" || !integer(value.activityCount) || !integer(value.materializedSectionCount) ||
      !integer(value.materializedEnrollmentCount) || !Array.isArray(value.quality) || !value.quality.every(tier) ||
      !(value.clonedFrom === null || record(value.clonedFrom) && uuid(value.clonedFrom.scenarioId) &&
        revision(value.clonedFrom.scenarioRevision) && uuid(value.clonedFrom.timetableId) && revision(value.clonedFrom.timetableRevision) &&
        value.clonedFrom.scenarioId !== value.receipt.scenarioId && value.clonedFrom.timetableId !== value.receipt.timetableId)) return invalidResponse();
  return value as unknown as LoadedScenario;
}

export function parseScenarioPage(value: unknown): ScenarioPage {
  if (!record(value) || value.schemaVersion !== 1 || !uuid(value.projectId) || !Array.isArray(value.scenarios) ||
      value.scenarios.length > 100 || !value.scenarios.every((item: unknown) => record(item) && text(item.scenarioId) &&
        typeof item.openable === "boolean" && (!item.openable || uuid(item.scenarioId)) && item.projectId === value.projectId &&
        text(item.displayName) && revision(item.scenarioRevision) && text(item.timetableId) && revision(item.timetableRevision) &&
        revision(item.sourceProjectRevision) && instant(item.createdAt)) || typeof value.hasMore !== "boolean" ||
      !(value.hasMore ? integer(value.nextOffset) : value.nextOffset === null)) return invalidResponse();
  return value as unknown as ScenarioPage;
}

export function canAdoptRun(project: ImportedProjectReceipt, loaded: LoadedRun | null): boolean {
  return loaded !== null && loaded.failureCode === null && ["Feasible", "Optimal"].includes(loaded.run.status) &&
    loaded.run.projectId === project.projectId && loaded.run.revision === project.revision && loaded.run.sourcePayloadHash === project.payloadHash;
}

export function adoptScenarioRequest(run: LoadedRun, displayName: string): Record<string, unknown> {
  return { schemaVersion: 1, projectId: run.run.projectId, expectedSourceRevision: run.run.revision,
    runId: run.run.runId, displayName };
}

export function copyScenarioRequest(loaded: LoadedScenario, displayName: string): Record<string, unknown> {
  const source = loaded.receipt;
  return { schemaVersion: 1, projectId: source.projectId, expectedSourceRevision: source.sourceProjectRevision,
    parentScenarioId: source.scenarioId, expectedScenarioRevision: source.scenarioRevision,
    expectedTimetableRevision: source.timetableRevision, displayName };
}

export async function adoptRunAsScenario(run: LoadedRun, displayName: string): Promise<ScenarioReceipt> {
  const response = parseScenarioReceipt(await desktopInvoke("adopt_run_as_scenario", { request: adoptScenarioRequest(run, displayName) }));
  if (response.projectId !== run.run.projectId || response.sourceProjectRevision !== run.run.revision ||
      response.sourcePayloadHash !== run.run.sourcePayloadHash || response.originRunId !== run.run.runId ||
      response.originArtifactHash !== run.run.artifactPayloadHash) return invalidResponse();
  return response;
}

export async function copySavedScenario(loaded: LoadedScenario, displayName: string): Promise<ScenarioReceipt> {
  const response = parseScenarioReceipt(await desktopInvoke("copy_saved_scenario", { request: copyScenarioRequest(loaded, displayName) }));
  if (response.projectId !== loaded.receipt.projectId || response.sourceProjectRevision !== loaded.receipt.sourceProjectRevision ||
      response.sourcePayloadHash !== loaded.receipt.sourcePayloadHash || response.originRunId !== loaded.receipt.originRunId ||
      response.originArtifactHash !== loaded.receipt.originArtifactHash || response.scenarioId === loaded.receipt.scenarioId ||
      response.timetableId === loaded.receipt.timetableId) return invalidResponse();
  return response;
}

export async function loadSavedScenario(scenarioId: string): Promise<LoadedScenario> {
  const response = parseLoadedScenario(await desktopInvoke("load_saved_scenario", { request: { schemaVersion: 1, scenarioId } }));
  if (response.receipt.scenarioId !== scenarioId) return invalidResponse();
  return response;
}

export async function listSavedScenarios(projectId: string, offset = 0): Promise<ScenarioPage> {
  const response = parseScenarioPage(await desktopInvoke("list_saved_scenarios", { request: { schemaVersion: 1, projectId, limit: 20, offset } }));
  if (response.projectId !== projectId || response.scenarios.length > 20 || response.hasMore && response.nextOffset !== offset + 20) return invalidResponse();
  return response;
}
