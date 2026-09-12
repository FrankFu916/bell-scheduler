import assert from "node:assert/strict";
import test from "node:test";
import { commandError } from "../src/api.ts";
import { TIMETABLE_VIEWS } from "../src/timetableApi.ts";
import { checkedScenarioExportResult, createScenarioExportGuard, exportScenarioTimetable,
  scenarioExportContextKey, scenarioExportRequest } from "../src/scenarioTimetableExportApi.ts";

const id = "11111111-1111-4111-8111-111111111111";
const other = "22222222-2222-4222-8222-222222222222";
const receipt = { schemaVersion: 1, projectId: id, sourceProjectRevision: "9007199254740993", sourcePayloadHash: "a".repeat(64),
  scenarioId: id, scenarioRevision: "9007199254740995", scenarioPayloadHash: "b".repeat(64),
  timetableId: other, timetableRevision: "9223372036854775807", timetablePayloadHash: "c".repeat(64),
  originRunId: other, originArtifactHash: "d".repeat(64), createdAt: "2026-09-12T12:00:00.000Z" };
const selection = { id, code: "G12", label: "合成年级" };
const context = { receipt, view: "grade", selection, totalRows: 134 };

function saved(format = "xlsx") {
  return { schemaVersion: 1, outcome: "saved", receipt: { ...receipt }, sourceIsCurrent: false,
    scenarioDisplayName: "历史方案", view: context.view, selection: { ...selection }, format,
    exportedActivityCount: 134, fileName: `历史方案-年级.${format}`, generatedAt: "2026-09-12T12:01:00.000Z",
    byteLength: "9007199254740993", payloadHash: "e".repeat(64), payloadHashAlgorithm: "blake3" };
}

test("export requests carry exact dual revisions and an explicit whole-object selection for seven views", () => {
  for (const { value: view } of TIMETABLE_VIEWS) {
    for (const format of ["xlsx", "csv"]) {
      assert.deepEqual(scenarioExportRequest({ ...context, view }, format), {
        schemaVersion: 1, scenarioId: id, expectedScenarioRevision: "9007199254740995",
        expectedTimetableRevision: "9223372036854775807", view, entityId: id, format,
      });
    }
  }
  const zero = scenarioExportRequest({ ...context, receipt: { ...receipt, scenarioRevision: "0", timetableRevision: "0" } }, "csv");
  assert.equal(zero.expectedScenarioRevision, "0");
  for (const field of ["rows", "offset", "limit", "totalRows", "path", "fileName", "runId", "databasePath", "workerPath"]) assert.equal(field in zero, false);
  for (const [input, format] of [[{ ...context, view: "all" }, "xlsx"], [context, "pdf"],
    [{ ...context, selection: { ...selection, id: "G12" } }, "csv"], [{ ...context, totalRows: -1 }, "xlsx"]]) {
    assert.throws(() => scenarioExportRequest(input, format), { code: "DESKTOP_SCENARIO_EXPORT_INVALID_REQUEST" });
  }
});

test("saved export receipts require the complete row count and preserve historical provenance and integers", () => {
  for (const format of ["xlsx", "csv"]) {
    const value = checkedScenarioExportResult(saved(format), context, format);
    assert.equal(value.outcome, "saved");
    assert.equal(value.exportedActivityCount, 134);
    assert.equal(value.sourceIsCurrent, false);
    assert.equal(value.receipt.timetableRevision, "9223372036854775807");
    assert.equal(value.byteLength, "9007199254740993");
    assert.equal(value.fileName, `历史方案-年级.${format}`);
    assert.throws(() => checkedScenarioExportResult({ ...saved(format), exportedActivityCount: 100 }, context, format),
      { code: "DESKTOP_SCENARIO_EXPORT_INVALID_RESPONSE" });
  }
  assert.equal(checkedScenarioExportResult({ ...saved(), exportedActivityCount: 0 }, { ...context, totalRows: 0 }, "xlsx").exportedActivityCount, 0);
  assert.equal(checkedScenarioExportResult({ ...saved(), fileName: "历史方案.XLSX" }, context, "xlsx").fileName, "历史方案.XLSX");
});

