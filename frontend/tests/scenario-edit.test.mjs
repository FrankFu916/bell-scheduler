import assert from "node:assert/strict";
import test from "node:test";
import { checkedScenarioEditCommit, checkedScenarioEditPreview, commitScenarioEdit, previewScenarioEdit,
  scenarioEditCommitRequest, scenarioEditKey, scenarioEditRequest } from "../src/scenarioEditApi.ts";
import { createScenarioEditGuard, sameEditRevision, selectScenarioActivity } from "../src/scenarioEditState.ts";
import { scenarioTimetableKey } from "../src/scenarioTimetableApi.ts";

const id = (index) => `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`;
const receipt = { schemaVersion: 1, projectId: id(1), sourceProjectRevision: "9007199254740993", sourcePayloadHash: "a".repeat(64),
  scenarioId: id(2), scenarioRevision: "9007199254740995", scenarioPayloadHash: "b".repeat(64),
  timetableId: id(3), timetableRevision: "9007199254740995", timetablePayloadHash: "c".repeat(64),
  originRunId: id(4), originArtifactHash: "d".repeat(64), createdAt: "2026-09-12T12:00:00.000Z" };
const move = { kind: "move", activityId: id(5), startTimeslotId: id(12), lockAfter: false };
const entity = { id: id(7), code: "synthetic", label: "合成教学班" };
const calendar = [1, 2, 3].map((period) => ({ timeslotId: id(10 + period), day: "monday", dayLabel: "星期一", periodIndex: period, periodLabel: `第${period}节` }));
const quality = [{ id: "distribution", priority: 1, value: "9007199254740993", metrics: [
  { code: "QUALITY_COURSE_DISTRIBUTION", rawValue: "9007199254740993", weightWithinTier: 1, weightedValue: "9007199254740993" },
] }];
const assignment = (slot = id(11)) => ({ startTimeslotId: slot, durationPeriods: 1, roomId: id(7), teacherId: id(7) });
const row = (activityId = id(5), period = 1) => ({ activityId, activityIndex: period - 1, courseOfferingId: id(8), meetingOrdinal: 1,
  startTimeslotIndex: period - 1, day: "monday", dayLabel: "星期一", periodIndex: period, periodLabel: `第${period}节`,
  durationPeriods: 1, occupiedTimeslotIndices: [period - 1], grade: entity, subject: entity, coursePlan: entity,
  audience: { kind: "teaching_section", entity }, teacher: entity, room: entity, studentCount: 12 });

function preview() {
  return { schemaVersion: 1, receipt: structuredClone(receipt), sourceIsCurrent: true, status: "valid", canCommit: true,
    changes: [{ activityId: id(5), before: assignment(), after: assignment(id(12)), wasUserLocked: false, isUserLocked: false }],
    validation: { passed: true, hardProblems: [], totalHardProblems: 0, hardProblemsTruncated: false },
    beforeQuality: structuredClone(quality), afterQuality: structuredClone(quality),
    context: { activities: [row()], calendar: structuredClone(calendar), diagnostics: [], totalDiagnostics: 0,
      diagnosticsTruncated: false, activitiesTruncated: false } };
}

function committed() {
  return { schemaVersion: 1, status: "committed", receipt: { ...receipt, scenarioRevision: "9007199254740996",
    timetableRevision: "9007199254740996", scenarioPayloadHash: "e".repeat(64), timetablePayloadHash: "f".repeat(64) } };
}

test("edit requests use exact dual revisions and only stable operation IDs; commit freezes both preview hashes", () => {
  for (const operation of [move, { kind: "swap_starts", leftActivityId: id(5), rightActivityId: id(6) },
    { kind: "lock_current", activityId: id(5) }, { kind: "unlock", activityId: id(5) }]) {
    assert.deepEqual(scenarioEditRequest(receipt, operation), { schemaVersion: 1, scenarioId: id(2),
      expectedScenarioRevision: "9007199254740995", expectedTimetableRevision: "9007199254740995", operation });
  }
  const request = scenarioEditCommitRequest(preview(), move);
  assert.equal(request.expectedScenarioPayloadHash, receipt.scenarioPayloadHash);
  assert.equal(request.expectedTimetablePayloadHash, receipt.timetablePayloadHash);
  for (const key of ["assignments", "rows", "quality", "validation", "databasePath", "workerPath", "projectId"]) assert.equal(key in request, false);
  for (const operation of [{ ...move, startTimeslotId: 2 }, { ...move, lockAfter: "false" }, { ...move, assignments: [] },
    { kind: "unlock", activityId: "PHY1" }, { kind: "improve", activityId: id(5) }]) {
    assert.throws(() => scenarioEditRequest(receipt, operation), { code: "DESKTOP_SCENARIO_EDIT_INVALID_REQUEST" });
  }
});

