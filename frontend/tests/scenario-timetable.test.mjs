import assert from "node:assert/strict";
import test from "node:test";
import { TIMETABLE_VIEWS, parseSavedTimetablePage, parseTimetableEntityPage } from "../src/timetableApi.ts";
import { checkedScenarioTimetableEntities, checkedScenarioTimetablePage, loadScenarioTimetable,
  loadScenarioTimetableEntities, parseScenarioTimetablePage, parseScenarioTimetableEntityPage,
  scenarioTimetableKey, scenarioTimetableRequest } from "../src/scenarioTimetableApi.ts";

const id = "11111111-1111-4111-8111-111111111111";
const other = "22222222-2222-4222-8222-222222222222";
const entity = { id, code: "synthetic", label: "合成班级" };
const receipt = { schemaVersion: 1, projectId: id, sourceProjectRevision: "9007199254740993", sourcePayloadHash: "a".repeat(64),
  scenarioId: id, scenarioRevision: "9007199254740995", scenarioPayloadHash: "b".repeat(64),
  timetableId: other, timetableRevision: "9223372036854775807", timetablePayloadHash: "c".repeat(64),
  originRunId: other, originArtifactHash: "d".repeat(64), createdAt: "2026-09-08T12:00:00.000Z" };

function response() {
  return { schemaVersion: 1, receipt: { ...receipt }, sourceIsCurrent: false, scenarioDisplayName: "正式方案",
    selection: entity, rows: [{ activityId: id, activityIndex: 0, courseOfferingId: id, meetingOrdinal: 1,
      startTimeslotIndex: 0, day: "monday", dayLabel: "星期一", periodIndex: 1, periodLabel: "第1节",
      durationPeriods: 2, occupiedTimeslotIndices: [0, 1], grade: entity, subject: entity, coursePlan: entity,
      audience: { kind: "teaching_section", entity }, teacher: entity, room: entity, studentCount: 12 }],
    calendar: [0, 1].map((index) => ({ timeslotIndex: index, day: "monday", dayLabel: "星期一",
      periodIndex: index + 1, periodLabel: `第${index + 1}节`, instructionalBlock: 1,
      occupiedCount: 2, pageActivityIds: [id] })),
    totalRows: 2, offset: 0, hasMore: true, nextOffset: 1,
    quality: [{ id: "distribution", priority: 1, value: "9007199254740993", metrics: [
      { code: "QUALITY_COURSE_DISTRIBUTION", rawValue: "9007199254740993", weightWithinTier: 1, weightedValue: "9007199254740993" },
    ] }],
  };
}

function entities(view = "administrative_class") {
  return { schemaVersion: 1, receipt: { ...receipt }, sourceIsCurrent: true, scenarioDisplayName: "正式方案",
    view, entities: [entity], totalEntities: 1, offset: 0, hasMore: false, nextOffset: null };
}

test("all seven scenario views submit only real scenario identity and exact dual revisions", () => {
  assert.equal(TIMETABLE_VIEWS.length, 7);
  for (const { value: view } of TIMETABLE_VIEWS) {
    const request = scenarioTimetableRequest(receipt, view, 0, 20);
    assert.deepEqual(request, { schemaVersion: 1, scenarioId: id, expectedScenarioRevision: "9007199254740995",
      expectedTimetableRevision: "9223372036854775807", view, offset: 0, limit: 20 });
    assert.equal(checkedScenarioTimetableEntities(entities(view), receipt, view, 0, 20).view, view);
    const rowRequest = scenarioTimetableRequest(receipt, view, 0, 1, id);
    assert.equal(rowRequest.entityId, id);
    for (const key of ["runId", "originRunId", "databasePath", "workerPath", "assignments"]) assert.equal(key in rowRequest, false);
  }
  assert.equal(scenarioTimetableRequest({ ...receipt, scenarioRevision: "0", timetableRevision: "0" }, "student", 0, 20).expectedScenarioRevision, "0");
});

test("scenario rows preserve history, full duration occupancy and precision without a run disguise", () => {
  const page = checkedScenarioTimetablePage(response(), receipt, id, 0, 1);
  assert.equal(page.sourceIsCurrent, false);
  assert.equal(page.receipt.sourceProjectRevision, "9007199254740993");
  assert.equal(page.receipt.timetableRevision, "9223372036854775807");
  assert.equal(page.quality[0].value, "9007199254740993");
  assert.deepEqual(page.rows[0].occupiedTimeslotIndices, [0, 1]);
  assert.equal(page.calendar[1].occupiedCount - page.calendar[1].pageActivityIds.length, 1);
  for (const field of ["runId", "adopted", "inputSnapshotHash", "outputHash"]) assert.equal(field in page, false);
});

