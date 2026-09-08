import assert from "node:assert/strict";
import test from "node:test";
import { loadSavedTimetable, parseSavedTimetablePage, parseTimetableEntityPage } from "../src/timetableApi.ts";

const id = "11111111-1111-4111-8111-111111111111";
const digest = "a".repeat(64);
const entity = { id, code: "fixture", label: "合成样本" };

function response() {
  return {
    schemaVersion: 1, runId: id, projectId: id, projectRevision: "9007199254740993",
    projectDisplayName: "课表传输测试", sourcePayloadHash: digest, artifactPayloadHash: digest,
    inputSnapshotHash: digest, outputHash: digest, adopted: false, selectedAttemptIndex: null, selection: entity,
    rows: [{ activityId: id, activityIndex: 0, courseOfferingId: id, meetingOrdinal: 1,
      startTimeslotIndex: 0, day: "monday", dayLabel: "星期一", periodIndex: 1, periodLabel: "第1节",
      durationPeriods: 1, occupiedTimeslotIndices: [0], grade: entity, subject: entity, coursePlan: entity,
      audience: { kind: "teaching_section", entity }, teacher: entity, room: entity, studentCount: 12 }],
    calendar: [{ timeslotIndex: 0, day: "monday", dayLabel: "星期一", periodIndex: 1, periodLabel: "第1节",
      instructionalBlock: 1, occupiedCount: 1, pageActivityIds: [id] },
    { timeslotIndex: 1, day: "monday", dayLabel: "星期一", periodIndex: 2, periodLabel: "第2节",
      instructionalBlock: 1, occupiedCount: 1, pageActivityIds: [] }],
    totalRows: 2, offset: 0, hasMore: true, nextOffset: 1,
    quality: [{ id: "distribution", priority: 1, value: "9223372036854775807", metrics: [{
      code: "QUALITY_COURSE_DISTRIBUTION", rawValue: "9223372036854775807", weightWithinTier: 1, weightedValue: "9223372036854775807",
    }] }],
  };
}

test("read-model DTO preserves decimal identities and cells occupied by other pages", () => {
  const page = parseSavedTimetablePage(response());
  assert.equal(page.projectRevision, "9007199254740993");
  assert.equal(page.quality[0].value, "9223372036854775807");
  assert.equal(page.calendar[1].occupiedCount, 1);
  assert.deepEqual(page.calendar[1].pageActivityIds, []);
});

test("invalid schema, unsafe integers, adoption and missing row references fail closed", () => {
  for (const mutate of [
    (value) => { value.schemaVersion = 2; },
    (value) => { value.totalRows = Number.MAX_SAFE_INTEGER + 1; },
    (value) => { value.adopted = true; },
    (value) => { value.calendar[0].pageActivityIds = ["22222222-2222-4222-8222-222222222222"]; },
    (value) => { value.rows[0].occupiedTimeslotIndices = [3]; },
    (value) => { value.rows[0].studentCount = 2.5; },
    (value) => { value.quality[0].value = 1; },
  ]) {
    const value = response(); mutate(value);
    assert.throws(() => parseSavedTimetablePage(value), { code: "DESKTOP_TIMETABLE_INVALID_RESPONSE" });
  }
  assert.throws(() => parseSavedTimetablePage({}), { code: "DESKTOP_TIMETABLE_INVALID_RESPONSE" });
});

test("entity pages require a known view and bounded concrete entity labels", () => {
  const page = { schemaVersion: 1, runId: id, view: "student", entities: [entity], totalEntities: 1, offset: 0, hasMore: false, nextOffset: null };
  assert.equal(parseTimetableEntityPage(page).entities[0].label, "合成样本");
  assert.throws(() => parseTimetableEntityPage({ ...page, view: "all" }), { code: "DESKTOP_TIMETABLE_INVALID_RESPONSE" });
  assert.throws(() => parseTimetableEntityPage({ ...page, entities: Array.from({ length: 101 }, () => entity) }), { code: "DESKTOP_TIMETABLE_INVALID_RESPONSE" });
});

test("browser preview does not invent a saved timetable", async () => {
  await assert.rejects(loadSavedTimetable(id, "student", id), { code: "DESKTOP_RUNTIME_REQUIRED" });
});