test("a valid preview cannot authorize another target, lock choice, activity or operation", () => {
  const value = checkedScenarioEditPreview(preview(), receipt, move);
  for (const other of [{ ...move, startTimeslotId: id(13) }, { ...move, activityId: id(6) }, { ...move, lockAfter: true },
    { kind: "lock_current", activityId: id(5) }, { kind: "unlock", activityId: id(5) }]) {
    assert.throws(() => scenarioEditCommitRequest(value, other), { code: "DESKTOP_SCENARIO_EDIT_INVALID_RESPONSE" });
    assert.notEqual(scenarioEditKey(receipt, move), scenarioEditKey(receipt, other));
  }
  const wrongRoom = preview(); wrongRoom.changes[0].after.roomId = id(99);
  assert.throws(() => checkedScenarioEditPreview(wrongRoom, receipt, move), { code: "DESKTOP_SCENARIO_EDIT_INVALID_RESPONSE" });
});

test("swap response must exchange both actual starts and preserve both lock states", () => {
  const operation = { kind: "swap_starts", leftActivityId: id(5), rightActivityId: id(6) };
  const value = preview();
  value.context.activities.push(row(id(6), 2));
  value.changes.push({ activityId: id(6), before: assignment(id(12)), after: assignment(id(11)), wasUserLocked: false, isUserLocked: false });
  assert.equal(checkedScenarioEditPreview(value, receipt, operation).changes.length, 2);
  for (const mutate of [(copy) => { copy.changes.pop(); }, (copy) => { copy.changes[1].after.startTimeslotId = id(13); },
    (copy) => { copy.changes[1].isUserLocked = true; }]) {
    const changed = structuredClone(value); mutate(changed);
    assert.throws(() => checkedScenarioEditPreview(changed, receipt, operation), { code: "DESKTOP_SCENARIO_EDIT_INVALID_RESPONSE" });
  }
});

test("lock and unlock bind direction without moving a course", () => {
  for (const kind of ["lock_current", "unlock"]) {
    const operation = { kind, activityId: id(5) };
    const value = preview(); value.changes[0].after = assignment();
    value.changes[0].wasUserLocked = kind === "unlock"; value.changes[0].isUserLocked = kind === "lock_current";
    assert.equal(checkedScenarioEditPreview(value, receipt, operation).canCommit, true);
    assert.throws(() => scenarioEditCommitRequest(value, { kind: kind === "unlock" ? "lock_current" : "unlock", activityId: id(5) }),
      { code: "DESKTOP_SCENARIO_EDIT_INVALID_RESPONSE" });
    value.changes[0].after.startTimeslotId = id(12);
    assert.throws(() => checkedScenarioEditPreview(value, receipt, operation), { code: "DESKTOP_SCENARIO_EDIT_INVALID_RESPONSE" });
  }
});

