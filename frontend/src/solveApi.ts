import { desktopInvoke, type ImportedProjectReceipt } from "./api.ts";

export type SolverStatus = "Optimal" | "Feasible" | "ProvenInfeasible" | "Timeout" | "Unknown" | "Cancelled" | "InvalidInput" | "InvalidModel" | "InternalError";
const statuses: readonly string[] = ["Optimal", "Feasible", "ProvenInfeasible", "Timeout", "Unknown", "Cancelled", "InvalidInput", "InvalidModel", "InternalError"];

export interface SolveSettings {
  readonly seed: string;
  readonly execution: "reproducible" | "fast";
  readonly workerCount: number;
  readonly timeLimitSeconds: number;
  readonly minimumSize: number;
  readonly targetSize: number;
  readonly maximumSize: number;
  readonly candidateCount: number;
}

export interface SavedRun {
  readonly runId: string;
  readonly projectId: string;
  readonly revision: string;
  readonly status: SolverStatus;
  readonly terminationCode: string;
  readonly artifactSchemaVersion: number;
  readonly sourcePayloadHash: string;
  readonly artifactPayloadHash: string;
  readonly startedAt: string;
  readonly finishedAt: string;
  readonly adopted: false;
}

export interface SolveJob {
  readonly schemaVersion: 1;
  readonly jobId: string;
  readonly projectId: string;
  readonly revision: string;
  readonly state: "running" | "completed" | "failed";
  readonly cancellationRequested: boolean;
  readonly elapsedMillis: string;
  readonly run: SavedRun | null;
  readonly error: null | { readonly schemaVersion: number; readonly code: string; readonly message: string; readonly details: unknown };
}

export interface RunSummary {
  readonly runId: string;
  readonly projectId: string;
  readonly revision: string;
  readonly status: SolverStatus;
  readonly artifactSchemaVersion: number;
  readonly startedAt: string;
  readonly finishedAt: string;
}

export interface RunPage {
  readonly schemaVersion: 1;
  readonly runs: readonly RunSummary[];
  readonly hasMore: boolean;
  readonly nextOffset: number | null;
}

export interface LoadedRun {
  readonly schemaVersion: 1;
  readonly run: SavedRun;
  readonly failureCode: string | null;
  readonly recordedAttemptCount: number;
}

const record = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);
const integer = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
const decimal = (value: unknown): value is string => typeof value === "string" && /^(0|[1-9][0-9]*)$/u.test(value);

function summary(value: unknown): value is RunSummary {
  return record(value) && typeof value.runId === "string" && typeof value.projectId === "string" && decimal(value.revision) &&
    typeof value.status === "string" && statuses.includes(value.status) && integer(value.artifactSchemaVersion) &&
    typeof value.startedAt === "string" && typeof value.finishedAt === "string";
}

function saved(value: unknown): value is SavedRun {
  return summary(value) && record(value) && typeof value.terminationCode === "string" &&
    typeof value.sourcePayloadHash === "string" && typeof value.artifactPayloadHash === "string" && value.adopted === false;
}

function invalidResponse(): never { throw new Error("本机排课服务返回的结构不完整，请重新启动与前端匹配的 Bell 应用。"); }

export function parseSolveJob(value: unknown): SolveJob {
  if (!record(value) || value.schemaVersion !== 1 || typeof value.jobId !== "string" || typeof value.projectId !== "string" ||
      !decimal(value.revision) || !["running", "completed", "failed"].includes(String(value.state)) ||
      typeof value.cancellationRequested !== "boolean" || !decimal(value.elapsedMillis) ||
      !(value.run === null || saved(value.run)) || !(value.error === null || (record(value.error) &&
        typeof value.error.code === "string" && typeof value.error.message === "string" && integer(value.error.schemaVersion))) ||
      (value.state === "completed" && (value.run === null || value.error !== null)) ||
      (value.state === "failed" && (value.error === null || value.run !== null)) ||
      (value.state === "running" && (value.run !== null || value.error !== null))) invalidResponse();
  return value as unknown as SolveJob;
}

export function solveRequest(project: ImportedProjectReceipt, settings: SolveSettings): Record<string, unknown> {
  return { schemaVersion: 1, projectId: project.projectId, expectedRevision: project.revision,
    inputMode: project.sectioningRequired ? "auto_sectioning" : "existing_sections",
    seed: settings.seed, execution: settings.execution, workerCount: settings.workerCount,
    timeLimitSeconds: settings.timeLimitSeconds,
    autoSectioning: project.sectioningRequired ? { minimumSize: settings.minimumSize, targetSize: settings.targetSize,
      maximumSize: settings.maximumSize, candidateCount: settings.candidateCount } : null };
}

export async function startSolve(project: ImportedProjectReceipt, settings: SolveSettings): Promise<SolveJob> {
  return parseSolveJob(await desktopInvoke("start_project_solve", { request: solveRequest(project, settings) }));
}

export async function querySolveJob(jobId: string): Promise<SolveJob> {
  return parseSolveJob(await desktopInvoke("query_solve_job", { request: { schemaVersion: 1, jobId } }));
}

export async function cancelSolveJob(jobId: string): Promise<SolveJob> {
  return parseSolveJob(await desktopInvoke("cancel_solve_job", { request: { schemaVersion: 1, jobId } }));
}

export async function listProjectRuns(projectId: string, offset = 0): Promise<RunPage> {
  const value = await desktopInvoke("list_project_runs", { request: { schemaVersion: 1, projectId, limit: 20, offset } });
  if (!record(value) || value.schemaVersion !== 1 || !Array.isArray(value.runs) || !value.runs.every(summary) ||
      typeof value.hasMore !== "boolean" || !(value.nextOffset === null || integer(value.nextOffset)) ||
      value.hasMore !== (value.nextOffset !== null)) invalidResponse();
  return value as unknown as RunPage;
}

export async function loadProjectRun(runId: string): Promise<LoadedRun> {
  const value = await desktopInvoke("load_project_run", { request: { schemaVersion: 1, runId } });
  if (!record(value) || value.schemaVersion !== 1 || !saved(value.run) ||
      !(value.failureCode === null || typeof value.failureCode === "string") || !integer(value.recordedAttemptCount)) invalidResponse();
  return value as unknown as LoadedRun;
}

export function solveStatusLabel(status: SolverStatus, terminationCode?: string): string {
  if (terminationCode === "CandidateBudgetExhausted") return "候选预算耗尽，尚未找到课表";
  return { Optimal: "已找到最优课表", Feasible: "已找到可行课表", ProvenInfeasible: "已证明当前约束不可行",
    Timeout: "达到时限，未找到课表", Unknown: "尚不能判断可行性", Cancelled: "已取消",
    InvalidInput: "输入未通过检查", InvalidModel: "求解模型无效", InternalError: "运行失败" }[status];
}
