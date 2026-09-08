import { invoke, isTauri } from "@tauri-apps/api/core";

export const TIMETABLE_VIEWS = [
  { value: "administrative_class", label: "行政班" },
  { value: "teaching_section", label: "教学班" },
  { value: "teacher", label: "教师" },
  { value: "room", label: "教室" },
  { value: "student", label: "学生" },
  { value: "subject", label: "学科" },
  { value: "grade", label: "全年级" },
] as const;

export type TimetableView = typeof TIMETABLE_VIEWS[number]["value"];
export type TimetableDay = "monday" | "tuesday" | "wednesday" | "thursday" | "friday" | "saturday" | "sunday";
export interface TimetableEntity { readonly id: string; readonly code: string; readonly label: string }
export interface TimetableEntityPage {
  readonly schemaVersion: 1; readonly runId: string; readonly view: TimetableView;
  readonly entities: readonly TimetableEntity[]; readonly totalEntities: number;
  readonly offset: number; readonly hasMore: boolean; readonly nextOffset: number | null;
}
export interface TimetableRow {
  readonly activityId: string; readonly activityIndex: number; readonly courseOfferingId: string;
  readonly meetingOrdinal: number; readonly startTimeslotIndex: number; readonly day: TimetableDay;
  readonly dayLabel: string; readonly periodIndex: number; readonly periodLabel: string;
  readonly durationPeriods: number; readonly occupiedTimeslotIndices: readonly number[];
  readonly grade: TimetableEntity; readonly subject: TimetableEntity; readonly coursePlan: TimetableEntity;
  readonly audience: { readonly kind: "administrative_class" | "teaching_section"; readonly entity: TimetableEntity };
  readonly teacher: TimetableEntity; readonly room: TimetableEntity; readonly studentCount: number;
}
export interface TimetableGridCell {
  readonly timeslotIndex: number; readonly day: TimetableDay; readonly dayLabel: string;
  readonly periodIndex: number; readonly periodLabel: string; readonly instructionalBlock: number;
  readonly occupiedCount: number; readonly pageActivityIds: readonly string[];
}
export interface TimetableMetric {
  readonly code: string; readonly rawValue: string; readonly weightWithinTier: number; readonly weightedValue: string;
}
export interface TimetableQualityTier {
  readonly id: string; readonly priority: number; readonly value: string; readonly metrics: readonly TimetableMetric[];
}
export interface SavedTimetablePage {
  readonly schemaVersion: 1; readonly runId: string; readonly projectId: string; readonly projectRevision: string;
  readonly projectDisplayName: string; readonly sourcePayloadHash: string; readonly artifactPayloadHash: string;
  readonly inputSnapshotHash: string; readonly outputHash: string; readonly adopted: false;
  readonly selectedAttemptIndex: number | null; readonly selection: TimetableEntity;
  readonly rows: readonly TimetableRow[]; readonly calendar: readonly TimetableGridCell[];
  readonly totalRows: number; readonly offset: number; readonly hasMore: boolean; readonly nextOffset: number | null;
  readonly quality: readonly TimetableQualityTier[];
}

const days = new Set(["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"]);
const record = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);
const text = (value: unknown): value is string => typeof value === "string" && value.length > 0;
const integer = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && value <= 0xffff_ffff;
const positive = (value: unknown): value is number => integer(value) && value > 0;
const uuid = (value: unknown): value is string => typeof value === "string" && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/u.test(value);
const hash = (value: unknown): value is string => typeof value === "string" && /^[0-9a-f]{64}$/u.test(value);
const decimal = (value: unknown): value is string => typeof value === "string" && /^-?(?:0|[1-9][0-9]*)$/u.test(value);
const entity = (value: unknown): value is TimetableEntity => record(value) && uuid(value.id) && text(value.code) && text(value.label);
const day = (value: unknown): value is TimetableDay => typeof value === "string" && days.has(value);
const view = (value: unknown): value is TimetableView => TIMETABLE_VIEWS.some((item) => item.value === value);
const page = (value: Record<string, unknown>): boolean => integer(value.offset) && typeof value.hasMore === "boolean" &&
  (value.hasMore ? integer(value.nextOffset) && value.nextOffset > value.offset : value.nextOffset === null);

function row(value: unknown): value is TimetableRow {
  return record(value) && uuid(value.activityId) && integer(value.activityIndex) && uuid(value.courseOfferingId) &&
    positive(value.meetingOrdinal) && integer(value.startTimeslotIndex) && day(value.day) && text(value.dayLabel) &&
    positive(value.periodIndex) && text(value.periodLabel) && positive(value.durationPeriods) && value.durationPeriods <= 255 &&
    Array.isArray(value.occupiedTimeslotIndices) && value.occupiedTimeslotIndices.every(integer) &&
    value.occupiedTimeslotIndices.length === value.durationPeriods &&
    entity(value.grade) && entity(value.subject) && entity(value.coursePlan) && entity(value.teacher) && entity(value.room) &&
    record(value.audience) && (value.audience.kind === "administrative_class" || value.audience.kind === "teaching_section") &&
    entity(value.audience.entity) && integer(value.studentCount);
}