test("Hard rejection retains cross-page stable descriptions and cannot carry a successful score or commit", () => {
  const value = preview(); value.status = "hard_rejected"; value.canCommit = false; value.afterQuality = null;
  value.context.activities.push(row(id(6), 2));
  value.context.diagnostics = [{ code: "VALIDATION_STUDENT_CONFLICT", message: "学生课程时间冲突", activityIds: [id(5), id(6)], activitiesTruncated: false }];
  value.context.totalDiagnostics = 1;
  value.validation = { passed: false, totalHardProblems: 1, hardProblemsTruncated: false, hardProblems: [{ code: "VALIDATION_STUDENT_CONFLICT",
    message: "学生课程时间冲突", activityIndices: [0, 1], entityIndices: { student: 0 }, parameters: {} }] };
  const parsed = checkedScenarioEditPreview(value, receipt, move);
  assert.equal(parsed.context.activities[1].activityId, id(6));
  assert.deepEqual(parsed.context.diagnostics[0].activityIds, [id(5), id(6)]);
  assert.throws(() => scenarioEditCommitRequest(parsed, move), { code: "DESKTOP_SCENARIO_EDIT_PREVIEW_REQUIRED" });
  for (const mutate of [(copy) => { copy.canCommit = true; }, (copy) => { copy.afterQuality = quality; },
    (copy) => { copy.validation.passed = true; }, (copy) => { copy.context.activities.pop(); },
    (copy) => { copy.context.diagnostics[0].code = "OTHER"; }, (copy) => { copy.context.totalDiagnostics = 2; }]) {
    const changed = structuredClone(value); mutate(changed);
    assert.throws(() => checkedScenarioEditPreview(changed, receipt, move), { code: "DESKTOP_SCENARIO_EDIT_INVALID_RESPONSE" });
  }
});

test("historical and unchanged previews cannot commit, and scores remain exact decimal strings", () => {
  const historical = preview(); historical.sourceIsCurrent = false; historical.canCommit = false;
  assert.equal(checkedScenarioEditPreview(historical, receipt, move).afterQuality[0].value, "9007199254740993");
  assert.throws(() => scenarioEditCommitRequest(historical, move), { code: "DESKTOP_SCENARIO_EDIT_PREVIEW_REQUIRED" });
  const unchanged = preview(); unchanged.status = "no_change"; unchanged.changes = []; unchanged.context.activities = []; unchanged.canCommit = false;
  assert.equal(checkedScenarioEditPreview(unchanged, receipt, move).status, "no_change");
  assert.throws(() => scenarioEditCommitRequest(unchanged, move), { code: "DESKTOP_SCENARIO_EDIT_PREVIEW_REQUIRED" });
  historical.canCommit = true;
  assert.throws(() => checkedScenarioEditPreview(historical, receipt, move), { code: "DESKTOP_SCENARIO_EDIT_INVALID_RESPONSE" });
  const numericScore = preview(); numericScore.afterQuality[0].value = 9007199254740992;
  assert.throws(() => checkedScenarioEditPreview(numericScore, receipt, move), { code: "DESKTOP_SCENARIO_EDIT_INVALID_RESPONSE" });
});

test("all immutable preview receipt fields must match the frozen scenario", () => {
  for (const [field, next] of [["projectId", id(99)], ["scenarioId", id(99)], ["timetableId", id(99)], ["originRunId", id(99)],
    ["sourceProjectRevision", "0"], ["scenarioRevision", "0"], ["timetableRevision", "0"], ["sourcePayloadHash", "0".repeat(64)],
    ["scenarioPayloadHash", "0".repeat(64)], ["timetablePayloadHash", "0".repeat(64)], ["originArtifactHash", "0".repeat(64)],
    ["createdAt", "2026-09-12T12:00:01.000Z"]]) {
    const changed = preview(); changed.receipt[field] = next;
    assert.throws(() => checkedScenarioEditPreview(changed, receipt, move), { code: "DESKTOP_SCENARIO_EDIT_INVALID_RESPONSE" });
  }
});

test("commit acknowledges exactly one revision pair or a fully identical NoChange, never a fabricated increment", () => {
  assert.deepEqual(checkedScenarioEditCommit(committed(), receipt), committed());
  assert.deepEqual(checkedScenarioEditCommit({ schemaVersion: 1, status: "no_change", receipt }, receipt).receipt, receipt);
  for (const changed of [ { ...committed(), status: "failed" }, { ...committed(), error: "failed" },
    ...[{ scenarioRevision: "9007199254740997" }, { timetableRevision: "9007199254740995" }, { scenarioRevision: 9007199254740996 },
      { sourceProjectRevision: "0" }, { scenarioPayloadHash: receipt.scenarioPayloadHash }, { timetablePayloadHash: receipt.timetablePayloadHash },
      { timetableId: id(99) }].map((fields) => ({ ...committed(), receipt: { ...committed().receipt, ...fields } })) ]) {
    assert.throws(() => checkedScenarioEditCommit(changed, receipt));
  }
  const acknowledged = checkedScenarioEditCommit(committed(), receipt);
  assert.notEqual(scenarioTimetableKey(acknowledged.receipt), scenarioTimetableKey(receipt), "old pages and exports belong to a different exact receipt");
});