test("all immutable receipt fields plus selection and format must match the active export", () => {
  for (const [field, value] of [["projectId", other], ["sourceProjectRevision", "0"], ["sourcePayloadHash", "f".repeat(64)],
    ["scenarioId", other], ["scenarioRevision", "9007199254740996"], ["scenarioPayloadHash", "f".repeat(64)],
    ["timetableId", id], ["timetableRevision", "0"], ["timetablePayloadHash", "f".repeat(64)],
    ["originRunId", id], ["originArtifactHash", "f".repeat(64)], ["createdAt", "2026-09-12T12:00:01.000Z"]]) {
    const response = saved(); response.receipt[field] = value;
    assert.throws(() => checkedScenarioExportResult(response, context, "xlsx"), { code: "DESKTOP_SCENARIO_EXPORT_INVALID_RESPONSE" });
    assert.notEqual(scenarioExportContextKey({ ...context, receipt: response.receipt }), scenarioExportContextKey(context));
  }
  for (const response of [{ ...saved(), view: "student" }, { ...saved(), format: "csv" },
    ...["id", "code", "label"].map((field) => ({ ...saved(), selection: { ...selection, [field]: field === "id" ? other : "other" } }))]) {
    assert.throws(() => checkedScenarioExportResult(response, context, "xlsx"), { code: "DESKTOP_SCENARIO_EXPORT_INVALID_RESPONSE" });
  }
  assert.notEqual(scenarioExportContextKey(context), scenarioExportContextKey({ ...context, view: "student" }));
  assert.notEqual(scenarioExportContextKey(context), scenarioExportContextKey({ ...context, selection: { ...selection, id: other } }));
});

test("native cancellation is neutral and cannot also claim a saved receipt or error", () => {
  assert.deepEqual(checkedScenarioExportResult({ schemaVersion: 1, outcome: "cancelled" }, context, "xlsx"),
    { schemaVersion: 1, outcome: "cancelled" });
  for (const extra of [{ receipt }, { fileName: "file.xlsx" }, { exportedActivityCount: 134 }, { error: "failed" }]) {
    assert.throws(() => checkedScenarioExportResult({ schemaVersion: 1, outcome: "cancelled", ...extra }, context, "xlsx"),
      { code: "DESKTOP_SCENARIO_EXPORT_INVALID_RESPONSE" });
  }
});

test("invalid or failed export responses never become a success and useful native errors survive", () => {
  for (const value of [{}, null, { schemaVersion: 1, outcome: "failed" },
    ...[{ schemaVersion: 2 }, { receipt: { ...receipt, scenarioRevision: 0 } }, { sourceIsCurrent: "false" },
      { fileName: "/private/file.xlsx" }, { fileName: "dir\\file.xlsx" }, { fileName: "bad\nname.xlsx" }, { fileName: "" }, { fileName: "wrong.csv" },
      { exportedActivityCount: 134.5 }, { generatedAt: "invalid" }, { byteLength: 123 }, { byteLength: "0" },
      { byteLength: "18446744073709551616" }, { payloadHash: "bad" }, { payloadHashAlgorithm: "sha256" },
      { error: "failed" }, { runId: other }].map((changes) => ({ ...saved(), ...changes }))]) {
    assert.throws(() => checkedScenarioExportResult(value, context, "xlsx"), { code: "DESKTOP_SCENARIO_EXPORT_INVALID_RESPONSE" });
  }
  const error = commandError({ schemaVersion: 1, code: "APPLICATION_SCENARIO_REVISION_CONFLICT", message: "方案版本已变化。", details: null });
  assert.equal(error.code, "APPLICATION_SCENARIO_REVISION_CONFLICT");
  assert.equal(error.message, "方案版本已变化。");
  assert.notEqual(commandError({}).message, "{}");
});

test("one pending native dialog blocks duplicate exports and success cancellation or failure permits retry", () => {
  for (const outcome of ["saved", "cancelled", "failed"]) {
    const guard = createScenarioExportGuard();
    const token = guard.begin();
    assert.notEqual(token, null);
    assert.equal(guard.begin(), null, outcome);
    assert.equal(guard.accepts(token), true);
    guard.finish(token);
    assert.equal(guard.accepts(token), false);
    assert.notEqual(guard.begin(), null);
  }
});

test("selection changes or unmount isolate late successful and failed native responses", async () => {
  const guard = createScenarioExportGuard();
  const old = guard.begin();
  let resolveOld;
  const delayed = new Promise((resolve) => { resolveOld = resolve; });
  const messages = [];
  const oldCompletion = delayed.then(() => { if (guard.accepts(old)) messages.push("old saved"); })
    .finally(() => { if (guard.accepts(old)) guard.finish(old); });
  guard.invalidate();
  const current = guard.begin();
  resolveOld(); await oldCompletion;
  assert.deepEqual(messages, []);
  assert.equal(guard.accepts(current), true, "old completion must not clear the current busy guard");
  const lateFailure = Promise.reject(new Error("old failure")).catch(() => { if (guard.accepts(old)) messages.push("old failure"); });
  await lateFailure;
  assert.deepEqual(messages, []);
  guard.invalidate();
  assert.equal(guard.accepts(current), false, "unmounted selection must not accept a late response");
});

test("a browser cannot simulate a native save dialog or an exported timetable", async () => {
  await assert.rejects(exportScenarioTimetable(context, "xlsx"), { code: "DESKTOP_RUNTIME_REQUIRED" });
  await assert.rejects(exportScenarioTimetable(context, "csv"), { code: "DESKTOP_RUNTIME_REQUIRED" });
});