function cell(value: unknown): value is TimetableGridCell {
  return record(value) && integer(value.timeslotIndex) && day(value.day) && text(value.dayLabel) &&
    positive(value.periodIndex) && text(value.periodLabel) && positive(value.instructionalBlock) && integer(value.occupiedCount) &&
    Array.isArray(value.pageActivityIds) && value.pageActivityIds.every(uuid) && value.pageActivityIds.length <= value.occupiedCount;
}

function tier(value: unknown): value is TimetableQualityTier {
  return record(value) && text(value.id) && positive(value.priority) && decimal(value.value) && Array.isArray(value.metrics) &&
    value.metrics.every((metric: unknown) => record(metric) && text(metric.code) && decimal(metric.rawValue) &&
      positive(metric.weightWithinTier) && decimal(metric.weightedValue));
}

function invalidResponse(): never {
  throw { schemaVersion: 1, code: "DESKTOP_TIMETABLE_INVALID_RESPONSE", message: "课表数据格式或运行身份不一致，请重新打开已保存运行。", details: null };
}

export function parseTimetableEntityPage(value: unknown): TimetableEntityPage {
  if (!record(value) || value.schemaVersion !== 1 || !uuid(value.runId) || !view(value.view) || !page(value) ||
      !Array.isArray(value.entities) || value.entities.length > 100 || !value.entities.every(entity) || !integer(value.totalEntities)) {
    return invalidResponse();
  }
  return value as unknown as TimetableEntityPage;
}

export function parseSavedTimetablePage(value: unknown): SavedTimetablePage {
  if (!record(value) || value.schemaVersion !== 1 || !uuid(value.runId) || !uuid(value.projectId) ||
      !decimal(value.projectRevision) || value.projectRevision.startsWith("-") || !text(value.projectDisplayName) ||
      !hash(value.sourcePayloadHash) || !hash(value.artifactPayloadHash) || !hash(value.inputSnapshotHash) || !hash(value.outputHash) ||
      value.adopted !== false || !(value.selectedAttemptIndex === null || integer(value.selectedAttemptIndex) && value.selectedAttemptIndex < 16) ||
      !entity(value.selection) || !page(value) || !integer(value.totalRows) || !Array.isArray(value.rows) || value.rows.length > 100 ||
      !value.rows.every(row) || !Array.isArray(value.calendar) || value.calendar.length === 0 || value.calendar.length > 4096 ||
      !value.calendar.every(cell) || !Array.isArray(value.quality) || !value.quality.every(tier)) {
    return invalidResponse();
  }
  const rows = value.rows as TimetableRow[];
  const calendar = value.calendar as TimetableGridCell[];
  const rowIds = new Set(rows.map((item) => item.activityId));
  const slotIds = new Set(calendar.map((item) => item.timeslotIndex));
  if (rowIds.size !== rows.length || slotIds.size !== calendar.length ||
      calendar.some((item) => item.pageActivityIds.some((id) => !rowIds.has(id))) ||
      rows.some((item) => item.occupiedTimeslotIndices.some((index) => !slotIds.has(index)))) {
    return invalidResponse();
  }
  return value as unknown as SavedTimetablePage;
}

async function native(command: string, request: Record<string, unknown>): Promise<unknown> {
  if (!isTauri()) {
    throw { schemaVersion: 1, code: "DESKTOP_RUNTIME_REQUIRED", message: "请在本机桌面应用中打开已保存运行以查看课表。", details: null };
  }
  return invoke<unknown>(command, { request });
}

export async function loadTimetableEntities(runId: string, selectedView: TimetableView, offset = 0, limit = 20): Promise<TimetableEntityPage> {
  const response = parseTimetableEntityPage(await native("query_saved_timetable_entities", {
    schemaVersion: 1, runId, view: selectedView, offset, limit,
  }));
  if (response.runId !== runId || response.view !== selectedView || response.offset !== offset || response.entities.length > limit) return invalidResponse();
  return response;
}

export async function loadSavedTimetable(runId: string, selectedView: TimetableView, entityId: string, offset = 0, limit = 100): Promise<SavedTimetablePage> {
  const response = parseSavedTimetablePage(await native("query_saved_timetable", {
    schemaVersion: 1, runId, view: selectedView, entityId, offset, limit,
  }));
  if (response.runId !== runId || response.selection.id !== entityId || response.offset !== offset || response.rows.length > limit) return invalidResponse();
  return response;
}
