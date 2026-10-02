import assert from "node:assert/strict";
import test from "node:test";
import { parseSolveJob, solveRequest, solveStatusLabel, startSolve } from "../src/solveApi.ts";

const project = { projectId: "77777777-1111-4111-8111-111111111111", revision: "9007199254740993", sectioningRequired: false };
const settings = { seed: "9007199254740993", execution: "reproducible", workerCount: 1,
  timeLimitSeconds: 30, minimumSize: 10, targetSize: 12, maximumSize: 16, candidateCount: 3 };
const running = { schemaVersion: 1, jobId: "job", projectId: project.projectId, revision: project.revision,
  state: "running", cancellationRequested: false, elapsedMillis: "1250", run: null, error: null };

test("saved project request preserves exact revision and seed without an executable or database path", () => {
  const value = solveRequest(project, settings);
  assert.equal(value.expectedRevision, project.revision);
  assert.equal(value.seed, settings.seed);
  assert.equal(value.autoSectioning, null);
  assert.equal(value.inputMode, "existing_sections");
  for (const key of ["worker", "workerPath", "databasePath", "assignments"]) assert.equal(key in value, false);
  const auto = solveRequest({ ...project, sectioningRequired: true }, settings);
  assert.deepEqual(auto.autoSectioning, { minimumSize: 10, targetSize: 12, maximumSize: 16, candidateCount: 3 });
});

test("desktop profiles preserve the exact seed and request with one or two allowed workers", () => {
  for (const [execution, workerCount] of [["reproducible", 1], ["fast", 1], ["fast", 2]]) {
    assert.deepEqual(solveRequest(project, { ...settings, execution, workerCount }), {
      schemaVersion: 1, projectId: project.projectId, expectedRevision: project.revision,
      inputMode: "existing_sections", seed: settings.seed, execution, workerCount,
      timeLimitSeconds: settings.timeLimitSeconds, autoSectioning: null,
    });
  }
});

test("invalid worker counts and reproducible multiworker settings are rejected before desktop IPC", async () => {
  for (const invalid of [
    ...[0, -1, 3, 16, 1.5, Number.NaN, Number.POSITIVE_INFINITY, Number.MAX_SAFE_INTEGER + 1, "2"].map((workerCount) =>
      ({ ...settings, execution: "fast", workerCount })),
    { ...settings, workerCount: 2 }, { ...settings, execution: "other" },
  ]) {
    assert.throws(() => solveRequest(project, invalid), { code: "DESKTOP_INVALID_SOLVE_LIMITS" });
    await assert.rejects(startSolve(project, invalid), { code: "DESKTOP_INVALID_SOLVE_LIMITS" },
      "the request must fail before the ordinary-browser desktop runtime check");
  }
});

test("job decoding rejects empty and contradictory success/error responses", () => {
  assert.deepEqual(parseSolveJob(running), running);
  for (const invalid of [{}, null, { ...running, state: "completed" }, { ...running, state: "failed" },
    { ...running, state: "stopped" }, { ...running, revision: 0 }, { ...running, elapsedMillis: "NaN" }]) {
    assert.throws(() => parseSolveJob(invalid), /结构不完整/u);
  }
});

test("timeout unknown infeasible and cancellation labels stay distinct", () => {
  const labels = ["Timeout", "Unknown", "ProvenInfeasible", "Cancelled"].map((status) => solveStatusLabel(status));
  assert.equal(new Set(labels).size, 4);
  assert.match(solveStatusLabel("Unknown", "CandidateBudgetExhausted"), /候选预算/u);
  assert.doesNotMatch(solveStatusLabel("Unknown", "CandidateBudgetExhausted"), /已证明/u);
});

test("starting solve in an ordinary browser never reports an invented job", async () => {
  await assert.rejects(startSolve(project, settings), { code: "DESKTOP_RUNTIME_REQUIRED" });
});
