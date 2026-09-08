import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { commandError, csvImportRequest, loadImportCatalog, selectCsvFiles, validateInspectionInput } from "../src/api.ts";

const config = {
  projectStableKey: "frontend-fixture", inputMode: "existing_sections", exactSubjectChoices: 3,
  periodsPerDay: 8, breakAfterPeriod: 4, minimumSize: 10, targetSize: 12, maximumSize: 16,
  seed: 20260904, candidateCount: 3,
};
const template = { dataset: "students", label: "学生", required: true,
  requiredHeaders: ["student_code", "name", "administrative_class_code"], optionalHeaders: [] };

test("real fixture bytes and explicit dataset survive arbitrary CSV filenames", async () => {
  const bytes = await readFile(new URL("../../fixtures/small/students.csv", import.meta.url));
  const file = new File([bytes], "高二学生名单.csv", { type: "text/csv" });
  const [selected] = selectCsvFiles([file], [template]);
  assert.equal(selected.dataset, "");
  const request = await csvImportRequest([{ ...selected, dataset: "students" }], config);
  assert.deepEqual(Buffer.from(request.datasets[0].bytes), bytes);
  assert.equal(request.datasets[0].dataset, "students");
  assert.equal(request.autoSectioning, null);
});

test("canonical filenames resolve, unknown filenames remain an explicit choice", () => {
  const selected = selectCsvFiles([new File([""], "STUDENTS.CSV"), new File([""], "学生.csv")], [template]);
  assert.deepEqual(selected.map((item) => item.dataset), ["students", ""]);
});

test("unselected, duplicate and unsupported files cannot reach an IPC request", async () => {
  const file = new File(["a,b\n1,2\n"], "students.csv");
  await assert.rejects(csvImportRequest([{ file, dataset: "" }], config), { code: "DESKTOP_DATASET_SELECTION_REQUIRED" });
  await assert.rejects(csvImportRequest([{ file, dataset: "students" }, { file, dataset: "students" }], config), { code: "DESKTOP_DUPLICATE_DATASET_SELECTION" });
  await assert.rejects(csvImportRequest([{ file: new File(["not CSV"], "students.xlsx"), dataset: "students" }], config), { code: "DESKTOP_UNSUPPORTED_FILE_FORMAT" });
});

test("all file bytes are present before a request can be returned", async () => {
  const first = new File(["student_code,name,administrative_class_code\n"], "students.csv");
  const second = new File(["teacher_code,name\n"], "teachers.csv");
  const request = await csvImportRequest([{ file: first, dataset: "students" }, { file: second, dataset: "teachers" }], config);
  assert.equal(request.datasets.length, 2);
  assert.equal(Buffer.from(request.datasets[1].bytes).toString(), await second.text());
});

test("opaque and JSON-encoded errors always have useful text and stable identity", () => {
  for (const error of [{}, "{}", null, undefined, "", new Error("")]) {
    assert.match(commandError(error).message, /本机操作未完成/u);
    assert.equal(commandError(error).details, null);
  }
  assert.equal(commandError(new Error("file read denied")).message, "file read denied");
  const payload = { schemaVersion: 1, code: "APPLICATION_SECTIONING_REQUIRED", message: "需要成班", details: { choiceCount: 72 } };
  assert.deepEqual(commandError(JSON.stringify(payload)), payload);
  assert.deepEqual(commandError(payload), payload);
  assert.match(commandError({ code: "SOME_CODE", message: "" }).message, /未返回说明/u);
});

test("numeric and memory limits are checked before byte allocation", () => {
  const file = new File([""], "students.csv");
  assert.equal(validateInspectionInput([file], { ...config, seed: Number.MAX_SAFE_INTEGER + 1, inputMode: "auto_sectioning" }).code, "DESKTOP_INVALID_NUMERIC_INPUT");
  assert.equal(validateInspectionInput([file], { ...config, breakAfterPeriod: 8 }).code, "DESKTOP_INVALID_BREAK_POSITION");
  const oversized = new File([new Uint8Array(32 * 1024 * 1024 + 1)], "students.csv");
  assert.equal(validateInspectionInput([oversized], config).code, "DESKTOP_IMPORT_TOO_LARGE");
});

test("browser preview without a native runtime fails with actionable instructions", async () => {
  await assert.rejects(loadImportCatalog(), { code: "DESKTOP_RUNTIME_REQUIRED", message: /Tauri/u });
});

test("CSV and XLSX table assignments cannot duplicate or omit a dataset", async () => {
  const file = new File([""], "teachers.csv");
  const workbook = { file: new File([""], "学校.xlsx"), sheetMappings: [{ sheetName: "教师", dataset: "teachers" }] };
  await assert.rejects(csvImportRequest([{ file, dataset: "teachers" }, workbook], config), { code: "DESKTOP_DUPLICATE_DATASET_SELECTION" });
  await assert.rejects(csvImportRequest([{ ...workbook, sheetMappings: [{ sheetName: "教师", dataset: "" }] }], config), { code: "DESKTOP_DATASET_SELECTION_REQUIRED" });
  await assert.rejects(csvImportRequest([{ ...workbook, sheetMappings: [] }], config), { code: "DESKTOP_DATASET_SELECTION_REQUIRED" });
});