test("cross-view selections retain original rows and locks, but cannot cross scenario revisions", () => {
  const page = { receipt, rows: [row()], activityLocks: [{ activityId: id(5), sourceLocked: false, userLocked: true }], calendar };
  const first = selectScenarioActivity(page, id(5));
  const second = selectScenarioActivity({ ...page, rows: [row(id(6), 2)], activityLocks: [{ activityId: id(6), sourceLocked: true, userLocked: false }] }, id(6));
  assert.equal(sameEditRevision(first, second), true);
  assert.equal(first.row.activityId, id(5)); assert.equal(first.lock.userLocked, true);
  assert.equal(second.lock.sourceLocked, true);
  assert.equal(sameEditRevision(first, { ...second, receipt: committed().receipt }), false);
  assert.throws(() => selectScenarioActivity(page, id(6)), { code: "DESKTOP_SCENARIO_EDIT_SELECTION_MISSING" });
});

test("changing target invalidates old preview completion and finally without clearing its successor", async () => {
  const guard = createScenarioEditGuard(); const key = scenarioEditKey(receipt, move);
  const old = guard.begin(key); assert.notEqual(old, null); assert.equal(guard.begin(key), null);
  let release; const delayed = new Promise((resolve) => { release = resolve; }); const messages = [];
  const completion = delayed.then(() => { if (guard.accepts(old, key)) messages.push("old preview"); })
    .finally(() => { if (guard.accepts(old, key)) guard.finish(old); });
  guard.invalidate(); const nextKey = scenarioEditKey(receipt, { ...move, startTimeslotId: id(13) }); const next = guard.begin(nextKey);
  release(); await completion;
  assert.deepEqual(messages, []); assert.equal(guard.accepts(next, nextKey), true);
  assert.equal(guard.accepts(next, key), false);
  guard.invalidate(); assert.equal(guard.accepts(next, nextKey), false, "unmounted selections reject late responses");
});

test("source replacement during post-save reload releases the old owner and old finally cannot clear a new operation", async () => {
  const guard = createScenarioEditGuard(); const oldKey = JSON.stringify([receipt.projectId, "0", "old"]);
  const old = guard.begin(oldKey); let busy = true;
  const saved = checkedScenarioEditCommit(committed(), receipt);
  let reject; const reload = new Promise((_, rejectPromise) => { reject = rejectPromise; }); const failures = [];
  const completion = reload.catch((error) => { if (guard.accepts(old, oldKey)) failures.push(error.message); })
    .finally(() => { if (guard.accepts(old, oldKey)) { guard.finish(old); busy = false; } });
  // ScenarioPanel's source-change effect invalidates its owner and resets loading state together.
  guard.invalidate(); busy = false;
  const nextKey = JSON.stringify([receipt.projectId, "1", "new"]); const next = guard.begin(nextKey); busy = true;
  reject(new Error("old reload failed")); await completion;
  assert.deepEqual(failures, []); assert.equal(guard.accepts(next, nextKey), true); assert.equal(busy, true);
  assert.equal(saved.status, "committed"); assert.equal(saved.receipt.scenarioRevision, "9007199254740996", "acknowledged persistence is not changed by reload failure");
  if (guard.accepts(next, nextKey)) { guard.finish(next); busy = false; }
  assert.equal(busy, false); assert.notEqual(guard.begin(nextKey), null);
});

test("a normal browser cannot pretend to preview or save a manual timetable change", async () => {
  await assert.rejects(previewScenarioEdit(receipt, move), { code: "DESKTOP_RUNTIME_REQUIRED" });
  await assert.rejects(commitScenarioEdit(preview(), move), { code: "DESKTOP_RUNTIME_REQUIRED" });
});