test("response receipt identity, both revisions and each immutable provenance digest must match", () => {
  for (const [field, value] of [
    ["projectId", other], ["sourceProjectRevision", "0"], ["sourcePayloadHash", "e".repeat(64)],
    ["scenarioId", other], ["scenarioRevision", "9007199254740996"], ["scenarioPayloadHash", "e".repeat(64)],
    ["timetableId", id], ["timetableRevision", "0"], ["timetablePayloadHash", "e".repeat(64)],
    ["originRunId", id], ["originArtifactHash", "e".repeat(64)], ["createdAt", "2026-09-08T12:00:01.000Z"],
  ]) {
    const rows = response(); rows.receipt[field] = value;
    const options = entities(); options.receipt[field] = value;
    assert.throws(() => checkedScenarioTimetablePage(rows, receipt, id, 0, 1), { code: "DESKTOP_SCENARIO_TIMETABLE_INVALID_RESPONSE" });
    assert.throws(() => checkedScenarioTimetableEntities(options, receipt, "administrative_class", 0, 20), { code: "DESKTOP_SCENARIO_TIMETABLE_INVALID_RESPONSE" });
    assert.notEqual(scenarioTimetableKey(rows.receipt), scenarioTimetableKey(receipt));
  }
  assert.equal(scenarioTimetableKey({ ...receipt }), scenarioTimetableKey(receipt));
});

test("invalid scenario envelopes and contradictory row-grid data fail closed", () => {
  for (const mutate of [
    (value) => { value.schemaVersion = 2; },
    (value) => { value.receipt.timetableRevision = 0; },
    (value) => { value.sourceIsCurrent = "false"; },
    (value) => { value.scenarioDisplayName = ""; },
    (value) => { value.runId = id; },
    (value) => { value.adopted = false; },
    (value) => { value.rows[0].occupiedTimeslotIndices = [0, 0]; },
    (value) => { value.calendar[1].pageActivityIds = []; },
    (value) => { value.calendar[0].pageActivityIds = [other]; },
    (value) => { value.quality[0].value = 4; },
    (value) => { value.totalRows = 0; },
  ]) {
    const value = response(); mutate(value);
    assert.throws(() => parseScenarioTimetablePage(value), { code: "DESKTOP_SCENARIO_TIMETABLE_INVALID_RESPONSE" });
  }
  assert.throws(() => parseScenarioTimetablePage({}), { code: "DESKTOP_SCENARIO_TIMETABLE_INVALID_RESPONSE" });
});

test("scenario responses match requested view entity and bounded page", () => {
  for (const value of [entities("teacher"), { ...entities(), offset: 1 },
    { ...entities(), hasMore: true, nextOffset: 19 }, { ...entities(), entities: [entity, entity] }]) {
    assert.throws(() => checkedScenarioTimetableEntities(value, receipt, "administrative_class", 0, 20));
  }
  for (const value of [{ ...response(), selection: { ...entity, id: other } }, { ...response(), offset: 1 },
    { ...response(), nextOffset: 2 }]) {
    assert.throws(() => checkedScenarioTimetablePage(value, receipt, id, 0, 1));
  }
  for (const [view, offset, limit, entityId] of [["all", 0, 20], ["student", 0, 0], ["teacher", 0xffff_ffff, 1], ["room", 0, 101], ["grade", 0, 20, ""]]) {
    assert.throws(() => scenarioTimetableRequest(receipt, view, offset, limit, entityId), { code: "DESKTOP_SCENARIO_TIMETABLE_INVALID_REQUEST" });
  }
});

test("original run parsers retain real run provenance and reject scenario envelopes", () => {
  const { receipt: _receipt, sourceIsCurrent: _current, scenarioDisplayName: _name, ...content } = response();
  const run = { ...content, runId: other, projectId: id, projectRevision: "9007199254740993", projectDisplayName: "来源项目",
    sourcePayloadHash: "a".repeat(64), artifactPayloadHash: "d".repeat(64), inputSnapshotHash: "e".repeat(64),
    outputHash: "f".repeat(64), adopted: false, selectedAttemptIndex: null };
  assert.equal(parseSavedTimetablePage(run).runId, other);
  assert.equal(parseSavedTimetablePage(run).outputHash, "f".repeat(64));
  assert.throws(() => parseSavedTimetablePage(response()), { code: "DESKTOP_TIMETABLE_INVALID_RESPONSE" });
  assert.throws(() => parseSavedTimetablePage({ ...run, receipt }), { code: "DESKTOP_TIMETABLE_INVALID_RESPONSE" });
  assert.throws(() => parseTimetableEntityPage({ ...entities(), runId: other }), { code: "DESKTOP_TIMETABLE_INVALID_RESPONSE" });
  assert.throws(() => parseScenarioTimetablePage(run), { code: "DESKTOP_SCENARIO_TIMETABLE_INVALID_RESPONSE" });
  assert.throws(() => parseScenarioTimetableEntityPage({ ...entities(), runId: other }), { code: "DESKTOP_SCENARIO_TIMETABLE_INVALID_RESPONSE" });
});

test("ordinary browser never substitutes an origin-run query for a scenario timetable", async () => {
  await assert.rejects(loadScenarioTimetableEntities(receipt, "student"), { code: "DESKTOP_RUNTIME_REQUIRED" });
  await assert.rejects(loadScenarioTimetable(receipt, "student", id), { code: "DESKTOP_RUNTIME_REQUIRED" });
});
