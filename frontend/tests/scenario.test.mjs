import assert from "node:assert/strict";
import test from "node:test";
import { adoptScenarioRequest, canAdoptRun, copyScenarioRequest, loadSavedScenario,
  parseLoadedScenario, parseScenarioPage, parseScenarioReceipt } from "../src/scenarioApi.ts";

const id = "11111111-1111-4111-8111-111111111111";
const other = "22222222-2222-4222-8222-222222222222";
const receipt = { schemaVersion: 1, projectId: id, sourceProjectRevision: "9007199254740993", sourcePayloadHash: "a".repeat(64),
  scenarioId: other, scenarioRevision: "0", scenarioPayloadHash: "b".repeat(64), timetableId: id, timetableRevision: "9007199254740995",
  timetablePayloadHash: "c".repeat(64), originRunId: id, originArtifactHash: "d".repeat(64), createdAt: "2026-09-08T10:00:00.123Z" };
const loaded = { schemaVersion: 1, receipt, displayName: "正式方案", sourceIsCurrent: true, activityCount: 34,
  materializedSectionCount: 6, materializedEnrollmentCount: 72, quality: [{ id: "quality", priority: 1, value: "9007199254740993",
    metrics: [{ code: "teacher_gaps", rawValue: "4", weightWithinTier: 1, weightedValue: "4" }] }], clonedFrom: null };
const run = { failureCode: null, run: { projectId: id, runId: id, revision: receipt.sourceProjectRevision, status: "Feasible",
  sourcePayloadHash: receipt.sourcePayloadHash, artifactPayloadHash: receipt.originArtifactHash } };

test("scenario receipt and quality preserve exact revisions beyond JavaScript safe integers", () => {
  assert.deepEqual(parseScenarioReceipt(receipt), receipt);
  assert.deepEqual(parseLoadedScenario(loaded), loaded);
  for (const value of [{}, { ...receipt, scenarioRevision: 0 }, { ...receipt, sourceProjectRevision: "00" },
    { ...receipt, timetableRevision: "9223372036854775808" }, { ...receipt, sourcePayloadHash: "" }]) {
    assert.throws(() => parseScenarioReceipt(value), { code: "DESKTOP_SCENARIO_INVALID_RESPONSE" });
  }
  assert.throws(() => parseLoadedScenario({ ...loaded, clonedFrom: { scenarioId: other, scenarioRevision: "0", timetableId: id, timetableRevision: "0" } }));
});

test("adopt and copy requests use saved receipts and never transport assignments or storage paths", () => {
  const adopt = adoptScenarioRequest(run, "正式方案");
  assert.equal(adopt.expectedSourceRevision, receipt.sourceProjectRevision);
  const copy = copyScenarioRequest(loaded, "独立副本");
  assert.equal(copy.expectedScenarioRevision, "0");
  assert.equal(copy.expectedTimetableRevision, "9007199254740995");
  for (const value of [adopt, copy]) for (const key of ["scenarioId", "timetableId", "assignments", "databasePath", "workerPath"]) {
    assert.equal(key in value, false);
  }
});

test("adoption UI requires a successful current-source receipt and distinguishes cancelled historical runs", () => {
  const project = { projectId: id, revision: receipt.sourceProjectRevision, payloadHash: receipt.sourcePayloadHash };
  assert.equal(canAdoptRun(project, run), true);
  for (const status of ["Cancelled", "Timeout", "Unknown", "ProvenInfeasible"]) {
    assert.equal(canAdoptRun(project, { ...run, run: { ...run.run, status } }), false);
  }
  assert.equal(canAdoptRun({ ...project, revision: "0" }, run), false);
  assert.equal(canAdoptRun({ ...project, payloadHash: "e".repeat(64) }, run), false);
  assert.equal(canAdoptRun(project, { ...run, failureCode: "FAILED" }), false);
});

test("metadata pages keep previews separate and reject contradictory pagination or cross-project rows", () => {
  const entry = { scenarioId: other, openable: true, projectId: id, displayName: "方案", scenarioRevision: "0", timetableId: id,
    timetableRevision: "0", sourceProjectRevision: "0", createdAt: receipt.createdAt };
  const page = { schemaVersion: 1, projectId: id, scenarios: [entry], hasMore: false, nextOffset: null };
  assert.deepEqual(parseScenarioPage(page), page);
  assert.throws(() => parseScenarioPage({ ...page, hasMore: true }));
  assert.throws(() => parseScenarioPage({ ...page, scenarios: [{ ...entry, projectId: other }] }));
});

test("ordinary browser never invents a saved or revalidated scenario", async () => {
  await assert.rejects(loadSavedScenario(other), { code: "DESKTOP_RUNTIME_REQUIRED" });
});
